# Application scanner API

This fork adds a reusable scanner for Dusty Disk. The base revision is
`079c55266917387e416e0f871c97c06da52df12d`. The upstream CLI and its Apache-2.0
license are retained. CLI implementation modules are exported only to share
code with the binary; they are not a stable public API.

```rust,no_run
use du_dust::{scan, ScanControl, ScanOptions, ScanOutcome};

let control = ScanControl::default();
match scan(ScanOptions::new("."), &control)? {
    ScanOutcome::Complete(tree) => println!("{} bytes", tree.root().bytes),
    ScanOutcome::Cancelled => println!("cancelled"),
}
# Ok::<(), std::io::Error>(())
```

Call `scan` on a worker thread. A clone of `ScanControl` provides progress and
cooperative cancellation. It does not install signal handlers, print to the
terminal, or terminate the process. Progress bytes are provisional until hard
links have been accounted for. Cancelled work is not returned as a complete
tree; an in-flight OS operation cannot be forcibly interrupted.

`ScanTree` is a flat arena with parent/child IDs and original `PathBuf` paths.
Children are sorted by descending size with deterministic path tie breaking.
No depth/size pruning occurs in the scanner. The GUI owns visualization limits.
Unknown metadata is `own_bytes: None`, not an estimated size. `bytes` is the
known subtotal; `incomplete` propagates to ancestors and `issues` includes paths
and IO errors. Root setup failures return `Err`.

The selected root is resolved once; descendant symlinks are not followed.
Default scans include hidden entries and stay on one filesystem. Allocated mode
uses Unix block allocation or Windows `FileStandardInfo`; failures are reported
without substituting logical length. Allocated hard links are counted once,
while all paths remain available. Apparent mode counts each path. APFS clone
sharing, snapshots, and purgeable space are outside this API's scope.

Changes are concentrated in `scanner.rs`, a raw-tree entry point in the existing
parallel walker, optional cancellation/error reporting, and strict platform
metadata access. `cargo run --example scan -- DIRECTORY` exercises the API.

```sh
cargo test --locked
cargo check --locked --lib --target x86_64-pc-windows-msvc
cargo check --locked --lib --target x86_64-unknown-linux-gnu
```
