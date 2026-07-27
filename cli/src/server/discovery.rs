//! Workspace resolution, project discovery, and marker detection logic for the Butler server.
//!
//! Extracted from cli/src/bin/server.rs as part of Strangler Fig refactoring.
//! Powers list_projects, resolve_project, and the discovery fallback for marker-less roots.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use axum::{response::IntoResponse, Json};
use strsim::normalized_levenshtein;
use tokio::sync::RwLock;

use crate::server::dto::ProjectsResponse;

/// Shared registry for resolved project names (case-sensitive + lowercase keys).
static PROJECT_REGISTRY: OnceLock<RwLock<HashMap<String, String>>> = OnceLock::new();

/// Resolves a project name to its absolute filesystem path.
///
/// Two-tier: exact registry hit, else scan BUTLER_PROJECTS_ROOT (Cargo.toml dirs only)
/// + fuzzy (substring for <5 chars, else normalized Levenshtein >0.4).
pub fn resolve_project(project_name: &str) -> String {
    let registry = PROJECT_REGISTRY.get_or_init(|| RwLock::new(HashMap::new()));
    if let Some(path) = registry.blocking_read().get(project_name) {
        return path.clone();
    }

    let candidates = project_dirs();
    let lower_query = project_name.to_lowercase();

    let best_match = if project_name.len() < 5 {
        candidates.iter().find_map(|(name, full)| {
            name.to_lowercase()
                .contains(&lower_query)
                .then_some(full.clone())
        })
    } else {
        None
    }
    .or_else(|| {
        candidates
            .iter()
            .map(|(name, full)| {
                (
                    normalized_levenshtein(&lower_query, &name.to_lowercase()),
                    full,
                )
            })
            .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
            .filter(|(score, _)| *score > 0.4)
            .map(|(_, full)| full.clone())
    })
    .unwrap_or_else(|| project_name.to_string());

    let mut w = registry.blocking_write();
    for (name, full) in &candidates {
        let lower = name.to_lowercase();
        w.insert(lower, full.clone());
        w.insert(name.clone(), full.clone());
    }
    w.insert(project_name.to_string(), best_match.clone());
    best_match
}

/// Handles `GET /projects` — lists all discoverable projects under BUTLER_PROJECTS_ROOT.
pub async fn list_projects() -> impl IntoResponse {
    let projects: Vec<String> = project_dirs().into_iter().map(|(name, _)| name).collect();
    let count = projects.len();
    Json(ProjectsResponse { projects, count })
}

/// Marker files used to identify project roots (Cargo workspaces, Python, Node, etc.).
/// Expanded for classic C/C++ projects (e.g. SQLite uses Makefile.in/configure, no CMake).
pub const PROJECT_MARKERS: &[&str] = &[
    "Cargo.toml",
    "pyproject.toml",
    "package.json",
    "setup.py",
    "go.mod",
    "requirements.txt",
    "pom.xml",
    "build.gradle",
    "Makefile",
    "Makefile.in",
    "configure",
    "configure.ac",
    "meson.build",
    "build.ninja",
    "vcpkg.json",
    "conanfile.txt",
    "CMakeLists.txt",
];

pub fn has_any_marker(dir: &Path) -> bool {
    PROJECT_MARKERS.iter().any(|m| dir.join(m).exists())
}

pub fn guess_lang_from_markers(dir: &Path) -> &'static str {
    if dir.join("Cargo.toml").exists() {
        "Rust"
    } else if ["pyproject.toml", "setup.py", "requirements.txt"]
        .iter()
        .any(|m| dir.join(m).exists())
    {
        "Python"
    } else if dir.join("package.json").exists() {
        "JS/TS"
    } else if dir.join("go.mod").exists() {
        "Go"
    } else if [
        "Makefile",
        "Makefile.in",
        "configure",
        "configure.ac",
        "meson.build",
        "build.ninja",
        "vcpkg.json",
        "conanfile.txt",
        "CMakeLists.txt",
    ]
    .iter()
    .any(|m| dir.join(m).exists())
    {
        "C/C++"
    } else if ["pom.xml", "build.gradle"]
        .iter()
        .any(|m| dir.join(m).exists())
    {
        "Java"
    } else {
        "code"
    }
}

pub fn should_use_discovery_for_root(root: &str) -> bool {
    let p = Path::new(root);
    !p.exists() || !p.is_dir() || !has_project_marker_nearby(p)
}

fn project_dirs() -> Vec<(String, String)> {
    let base: Cow<str> = std::env::var("BUTLER_PROJECTS_ROOT")
        .map(Cow::Owned)
        .unwrap_or(Cow::Borrowed("/projects"));
    std::fs::read_dir(base.as_ref())
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            if path.is_dir() && path.join("Cargo.toml").exists() {
                entry
                    .file_name()
                    .to_str()
                    .map(|name| (name.to_string(), path.to_string_lossy().into_owned()))
            } else {
                None
            }
        })
        .collect()
}

fn child_dirs(dir: &Path, limit: usize) -> impl Iterator<Item = PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|e| {
            let p = e.path();
            p.is_dir().then_some(p)
        })
        .take(limit)
}

fn has_project_marker_nearby(dir: &Path) -> bool {
    has_any_marker(dir)
        || child_dirs(dir, 50)
            .any(|sub| has_any_marker(&sub) || child_dirs(&sub, 20).any(|s| has_any_marker(&s)))
}
