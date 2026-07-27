//! Time `load_shards` (+ optional trust finalize path) for a project root.
//!
//! ```bash
//! cargo run -p code_graph --example bench_load_shards --release -- /path/to/repo
//! ```

use std::path::Path;
use std::time::Instant;

fn main() {
    let root = std::env::args()
        .nth(1)
        .unwrap_or_else(|| ".".to_string());
    let root = Path::new(&root);
    println!("bench load_shards: {}", root.display());
    let t0 = Instant::now();
    match code_graph::snooper::scanner::shards::load_shards(root) {
        Ok(Some(g)) => {
            println!(
                "ok nodes={} edges={} files={} complete={} in {:.2?}",
                g.nodes.len(),
                g.edges.len(),
                g.file_hashes.len(),
                g.is_edge_build_complete(),
                t0.elapsed()
            );
        }
        Ok(None) => {
            println!("no shards ({:.2?})", t0.elapsed());
            std::process::exit(2);
        }
        Err(e) => {
            eprintln!("err: {e} ({:.2?})", t0.elapsed());
            std::process::exit(1);
        }
    }
}
