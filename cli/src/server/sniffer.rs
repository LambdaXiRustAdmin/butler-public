//! Lightweight polyglot direct dependency sniffer.
//!
//! Extracts *direct* dependency names (with ultra-compact version constraints)
//! from common manifest files. Designed to be called only for ArchitecturalSummary
//! to give LLMs cheap ecosystem awareness without bloating traces or requiring
//! heavy toolchains (no cargo metadata, no pip, etc.).
//!
//! Output format: comma-separated "name@version" strings, e.g.
//!   "tokio@1.38.0, serde@1.0, pydantic@>=2.0.0"
//!
//! Only names+constraints from direct deps (no transitive).

use std::path::Path;

/// Main entry point. Returns Some(joined_string) if any direct deps found,
/// None otherwise. Always returns a single string suitable for telemetry.
pub fn sniff_direct_dependencies(root: &Path) -> Option<String> {
    let mut deps: Vec<String> = Vec::new();

    // Rust
    let cargo = root.join("Cargo.toml");
    if cargo.exists() {
        if let Ok(content) = std::fs::read_to_string(&cargo) {
            deps.extend(parse_cargo_direct_deps(&content));
        }
    }

    // Python
    let pyproject = root.join("pyproject.toml");
    if pyproject.exists() {
        if let Ok(content) = std::fs::read_to_string(&pyproject) {
            deps.extend(parse_pyproject_direct_deps(&content));
        }
    }

    let requirements = root.join("requirements.txt");
    if requirements.exists() {
        if let Ok(content) = std::fs::read_to_string(&requirements) {
            deps.extend(parse_requirements_direct_deps(&content));
        }
    }

    // TypeScript / JavaScript
    let package_json = root.join("package.json");
    if package_json.exists() {
        if let Ok(content) = std::fs::read_to_string(&package_json) {
            deps.extend(parse_package_json_direct_deps(&content));
        }
    }

    // Go
    let go_mod = root.join("go.mod");
    if go_mod.exists() {
        if let Ok(content) = std::fs::read_to_string(&go_mod) {
            deps.extend(parse_go_mod_direct_deps(&content));
        }
    }

    // C/C++
    let cmake = root.join("CMakeLists.txt");
    if cmake.exists() {
        if let Ok(content) = std::fs::read_to_string(&cmake) {
            deps.extend(parse_cmake_direct_deps(&content));
        }
    }
    let vcpkg = root.join("vcpkg.json");
    if vcpkg.exists() {
        if let Ok(content) = std::fs::read_to_string(&vcpkg) {
            deps.extend(parse_vcpkg_direct_deps(&content));
        }
    }
    let conan = root.join("conanfile.txt");
    if conan.exists() {
        if let Ok(content) = std::fs::read_to_string(&conan) {
            deps.extend(parse_conan_direct_deps(&content));
        }
    }

    if deps.is_empty() {
        return None;
    }

    deps.sort();
    deps.dedup();
    Some(deps.join(", "))
}

// --- Rust Cargo.toml parser (simple, no toml crate) ---

fn parse_cargo_direct_deps(content: &str) -> Vec<String> {
    let mut out = vec![];
    let mut in_deps_section = false;

    for line in content.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_deps_section = t == "[dependencies]" || t == "[workspace.dependencies]";
            continue;
        }
        if !in_deps_section {
            continue;
        }
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if !t.contains('=') {
            continue;
        }

        // name = "1.0" or name = { version = "1.0", features = [...] }
        if let Some((left, right)) = t.split_once('=') {
            let name = left.trim().trim_matches('"').trim_matches('\'').to_string();
            if name.is_empty() || name == "workspace" {
                continue;
            }

            let right = right.trim();
            let ver = if right.starts_with('{') {
                // inline table: extract version = "..."
                if let Some(vstart) = right.find("version") {
                    if let Some(vend) = right[vstart..].find('=') {
                        let after = &right[vstart + vend + 1..].trim();
                        after
                            .split([',', '}', ' '])
                            .next()
                            .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
                            .unwrap_or_default()
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                }
            } else {
                // simple "1.0" or "^1.0" or { workspace = true } etc.
                right
                    .split([',', '}', ' '])
                    .next()
                    .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
                    .unwrap_or_default()
            };

            if ver.is_empty() || ver == "true" || ver == "false" || ver.contains("workspace") {
                out.push(name);
            } else {
                out.push(format!("{}@{}", name, ver));
            }
        }
    }
    out
}

// --- Python pyproject.toml parser (handles PEP-621 array + poetry table) ---

fn parse_pyproject_direct_deps(content: &str) -> Vec<String> {
    let mut out = vec![];
    let mut in_project = false;
    let mut in_poetry_deps = false;
    let mut in_deps_array = false;

    for line in content.lines() {
        let t = line.trim();

        if t.starts_with('[') {
            in_project = t == "[project]";
            in_poetry_deps = t.contains("tool.poetry.dependencies");
            in_deps_array = false;
            continue;
        }

        if in_project && t.starts_with("dependencies") && t.contains('=') && t.contains('[') {
            in_deps_array = true;
            continue;
        }

        if in_deps_array {
            if t.starts_with(']') {
                in_deps_array = false;
                continue;
            }
            // "pkg >= 1.0" or "pkg[extra]>=2"
            if let Some(start) = t.find('"') {
                if let Some(rel_end) = t[start + 1..].find('"') {
                    let dep_str = &t[start + 1..start + 1 + rel_end];
                    let dep_str = dep_str.trim();
                    if let Some(pos) = dep_str.find(|c: char| {
                        c.is_whitespace()
                            || c == '>'
                            || c == '<'
                            || c == '='
                            || c == '~'
                            || c == '!'
                    }) {
                        let name_part = &dep_str[..pos];
                        let ver_part = &dep_str[pos..];
                        let name = name_part.trim_end_matches(['[', ']']).to_string();
                        let ver = ver_part
                            .trim()
                            .chars()
                            .filter(|c| !c.is_whitespace())
                            .collect::<String>();
                        if !name.is_empty() {
                            if ver.is_empty() {
                                out.push(name);
                            } else {
                                out.push(format!("{}@{}", name, ver));
                            }
                        }
                    } else if !dep_str.is_empty() {
                        out.push(dep_str.to_string());
                    }
                }
            }
            continue;
        }

        // Poetry table style: foo = "^1.0" or foo = { version = "^1.0", ... }
        if in_poetry_deps && t.contains('=') && !t.starts_with('#') {
            if let Some((name, right)) = t.split_once('=') {
                let name = name.trim().trim_matches('"').trim_matches('\'').to_string();
                if name.is_empty() {
                    continue;
                }
                let right = right.trim();
                let ver = if right.starts_with('{') {
                    if let Some(vpos) = right.find("version") {
                        if let Some(after) = right[vpos..].split('=').nth(1) {
                            after
                                .split([',', '}'])
                                .next()
                                .unwrap_or("")
                                .trim()
                                .trim_matches('"')
                                .trim_matches('\'')
                                .to_string()
                        } else {
                            String::new()
                        }
                    } else {
                        String::new()
                    }
                } else {
                    right
                        .trim_matches('"')
                        .trim_matches('\'')
                        .trim_matches(',')
                        .to_string()
                };
                if ver.is_empty() {
                    out.push(name);
                } else {
                    out.push(format!("{}@{}", name, ver));
                }
            }
        }
    }
    out
}

// --- package.json parser (dependencies + devDependencies, compact @ format) ---

fn parse_package_json_direct_deps(content: &str) -> Vec<String> {
    let mut out = vec![];
    for section in ["dependencies", "devDependencies"] {
        if let Some(start) = content.find(&format!("\"{}\":", section)) {
            if let Some(obj_start) = content[start..].find('{') {
                let obj = &content[start + obj_start + 1..];
                if let Some(obj_end) = obj.find('}') {
                    let deps_obj = &obj[..obj_end];
                    for line in deps_obj.lines() {
                        let t = line.trim();
                        if t.contains(':') && t.contains('"') {
                            if let Some((k, v)) = t.split_once(':') {
                                let name =
                                    k.trim().trim_matches('"').trim_matches('\'').to_string();
                                let ver = v
                                    .trim()
                                    .trim_matches('"')
                                    .trim_matches('\'')
                                    .trim_matches(',')
                                    .to_string();
                                if !name.is_empty() {
                                    if ver.is_empty() || ver == "*" {
                                        out.push(name);
                                    } else {
                                        out.push(format!("{}@{}", name, ver));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    out
}

// --- Go go.mod parser (require directives, compact @v format per parity) ---

fn parse_go_mod_direct_deps(content: &str) -> Vec<String> {
    let mut out = vec![];
    let mut in_require = false;
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with("require") {
            if t.contains('(') {
                in_require = true;
            } else if let Some(after) = t.split_whitespace().nth(1) {
                // single-line: require github.com/foo/bar v1.2.3
                let parts: Vec<&str> = after.split_whitespace().collect();
                if parts.len() >= 2 {
                    let name = parts[0].to_string();
                    let ver = parts[1].to_string();
                    if !name.is_empty() {
                        out.push(format!("{}@{}", name, ver));
                    }
                }
            }
            continue;
        }
        if t == ")" {
            in_require = false;
            continue;
        }
        if in_require
            && !t.is_empty()
            && !t.starts_with("//")
            && !t.starts_with("replace")
            && !t.starts_with("exclude")
        {
            // github.com/foo/bar v1.2.3
            let parts: Vec<&str> = t.split_whitespace().collect();
            if parts.len() >= 2 {
                let name = parts[0].to_string();
                let ver = parts[1].to_string();
                if !name.is_empty() {
                    out.push(format!("{}@{}", name, ver));
                }
            }
        }
    }
    out
}

// --- C/C++ package manager parsers (CMake, vcpkg, conan) with compact parity format ---

fn parse_cmake_direct_deps(content: &str) -> Vec<String> {
    let mut out = vec![];
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with("find_package") {
            // find_package(Boost 1.74.0 REQUIRED) or find_package(OpenSSL)
            if let Some(start) = t.find('(') {
                if let Some(end) = t[start + 1..].find(')') {
                    let inside = &t[start + 1..start + 1 + end];
                    // Split on whitespace and parens, take first real token as name
                    let tokens: Vec<&str> = inside
                        .split(|c: char| c.is_whitespace() || c == '(' || c == ')')
                        .filter(|s| !s.is_empty())
                        .collect();
                    if !tokens.is_empty() {
                        let name = tokens[0]
                            .trim_matches(|c: char| c == '"' || c == '\'')
                            .to_string();
                        if name.is_empty() {
                            continue;
                        }
                        // Look for a version token after name; skip known keywords
                        let mut ver = String::new();
                        for tok in &tokens[1..] {
                            let t = tok.trim_matches(|c: char| {
                                c == '"' || c == '\'' || c == ',' || c == ':'
                            });
                            if t == "REQUIRED"
                                || t == "CONFIG"
                                || t == "EXACT"
                                || t == "COMPONENTS"
                                || t == "NAMES"
                                || t == "PATHS"
                                || t == "HINTS"
                            {
                                continue;
                            }
                            // version if starts with digit or 'v' + digit
                            if t.starts_with(|c: char| c.is_ascii_digit())
                                || (t.starts_with('v')
                                    && t.len() > 1
                                    && t[1..].starts_with(|c: char| c.is_ascii_digit()))
                            {
                                ver = t.to_string();
                                break;
                            }
                        }
                        if ver.is_empty() {
                            out.push(name);
                        } else {
                            out.push(format!("{}@{}", name, ver));
                        }
                    }
                }
            }
        }
    }
    out
}

fn parse_vcpkg_direct_deps(content: &str) -> Vec<String> {
    let mut out = vec![];
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(content) {
        if let Some(deps) = val.get("dependencies") {
            if let Some(arr) = deps.as_array() {
                for item in arr {
                    if let Some(s) = item.as_str() {
                        out.push(s.to_string());
                    } else if let Some(obj) = item.as_object() {
                        if let Some(name_val) = obj.get("name") {
                            if let Some(name) = name_val.as_str() {
                                if let Some(ver_val) = obj.get("version") {
                                    if let Some(ver) = ver_val.as_str() {
                                        out.push(format!("{}@{}", name, ver));
                                    } else {
                                        out.push(name.to_string());
                                    }
                                } else {
                                    out.push(name.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    out
}

fn parse_conan_direct_deps(content: &str) -> Vec<String> {
    let mut out = vec![];
    for line in content.lines() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') || l.starts_with('[') {
            continue;
        }
        // e.g. boost/1.74.0 or openssl/1.1.1g
        if let Some((name, ver)) = l.split_once('/') {
            let name = name.trim().to_string();
            let ver = ver.trim().to_string();
            if !name.is_empty() {
                if ver.is_empty() {
                    out.push(name);
                } else {
                    out.push(format!("{}@{}", name, ver));
                }
            }
        } else if !l.is_empty() {
            out.push(l.to_string());
        }
    }
    out
}

// --- requirements.txt parser ---

fn parse_requirements_direct_deps(content: &str) -> Vec<String> {
    let mut out = vec![];
    for line in content.lines() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') || l.starts_with('-') {
            continue;
        }
        // pkg>=1.0 or pkg[extra]>=1.0 ; comment
        let spec = l.split([' ', ';', '#']).next().unwrap_or(l).trim();
        if let Some((name_raw, ver_raw)) = spec.split_once(['>', '<', '=', '~', '!']) {
            let name = name_raw.trim_end_matches(['[', ']']).to_string();
            let ver = ver_raw.trim().to_string();
            if !name.is_empty() {
                if ver.is_empty() {
                    out.push(name);
                } else {
                    out.push(format!("{}@{}", name, ver));
                }
            }
        } else if !spec.is_empty() {
            let name = spec.trim_end_matches(['[', ']']).to_string();
            if !name.is_empty() {
                out.push(name);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cargo_basic() {
        let toml = r#"
[dependencies]
tokio = "1.38.0"
serde = { version = "1.0", features = ["derive"] }
"#;
        let out = parse_cargo_direct_deps(toml);
        assert!(out.contains(&"tokio@1.38.0".to_string()));
        assert!(out.contains(&"serde@1.0".to_string()));
    }

    #[test]
    fn test_pyproject_pep621_array() {
        let toml = r#"
[project]
dependencies = [
    "typer >= 0.9.0",
    "rich",
    "shellingham>=1.3.0",
]
"#;
        let out = parse_pyproject_direct_deps(toml);
        assert!(out.contains(&"typer@>=0.9.0".to_string()));
        assert!(out.contains(&"rich".to_string()));
        assert!(out.contains(&"shellingham@>=1.3.0".to_string()));
    }
}
