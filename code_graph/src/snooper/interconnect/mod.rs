//! Interconnect steward — **protocol grammar** (rules / patterns), not Tree-sitter.
//!
//! Same-lang CALL edges stay on [`CodeGraph::edges`]. Cross-stack glue is stored as
//! **typed bridges** ([`BridgeKind`]) so Trace never confuses Export/Ipc with CALL.
//!
//! | Module | Role |
//! |--------|------|
//! | [`kinds`] | `BridgeKind` (Export / Ipc / Twin) |
//! | [`presence`] | Language presence gates |
//! | [`passes`] | Post-edge map/reduce orchestration |
//! | [`detect`] | Peer-lang schedule (detect → pull) |
//!
//! Discovery helpers remain in `lang/*/ffi` and `ipc_engine`; this module **owns
//! linking + kinds + schedule**. See `plans/interconnect-steward.md` Track P.

mod detect;
mod kinds;
mod passes;
mod presence;

pub use detect::{detect_peer_schedule, dual_stack_parse_boost, PeerSchedule};
pub use kinds::BridgeKind;
pub use passes::{
    apply_post_edge_maps, map_without_decl_def, polyglot_ac_enabled, run_without_decl_def,
    PostEdgeMaps,
};
pub use presence::{
    graph_has_c_family, graph_has_python, graph_has_rust, graph_has_ts_js, path_lang_tag,
    LangPresence,
};
