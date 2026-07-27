//! Thin entry point after Strangler Fig extraction of MCP logic into cli::mcp.
//! The heavy protocol, handlers, stdio loop, and HTTP server now live in the library module.

fn main() {
    // Delegate to the library implementation (which has the tokio main).
    if let Err(e) = cli::mcp::run_main() {
        eprintln!("MCP bridge error: {}", e);
        std::process::exit(1);
    }
}
