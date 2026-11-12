//! Application-facing scanner. No global signal handlers, terminal output, or
//! process termination. Run `scan` on a worker thread and poll `ScanControl`.
pub use crate::node::EntryKind;
use crate::{
    dir_walker::{WalkData, walk_raw},
    progress::{ORDERING, PAtomicInfo, RuntimeErrors},
};
use std::{
    collections::{HashMap, HashSet},
    io,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

pub type NodeId = usize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeMode {
    Allocated,
    Apparent,
}

#[derive(Debug, Clone)]
pub struct ScanOptions {
    pub root: PathBuf,
    pub size_mode: SizeMode,
    pub same_filesystem: bool,
    pub include_hidden: bool,
    pub threads: usize,
}
impl ScanOptions {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            size_mode: SizeMode::Allocated,
            same_filesystem: true,
            include_hidden: true,
            threads: 4,
        }
    }
}

#[derive(Default, Clone)]
pub struct ScanControl {
    cancelled: Arc<AtomicBool>,
    progress: Arc<PAtomicInfo>,
}
impl ScanControl {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
    /// Bytes are provisional until hard links have been accounted for.
    pub fn progress(&self) -> ScanProgress {
        ScanProgress {
            files: self.progress.num_files.load(ORDERING),
            bytes: self.progress.total_file_size.load(ORDERING),
        }
    }
}
#[derive(Debug, Default, Clone, Copy)]
pub struct ScanProgress {
    pub files: usize,
    pub bytes: u64,
}

#[derive(Debug, Clone)]
pub struct ScanIssue {
    pub path: PathBuf,
    pub kind: io::ErrorKind,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct ScanNode {
    pub path: PathBuf,
    pub kind: EntryKind,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    /// Known total; `incomplete` indicates that part of the subtree is unknown.
    pub bytes: u64,
    pub own_bytes: Option<u64>,
    pub incomplete: bool,
    /// Allocated storage is attributed once, to this earlier entry.
    pub hardlink_to: Option<NodeId>,
}

#[derive(Debug)]
pub struct ScanTree {
    pub nodes: Vec<ScanNode>,
    pub issues: Vec<ScanIssue>,
    pub size_mode: SizeMode,
}
impl ScanTree {
    pub fn root(&self) -> &ScanNode {
        &self.nodes[0]
    }
}

#[derive(Debug)]
pub enum ScanOutcome {
    Complete(ScanTree),
    Cancelled,
}

/// A single-use scan. A cancelled result is never reported as complete.
/// Links below the selected root are not followed. A selected root is resolved
/// once so callers may deliberately select an alias to a directory.
pub fn scan(options: ScanOptions, control: &ScanControl) -> io::Result<ScanOutcome> {
    if control.is_cancelled() {
        return Ok(ScanOutcome::Cancelled);
    }
    if options.threads == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "thread count must be positive",
        ));
    }
    let root = std::fs::canonicalize(&options.root)?;
    let metadata = std::fs::symlink_metadata(&root)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "scan root must be a directory",
        ));
    }
    let apparent = options.size_mode == SizeMode::Apparent;
    let root_info = crate::platform::scan_metadata(&root, &metadata, apparent)?;
    let allowed_filesystems = if options.same_filesystem {
        let (_, device) = root_info.1.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "filesystem identity unavailable",
            )
        })?;
        HashSet::from([device])
    } else {
        HashSet::new()
    };
    let errors = Arc::new(Mutex::new(RuntimeErrors::default()));
    let walk = WalkData {
        ignore_directories: HashSet::new(),
        filter_regex: &[],
        invert_filter_regex: &[],
        allowed_filesystems,
        filter_modified_time: None,
        filter_accessed_time: None,
        filter_changed_time: None,
        use_apparent_size: apparent,
        by_filecount: false,
        by_filetime: &None,
        ignore_hidden: !options.include_hidden,
        follow_links: false,
        progress_data: control.progress.clone(),
        errors: errors.clone(),
        has_any_filter: options.same_filesystem,
        scan_mode: true,
        cancellation: Some(&control.cancelled),
    };
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(options.threads)
        .build()
        .map_err(io::Error::other)?;
    let roots = pool.install(|| walk_raw(HashSet::from([root]), &walk));
    let mut issues = std::mem::take(&mut errors.lock().unwrap().scan_issues);
    issues.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.message.cmp(&b.message)));
    issues.dedup_by(|a, b| a.path == b.path && a.kind == b.kind && a.message == b.message);
    // Flatten iteratively, including on cancellation, to avoid recursive drops
    // of deeply nested directory trees.
    let mut pending: Vec<_> = roots.into_iter().map(|node| (node, None)).collect();
    let mut nodes: Vec<ScanNode> = Vec::new();
    let mut identities = HashMap::new();
    while let Some((mut node, parent)) = pending.pop() {
        let id = nodes.len();
        let hardlink_to = if !apparent && node.kind == EntryKind::File {
            node.inode_device
                .and_then(|key| match identities.entry(key) {
                    std::collections::hash_map::Entry::Occupied(entry) => Some(*entry.get()),
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        entry.insert(id);
                        None
                    }
                })
        } else {
            None
        };
        let own_bytes =
            node.size_known
                .then_some(if hardlink_to.is_some() { 0 } else { node.size });
        node.children.sort_by(|a, b| b.name.cmp(&a.name));
        pending.extend(node.children.into_iter().map(|child| (child, Some(id))));
        nodes.push(ScanNode {
            path: node.name,
            kind: node.kind,
            parent,
            children: Vec::new(),
            bytes: own_bytes.unwrap_or(0),
            own_bytes,
            incomplete: !node.size_known,
            hardlink_to,
        });
        if let Some(parent) = parent {
            nodes[parent].children.push(id);
        }
    }
    if control.is_cancelled() {
        return Ok(ScanOutcome::Cancelled);
    }
    if nodes.is_empty() {
        return Err(io::Error::other("scanner returned no root"));
    }
    let by_path: HashMap<_, _> = nodes
        .iter()
        .enumerate()
        .map(|(id, node)| (node.path.clone(), id))
        .collect();
    for issue in &issues {
        // An entry can disappear before its Node exists; mark its closest
        // surviving ancestor as incomplete in that case.
        for ancestor in issue.path.ancestors() {
            if let Some(&id) = by_path.get(ancestor) {
                nodes[id].incomplete = true;
                break;
            }
        }
    }
    drop(by_path);
    for id in (1..nodes.len()).rev() {
        let parent = nodes[id].parent.expect("non-root node has parent");
        nodes[parent].bytes = nodes[parent]
            .bytes
            .checked_add(nodes[id].bytes)
            .ok_or_else(|| io::Error::other("directory size overflow"))?;
        nodes[parent].incomplete |= nodes[id].incomplete;
    }
    for id in 0..nodes.len() {
        let mut children = std::mem::take(&mut nodes[id].children);
        children.sort_by(|&a, &b| {
            nodes[b]
                .bytes
                .cmp(&nodes[a].bytes)
                .then_with(|| nodes[a].path.cmp(&nodes[b].path))
        });
        nodes[id].children = children;
    }
    if control.is_cancelled() {
        return Ok(ScanOutcome::Cancelled);
    }
    Ok(ScanOutcome::Complete(ScanTree {
        nodes,
        issues,
        size_mode: options.size_mode,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn complete(options: ScanOptions) -> ScanTree {
        match scan(options, &ScanControl::default()).unwrap() {
            ScanOutcome::Complete(tree) => tree,
            ScanOutcome::Cancelled => panic!("unexpected cancellation"),
        }
    }
    #[test]
    fn hierarchy_totals_include_own_directory_bytes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("empty")).unwrap();
        std::fs::create_dir(dir.path().join("nested")).unwrap();
        std::fs::write(dir.path().join("nested/file"), vec![0; 1234]).unwrap();
        let mut options = ScanOptions::new(dir.path());
        options.size_mode = SizeMode::Apparent;
        let tree = complete(options);
        assert_eq!(tree.nodes.len(), 4);
        assert!(tree.issues.is_empty());
        assert_eq!(
            tree.root().bytes,
            tree.nodes.iter().map(|n| n.own_bytes.unwrap()).sum::<u64>()
        );
        assert_eq!(
            tree.nodes
                .iter()
                .find(|n| n.path.ends_with("file"))
                .unwrap()
                .bytes,
            1234
        );
        assert_eq!(
            tree.nodes
                .iter()
                .find(|n| n.path.ends_with("empty"))
                .unwrap()
                .kind,
            EntryKind::Directory
        );
    }
    #[test]
    fn cancellation_is_not_an_empty_success() {
        let control = ScanControl::default();
        control.cancel();
        assert!(matches!(
            scan(ScanOptions::new("does-not-exist"), &control).unwrap(),
            ScanOutcome::Cancelled
        ));
    }
    #[test]
    fn invalid_roots_fail() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            scan(
                ScanOptions::new(dir.path().join("missing")),
                &ScanControl::default()
            )
            .is_err()
        );
        std::fs::write(dir.path().join("file"), b"hi").unwrap();
        assert!(
            scan(
                ScanOptions::new(dir.path().join("file")),
                &ScanControl::default()
            )
            .is_err()
        );
    }
    #[test]
    fn deep_trees_and_hidden_entries() {
        let dir = tempfile::tempdir().unwrap();
        let mut path = dir.path().to_path_buf();
        for _ in 0..100 {
            path.push("d");
            std::fs::create_dir(&path).unwrap();
        }
        std::fs::write(path.join("leaf"), b"data").unwrap();
        std::fs::write(dir.path().join(".hidden"), b"hidden").unwrap();
        let mut options = ScanOptions::new(dir.path());
        options.include_hidden = false;
        let tree = complete(options);
        assert_eq!(tree.nodes.len(), 102);
        assert!(tree.nodes.iter().any(|n| n.path.ends_with("leaf")));
        assert!(!tree.nodes.iter().any(|n| n.path.ends_with(".hidden")));
    }
    // macOS/APFS rejects this fixture with EILSEQ. Linux filesystems permit
    // arbitrary filename bytes, so exercise that platform's path semantics.
    #[cfg(target_os = "linux")]
    #[test]
    fn non_utf8_paths_remain_lossless() {
        use std::os::unix::ffi::OsStringExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join(std::ffi::OsString::from_vec(vec![b'a', 0xff]));
        std::fs::write(&path, b"data").unwrap();
        let tree = complete(ScanOptions::new(dir.path()));
        assert!(
            tree.nodes
                .iter()
                .any(|n| n.path.file_name() == path.file_name())
        );
        assert!(tree.issues.is_empty());
    }
    #[cfg(unix)]
    #[test]
    fn unreadable_directory_marks_ancestors_incomplete() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let blocked = dir.path().join("blocked");
        std::fs::create_dir(&blocked).unwrap();
        std::fs::write(blocked.join("secret"), b"data").unwrap();
        std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0)).unwrap();
        let denied = std::fs::read_dir(&blocked).is_err();
        let result = scan(ScanOptions::new(dir.path()), &ScanControl::default());
        // Always restore fixture permissions, even if the assertions below fail.
        std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o700)).unwrap();
        let ScanOutcome::Complete(tree) = result.unwrap() else {
            panic!("unexpected cancellation")
        };
        if denied {
            assert!(tree.root().incomplete);
            assert!(
                tree.issues
                    .iter()
                    .any(|e| e.kind == io::ErrorKind::PermissionDenied
                        && e.path.ends_with("blocked"))
            );
            assert!(
                tree.nodes
                    .iter()
                    .find(|n| n.path.ends_with("blocked"))
                    .unwrap()
                    .incomplete
            );
        } else {
            eprintln!("permission-denied assertion skipped: this user can bypass mode bits");
        }
    }
    #[cfg(unix)]
    #[test]
    fn hardlinks_remain_visible_and_symlink_cycles_are_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a"), vec![1; 8192]).unwrap();
        std::fs::hard_link(dir.path().join("a"), dir.path().join("b")).unwrap();
        std::os::unix::fs::symlink(dir.path(), dir.path().join("loop")).unwrap();
        let tree = complete(ScanOptions::new(dir.path()));
        assert_eq!(tree.nodes.len(), 4);
        assert_eq!(
            tree.nodes
                .iter()
                .filter(|n| n.hardlink_to.is_some())
                .count(),
            1
        );
        assert_eq!(
            tree.nodes
                .iter()
                .find(|n| n.path.ends_with("loop"))
                .unwrap()
                .kind,
            EntryKind::Symlink
        );
        let mut options = ScanOptions::new(dir.path());
        options.size_mode = SizeMode::Apparent;
        let apparent = complete(options);
        assert_eq!(
            apparent
                .nodes
                .iter()
                .filter(|n| n.kind == EntryKind::File)
                .map(|n| n.bytes)
                .sum::<u64>(),
            16384
        );
    }
    #[cfg(unix)]
    #[test]
    fn sparse_files_distinguish_allocated_from_apparent() {
        let dir = tempfile::tempdir().unwrap();
        let file = std::fs::File::create(dir.path().join("sparse")).unwrap();
        file.set_len(64 * 1024 * 1024).unwrap();
        let allocated = complete(ScanOptions::new(dir.path()));
        let mut options = ScanOptions::new(dir.path());
        options.size_mode = SizeMode::Apparent;
        let apparent = complete(options);
        assert!(
            allocated
                .nodes
                .iter()
                .find(|n| n.kind == EntryKind::File)
                .unwrap()
                .bytes
                < apparent
                    .nodes
                    .iter()
                    .find(|n| n.kind == EntryKind::File)
                    .unwrap()
                    .bytes
        );
    }
}
