//! Server module re-exports and thin Axum surface for the Butler HTTP/MCP server.

pub mod analysis;
pub mod auth;
pub mod build_status;
pub mod context_engine;
pub mod dashboard;
pub mod setup_page;
pub mod discovery;
pub mod dto;
pub mod filters;
pub mod handlers;
pub mod logv;
pub mod request_ring;
pub mod monorepo_scope;
pub mod mode_intent;
pub mod neural;
pub mod orchestrate;
pub mod paths;
pub mod query_cache;
pub mod render;
pub mod scope;
pub mod trace_memo;
pub mod trace_pack;
pub mod score_audit;
pub mod sniffer;
pub mod state;

/// Verbose-only println (no-op unless `BUTLER_VERBOSE=1`).
/// Available to all `server::*` modules via `use crate::vprintln`.
#[macro_export]
macro_rules! vprintln {
    ($($arg:tt)*) => {{
        if $crate::server::logv::verbose() {
            ::std::println!($($arg)*);
        }
    }};
}
