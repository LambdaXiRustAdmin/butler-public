//! Simple state for incremental fat graph (save/read previous as context).
//! Explicit, minimal alloc where possible. Uses serde for JSON.

use super::types::FatGraph;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct HarvestState {
    pub current: FatGraph,
    pub path: PathBuf,
}

impl HarvestState {
    pub fn load_or_new(path: &Path, query: &str) -> Self {
        if path.exists() {
            if let Ok(data) = std::fs::read_to_string(path) {
                if let Ok(mut g) = serde_json::from_str::<FatGraph>(&data) {
                    g.query = query.to_string(); // always use the query from this run (even on resume)
                    return Self {
                        current: g,
                        path: path.to_path_buf(),
                    };
                }
            }
        }
        Self {
            current: FatGraph {
                query: query.to_string(),
                ..Default::default()
            },
            path: path.to_path_buf(),
        }
    }

    pub fn save(&self) {
        if let Ok(data) = serde_json::to_string_pretty(&self.current) {
            let _ = std::fs::write(&self.path, data);
        }
    }
}
