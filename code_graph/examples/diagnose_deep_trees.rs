//! Diagnostic tool to test whether Tree-sitter + our visitor can handle very large/deep projects
//! like rust-lang/rust without stack overflow.
//!
//! Usage:
//!     cargo run --release --example diagnose_deep_trees -- /projects/rust-lang
//!
//! This runs the Tree-sitter parse + visit_node collection on a dedicated thread
//! with a very large stack (64 MiB) so we can rule out stack size as the cause.

use std::env;
use std::path::Path;
use std::time::Instant;
use walkdir::WalkDir;

use tree_sitter::Parser;

fn main() {
    let args: Vec<String> = env::args().collect();
    let root = if args.len() > 1 {
        Path::new(&args[1]).to_path_buf()
    } else {
        std::env::current_dir().expect("failed to get current dir")
    };

    println!("=== Tree-sitter Deep Tree Diagnostic ===");
    println!("Target: {}", root.display());
    println!("Starting on a dedicated thread with 64 MiB stack...\n");

    let handle = std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024) // Very large stack for diagnosis
        .name("diagnose-large-stack".into())
        .spawn(move || run_diagnostic(&root))
        .expect("failed to spawn diagnostic thread");

    handle.join().expect("diagnostic thread panicked");
}

fn run_diagnostic(root: &Path) {
    println!("🔎 Scanning directory: {}", root.display());

    let skip_dirs = [".git", "target", "node_modules", ".butler"];

    let mut file_count = 0;
    let mut total_nodes = 0usize;
    let mut interesting_blocks = 0usize;
    let start = Instant::now();

    for entry in WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| {
            if e.file_type().is_dir() {
                let name = e.file_name().to_string_lossy();
                !skip_dirs.iter().any(|d| name == *d)
            } else {
                true
            }
        })
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map_or(false, |ext| ext == "rs"))
    {
        let path = entry.path();
        file_count += 1;

        if file_count % 500 == 0 {
            println!(
                "   [{}] Still alive... last file: {}",
                file_count,
                path.display()
            );
        }

        if let Ok(source) = std::fs::read_to_string(path) {
            match parse_and_count_nodes(&source) {
                Ok((node_count, block_count)) => {
                    total_nodes += node_count;
                    interesting_blocks += block_count;
                }
                Err(e) => {
                    eprintln!("⚠️  Parse error in {}: {}", path.display(), e);
                }
            }
        }

        // Safety valve - stop after a while if needed during diagnosis
        if file_count > 50_000 {
            println!("(Reached 50k files, stopping early for this diagnostic run)");
            break;
        }
    }

    let elapsed = start.elapsed();
    println!("\n✅ Diagnostic completed without stack overflow!");
    println!("   Files processed: {}", file_count);
    println!("   Total AST nodes: {}", total_nodes);
    println!("   Interesting blocks found: {}", interesting_blocks);
    println!("   Time taken: {:.2?}", elapsed);
    println!("\nIf this completed successfully, the stack overflow in the full Butler server");
    println!("is likely coming from later phases (edge building, detect_cycles, etc.) or");
    println!("from very specific files that weren't reached yet.");
}

/// Parses a single Rust file and returns (total_node_count, interesting_block_count)
fn parse_and_count_nodes(source: &str) -> Result<(usize, usize), String> {
    let mut parser = Parser::new();
    let language = tree_sitter_rust::LANGUAGE;
    parser
        .set_language(&language.into())
        .map_err(|e| e.to_string())?;

    let tree = parser.parse(source, None).ok_or("parse failed")?;
    let root = tree.root_node();

    let mut node_count = 0;
    let mut interesting = 0;

    // Use TreeCursor for efficient traversal (same spirit as the real visitor)
    let cursor = root.walk();
    let mut stack = vec![cursor.node()];

    const INTERESTING: &[&str] = &[
        "function_item",
        "struct_item",
        "enum_item",
        "union_item",
        "trait_item",
        "impl_item",
        "mod_item",
        "type_item",
        "const_item",
        "static_item",
    ];

    while let Some(node) = stack.pop() {
        node_count += 1;

        if INTERESTING.contains(&node.kind()) {
            interesting += 1;
        }

        // Collect children using TreeCursor (non-recursive)
        let mut child_cursor = node.walk();
        if child_cursor.goto_first_child() {
            let mut children = vec![child_cursor.node()];
            while child_cursor.goto_next_sibling() {
                children.push(child_cursor.node());
            }
            // Push in reverse so we process left-to-right when popping
            for child in children.into_iter().rev() {
                stack.push(child);
            }
        }
    }

    Ok((node_count, interesting))
}
