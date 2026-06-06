use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Maps module names to their source directories.
#[derive(Debug, Clone, PartialEq)]
pub struct ModuleMap {
    pub modules: HashMap<String, Vec<PathBuf>>,
}

impl ModuleMap {
    /// Discover modules by scanning for Package.swift and Cargo.toml files.
    ///
    /// Walk root (max depth 4) looking for Swift packages and Cargo workspaces,
    /// then build a mapping of module name to source directories.
    pub fn discover(root: &Path) -> Self {
        let mut modules: HashMap<String, Vec<PathBuf>> = HashMap::new();

        // Walk for Package.swift files (Swift packages)
        discover_swift_packages(root, &mut modules);

        // Check for Cargo.toml at root
        discover_cargo_modules(root, &mut modules);

        // Fallback: if nothing found, treat root as single module
        if modules.is_empty() {
            let name = root
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("root")
                .to_string();
            modules.insert(name, vec![root.to_path_buf()]);
        }

        ModuleMap { modules }
    }

    /// Find which module a file belongs to.
    ///
    /// Accepts both absolute and relative paths. For relative paths, attempts
    /// suffix-based matching against module source directories.
    pub fn module_for_file(&self, file: &Path) -> Option<String> {
        let canonical_file = normalize_path(file);

        let mut best_match: Option<(&str, usize)> = None;

        for (name, dirs) in &self.modules {
            for dir in dirs {
                let canonical_dir = normalize_path(dir);

                // Try prefix-based matching (works for absolute paths)
                if let Ok(suffix) = canonical_file.strip_prefix(&canonical_dir) {
                    let depth = suffix.components().count();
                    match best_match {
                        Some((_, best_depth)) if depth < best_depth => {
                            best_match = Some((name, depth));
                        }
                        None => {
                            best_match = Some((name, depth));
                        }
                        _ => {}
                    }
                }

                // Fallback: suffix-based matching for relative paths.
                // Check if the relative file path is a suffix of the module dir
                // or if the module dir name appears as a component of the file path.
                if best_match.is_none()
                    && file.is_relative()
                    && let Some(dir_name) = canonical_dir.file_name().and_then(|n| n.to_str())
                {
                    let file_str = file.to_string_lossy();
                    if file_str.starts_with(dir_name)
                        || file_str.starts_with(&format!("{dir_name}/"))
                    {
                        best_match = Some((name, usize::MAX));
                    }
                }
            }
        }

        best_match.map(|(name, _)| name.to_string())
    }
}

/// Normalize a path by resolving . and .. without requiring the path to exist.
fn normalize_path(path: &Path) -> PathBuf {
    // Try canonicalize first (works if path exists)
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    // Fallback: manual normalization
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                components.pop();
            }
            std::path::Component::CurDir => {}
            other => components.push(other),
        }
    }
    components.iter().collect()
}

/// Recursively walk directories looking for Package.swift files.
/// When found, the containing directory is a Swift package module.
/// We do NOT descend into directories that contain Package.swift
/// (they are leaf modules, not containers of sub-modules).
fn discover_swift_packages(root: &Path, modules: &mut HashMap<String, Vec<PathBuf>>) {
    discover_swift_packages_recursive(root, modules);
}

fn discover_swift_packages_recursive(dir: &Path, modules: &mut HashMap<String, Vec<PathBuf>>) {
    let package_swift = dir.join("Package.swift");
    if package_swift.is_file() {
        // This directory is a Swift package — register it and stop recursing
        let module_name = dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        let sources_dir = dir.join("Sources");
        let source_dir = if sources_dir.is_dir() {
            sources_dir
        } else {
            dir.to_path_buf()
        };

        modules.entry(module_name).or_default().push(source_dir);
        return; // Don't descend further
    }

    // No Package.swift here — recurse into subdirectories
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        // Skip hidden dirs, build dirs, and common non-source dirs
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with('.')
            || name_str == "node_modules"
            || name_str == "build"
            || name_str == "DerivedData"
            || name_str == "Pods"
        {
            continue;
        }
        discover_swift_packages_recursive(&path, modules);
    }
}

fn discover_cargo_modules(root: &Path, modules: &mut HashMap<String, Vec<PathBuf>>) {
    let cargo_toml = root.join("Cargo.toml");
    if !cargo_toml.is_file() {
        return;
    }

    let content = match std::fs::read_to_string(&cargo_toml) {
        Ok(c) => c,
        Err(_) => return,
    };

    let parsed = match content.parse::<toml::Table>() {
        Ok(t) => t,
        Err(_) => return,
    };

    if let Some(workspace) = parsed.get("workspace").and_then(|w| w.as_table()) {
        if let Some(members) = workspace.get("members").and_then(|m| m.as_array()) {
            for member in members {
                if let Some(pattern) = member.as_str() {
                    expand_workspace_member(root, pattern, modules);
                }
            }
        }
    } else {
        // Single crate — use package name or dir name
        let name = parsed
            .get("package")
            .and_then(|p| p.as_table())
            .and_then(|p| p.get("name"))
            .and_then(|n| n.as_str())
            .or_else(|| root.file_name().and_then(|n| n.to_str()))
            .unwrap_or("root")
            .to_string();

        let src_dir = root.join("src");
        let source_dir = if src_dir.is_dir() {
            src_dir
        } else {
            root.to_path_buf()
        };

        modules.entry(name).or_default().push(source_dir);
    }
}

fn expand_workspace_member(
    root: &Path,
    pattern: &str,
    modules: &mut HashMap<String, Vec<PathBuf>>,
) {
    if pattern.contains('*') {
        // Glob pattern like "crates/*" — expand by listing directory
        let prefix = pattern.trim_end_matches('*').trim_end_matches('/');
        let parent_dir = root.join(prefix);
        if parent_dir.is_dir()
            && let Ok(entries) = std::fs::read_dir(&parent_dir)
        {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    add_cargo_member(root, &path, modules);
                }
            }
        }
    } else {
        let member_path = root.join(pattern);
        if member_path.is_dir() {
            add_cargo_member(root, &member_path, modules);
        }
    }
}

fn add_cargo_member(_root: &Path, member_path: &Path, modules: &mut HashMap<String, Vec<PathBuf>>) {
    let name = member_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();

    let src_dir = member_path.join("src");
    let source_dir = if src_dir.is_dir() {
        src_dir
    } else {
        member_path.to_path_buf()
    };

    modules.entry(name).or_default().push(source_dir);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn discovers_swift_package() {
        let dir = tempfile::tempdir().unwrap();
        let pkg_dir = dir.path().join("MyPackage");
        let sources_dir = pkg_dir.join("Sources");
        fs::create_dir_all(&sources_dir).unwrap();
        fs::write(pkg_dir.join("Package.swift"), "// swift-tools-version:5.5").unwrap();

        let map = ModuleMap::discover(dir.path());
        assert!(map.modules.contains_key("MyPackage"));
        let dirs = &map.modules["MyPackage"];
        assert_eq!(dirs.len(), 1);
        assert!(dirs[0].ends_with("Sources"));
    }

    #[test]
    fn discovers_cargo_workspace() {
        let dir = tempfile::tempdir().unwrap();

        // Create workspace Cargo.toml
        let cargo_toml = r#"
[workspace]
members = ["crates/*"]
"#;
        fs::write(dir.path().join("Cargo.toml"), cargo_toml).unwrap();

        // Create two member crates
        let crate_a = dir.path().join("crates/alpha");
        let crate_b = dir.path().join("crates/beta");
        fs::create_dir_all(crate_a.join("src")).unwrap();
        fs::create_dir_all(crate_b.join("src")).unwrap();

        let map = ModuleMap::discover(dir.path());
        assert!(
            map.modules.contains_key("alpha"),
            "modules: {:?}",
            map.modules
        );
        assert!(
            map.modules.contains_key("beta"),
            "modules: {:?}",
            map.modules
        );
    }

    #[test]
    fn module_for_file_finds_correct_module() {
        let dir = tempfile::tempdir().unwrap();

        // Create workspace with two crates
        let cargo_toml = r#"
[workspace]
members = ["core", "api"]
"#;
        fs::write(dir.path().join("Cargo.toml"), cargo_toml).unwrap();

        let core_src = dir.path().join("core/src");
        let api_src = dir.path().join("api/src");
        fs::create_dir_all(&core_src).unwrap();
        fs::create_dir_all(&api_src).unwrap();
        fs::write(core_src.join("lib.rs"), "").unwrap();
        fs::write(api_src.join("main.rs"), "").unwrap();

        let map = ModuleMap::discover(dir.path());

        assert_eq!(
            map.module_for_file(&core_src.join("lib.rs")),
            Some("core".to_string())
        );
        assert_eq!(
            map.module_for_file(&api_src.join("main.rs")),
            Some("api".to_string())
        );
    }

    #[test]
    fn falls_back_to_root_module() {
        let dir = tempfile::tempdir().unwrap();
        // Empty dir — no Package.swift, no Cargo.toml

        let map = ModuleMap::discover(dir.path());
        assert_eq!(map.modules.len(), 1);
        // Should have an entry for the temp dir name
        assert!(!map.modules.is_empty());
    }

    #[test]
    fn discovers_single_cargo_crate() {
        let dir = tempfile::tempdir().unwrap();

        let cargo_toml = r#"
[package]
name = "my_app"
version = "0.1.0"
edition = "2021"
"#;
        fs::write(dir.path().join("Cargo.toml"), cargo_toml).unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();

        let map = ModuleMap::discover(dir.path());
        assert!(
            map.modules.contains_key("my_app"),
            "modules: {:?}",
            map.modules
        );
    }
}
