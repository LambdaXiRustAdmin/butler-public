// code_graph/examples/test_scanner.rs
//
// Scan a fixture tree in-memory and persist cache only under a **temp** root —
// never write `.butler` into the source tree (cwd was a historical pollution source).
use code_graph::{load_graph, save_graph, scan_workspace};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    let config_skips: Vec<String> = vec!["target/".into(), "node_modules/".into(), ".git/".into()];

    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fixture = manifest.join("examples/test_data");
    if !fixture.is_dir() {
        eprintln!("missing fixture {}", fixture.display());
        std::process::exit(2);
    }

    // Persist only to temp (policy also refuses fixture roots if someone passes them).
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let cache_root = std::env::temp_dir().join(format!("butler_ex_scan_{stamp}"));
    let _ = std::fs::create_dir_all(&cache_root);

    println!("fixture (read)  {}", fixture.display());
    println!("cache root      {}", cache_root.display());

    let graph = scan_workspace(&fixture, None, &config_skips);
    println!("Scanned fixture: {} blocks", graph.nodes.len());

    if let Err(e) = save_graph(&graph, &cache_root) {
        eprintln!("⚠️ Cache save failed: {e}");
    } else {
        println!(
            "💾 Graph cached to {}/.butler/cache/graph.bin",
            cache_root.display()
        );
    }

    let loaded = load_graph(&cache_root, None, &config_skips);
    println!("\n✅ Scan complete!");
    println!(" Blocks built : {}", graph.nodes.len());
    println!(" Blocks from cache : {}", loaded.nodes.len());

    if let Some(first) = graph.nodes.values().next() {
        println!(" First block ID : {}", first.id.as_str());
        println!(" Lines : {}", first.end_line);
    }

    println!("\n=== ACTUAL GRAPH CONTENTS (summary) ===");
    let mut kind_count: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for block in graph.nodes.values() {
        *kind_count.entry(block.kind.clone()).or_insert(0) += 1;
    }
    println!("Block kinds:");
    for (kind, count) in kind_count {
        println!(" {:<15} → {} blocks", kind, count);
    }

    println!("\nSample blocks (first 8):");
    for (i, (id, block)) in graph.nodes.iter().take(8).enumerate() {
        println!(
            " {}. {} | {} | lines {}-{} | ID: {}",
            i + 1,
            block.file.display(),
            block.kind,
            block.start_line,
            block.end_line,
            id.as_str()
        );
    }

    println!(
        "\nEdges in graph: {}",
        graph.edges.values().map(|v| v.len()).sum::<usize>()
    );

    let cycles: usize = graph.nodes.values().filter(|b| b.has_cycle).count();
    println!("Blocks with cycles: {cycles}");
    println!("\nThe snooper spine is now working on real code! 🔥");

    let _ = std::fs::remove_dir_all(&cache_root);
}
