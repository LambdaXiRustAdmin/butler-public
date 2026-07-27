// code_graph/examples/test_languages.rs
use code_graph::{parse_file, CodeGraph};
use std::path::PathBuf;

fn main() {
    let graph = CodeGraph::new();

    // Robust path using CARGO_MANIFEST_DIR (crate root)
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let test_data = manifest_dir.join("examples/test_data");

    let test_files = [
        test_data.join("rust_example.rs"),
        test_data.join("python_example.py"),
    ];

    for file in test_files {
        if let Ok(source) = std::fs::read_to_string(&file) {
            let parsed = parse_file(&file, &source).expect("parse failed");
            let _blocks = parsed.blocks;
        //        println!("Parsed {} → {} blocks", file.display(), _blocks.len());
        } else {
            println!("Warning: Test file {} not found.", file.display());
        }
    }

    println!("\n✅ Multi-language test complete!");
    println!("Total blocks in graph: {}", graph.nodes.len());
    println!("Supported extensions: .rs, .py (add more in parser.rs)");
}
