//! Typed interconnect edge families (Track P.1).

use serde::{Deserialize, Serialize};

/// How two languages / stacks connect. Never stored as unlabeled CALL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BridgeKind {
    /// Structural export table: `#[pyfunction]`, pybind `m.def`, … → Python import/call.
    Export,
    /// Config-driven IPC (e.g. Tauri `invoke("…")` → `#[command]`).
    Ipc,
    /// Weak name-coincidence / twin (opt-in AC). Lowest confidence.
    Twin,
}

impl BridgeKind {
    /// Stable label for Trace `relation` / agent-facing structured rows.
    pub fn as_relation_label(self) -> &'static str {
        match self {
            Self::Export => "export",
            Self::Ipc => "ipc",
            Self::Twin => "twin",
        }
    }

    /// Prefer higher-confidence kind when two bridges collide (same endpoints).
    /// Export (structural FFI) > Ipc (config/schema) > Twin (weak name-coincidence).
    pub fn rank(self) -> u8 {
        match self {
            Self::Export => 3,
            Self::Ipc => 2,
            Self::Twin => 1,
        }
    }
}

impl std::fmt::Display for BridgeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_relation_label())
    }
}
