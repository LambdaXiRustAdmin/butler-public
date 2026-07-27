// code_graph/examples/test_context.rs
//
// Build context from the in-tree fixture without persisting `.butler` (no save / no CWD root).
use code_graph::{get_context, scan_workspace, ContextOptions};
use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fixture = manifest.join("examples/test_data");
    if !fixture.is_dir() {
        eprintln!("missing fixture {}", fixture.display());
        std::process::exit(2);
    }

    // RAM-only: scan_workspace does not write .butler; avoid load_graph(".") which used to.
    let graph = scan_workspace(&fixture, None, &[]);

    let sample = fixture.join("rust_example.rs");
    let sample_rel = sample
        .strip_prefix(&fixture)
        .map(|p| p.to_path_buf())
        .unwrap_or(sample);

    let output = get_context(
        &graph,
        sample_rel.to_string_lossy().as_ref(),
        1,
        40,
        ContextOptions {
            depth: 2,
            max_tokens: 4000,
            compress_tests: false,
            format: Default::default(),
            ..Default::default()
        },
        "mutex",
    );

    println!("{output}");
}
