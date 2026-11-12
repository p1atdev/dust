//! cargo run --example scan -- DIRECTORY
use du_dust::{ScanControl, ScanOptions, ScanOutcome};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os().nth(1).ok_or("Usage: scan DIRECTORY")?;
    let started = std::time::Instant::now();
    match du_dust::scan(ScanOptions::new(path), &ScanControl::default())? {
        ScanOutcome::Complete(tree) => println!(
            "{} entries, {} allocated bytes, {} issues, {:.3}s",
            tree.nodes.len(),
            tree.root().bytes,
            tree.issues.len(),
            started.elapsed().as_secs_f64()
        ),
        ScanOutcome::Cancelled => println!("Cancelled"),
    }
    Ok(())
}
