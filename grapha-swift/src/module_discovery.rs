use std::path::{Path, PathBuf};

use grapha_core::ModuleMap;
use ignore::WalkBuilder;

/// Directories that never contain a project's own Swift packages. These are
/// build outputs and vendored dependency trees; descending into them is both
/// wasteful and a correctness hazard. We prune them explicitly so discovery
/// stays fast even when they are not listed in `.gitignore`.
const PRUNED_DIRS: &[&str] = &["node_modules", "build", "DerivedData", "Pods", "target"];

pub fn discover_swift_modules(root: &Path) -> ModuleMap {
    let mut modules = ModuleMap::new();
    for package_dir in find_package_dirs(root) {
        register_package(&package_dir, &mut modules);
    }
    modules
}

/// Locate every Swift package root (a directory containing `Package.swift`)
/// under `root`, honoring `.gitignore` and skipping hidden / build directories,
/// mirroring how source files are discovered elsewhere. Packages nested inside
/// another package's tree are dropped so a single package is registered once.
fn find_package_dirs(root: &Path) -> Vec<PathBuf> {
    let walker = WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        // Honor `.gitignore` even outside a git repo so build trees stay pruned
        // for standalone checkouts and freshly cloned projects.
        .require_git(false)
        .filter_entry(|entry| {
            !entry
                .file_name()
                .to_str()
                .is_some_and(|name| PRUNED_DIRS.contains(&name))
        })
        .build();

    let mut package_dirs: Vec<PathBuf> = walker
        .flatten()
        .filter(|entry| {
            entry.file_name() == "Package.swift"
                && entry.file_type().is_some_and(|ft| ft.is_file())
        })
        .filter_map(|entry| entry.path().parent().map(Path::to_path_buf))
        .collect();

    // Sorting lexically places an ancestor path immediately before its
    // descendants, so a single pass can drop packages nested within a package
    // that was already registered.
    package_dirs.sort();
    let mut roots: Vec<PathBuf> = Vec::with_capacity(package_dirs.len());
    for dir in package_dirs {
        if roots.iter().any(|root| dir.starts_with(root)) {
            continue;
        }
        roots.push(dir);
    }
    roots
}

fn register_package(dir: &Path, modules: &mut ModuleMap) {
    let module_name = dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("unknown")
        .to_string();
    let sources_dir = dir.join("Sources");
    let source_dir = if sources_dir.is_dir() {
        sources_dir
    } else {
        dir.to_path_buf()
    };
    modules
        .modules
        .entry(module_name)
        .or_default()
        .push(source_dir);

    register_test_targets(dir, modules);
}

/// Register test targets: each subdirectory under `Tests/` becomes a module.
/// If `Tests/` has no subdirectories, register it as `<PackageName>Tests`.
fn register_test_targets(dir: &Path, modules: &mut ModuleMap) {
    let tests_dir = dir.join("Tests");
    if !tests_dir.is_dir() {
        return;
    }

    let mut found_subdirs = false;
    if let Ok(entries) = std::fs::read_dir(&tests_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if !name.starts_with('.') {
                    modules
                        .modules
                        .entry(name.to_string())
                        .or_default()
                        .push(path);
                    found_subdirs = true;
                }
            }
        }
    }

    if !found_subdirs {
        let pkg_name = dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");
        modules
            .modules
            .entry(format!("{pkg_name}Tests"))
            .or_default()
            .push(tests_dir);
    }
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

        let modules = discover_swift_modules(dir.path());
        assert!(modules.modules.contains_key("MyPackage"));
    }

    #[test]
    fn discovers_test_targets_as_modules() {
        let dir = tempfile::tempdir().unwrap();
        let pkg_dir = dir.path().join("FrameBase");
        let sources_dir = pkg_dir.join("Sources");
        let tests_dir = pkg_dir.join("Tests").join("FrameBaseTests");
        fs::create_dir_all(&sources_dir).unwrap();
        fs::create_dir_all(&tests_dir).unwrap();
        fs::write(pkg_dir.join("Package.swift"), "// swift-tools-version:5.5").unwrap();

        let modules = discover_swift_modules(dir.path());
        assert!(modules.modules.contains_key("FrameBase"));
        assert!(
            modules.modules.contains_key("FrameBaseTests"),
            "test subdirectory should be registered as a module"
        );
    }

    #[test]
    fn discovers_test_fallback_when_no_subdirs() {
        let dir = tempfile::tempdir().unwrap();
        let pkg_dir = dir.path().join("MyPkg");
        let sources_dir = pkg_dir.join("Sources");
        let tests_dir = pkg_dir.join("Tests");
        fs::create_dir_all(&sources_dir).unwrap();
        fs::create_dir_all(&tests_dir).unwrap();
        // Put a file directly in Tests/ with no subdirs
        fs::write(tests_dir.join("MyPkgTests.swift"), "import XCTest").unwrap();
        fs::write(pkg_dir.join("Package.swift"), "// swift-tools-version:5.5").unwrap();

        let modules = discover_swift_modules(dir.path());
        assert!(modules.modules.contains_key("MyPkg"));
        assert!(
            modules.modules.contains_key("MyPkgTests"),
            "Tests/ with no subdirs should fallback to <Package>Tests"
        );
    }

    #[test]
    fn prunes_build_artifact_directories() {
        let dir = tempfile::tempdir().unwrap();
        // A real package the walk should find.
        let pkg_dir = dir.path().join("RealPkg");
        fs::create_dir_all(pkg_dir.join("Sources")).unwrap();
        fs::write(pkg_dir.join("Package.swift"), "// swift-tools-version:5.5").unwrap();

        // A Package.swift buried inside Cargo's target/ output must be ignored.
        let target_pkg = dir.path().join("target").join("BuildArtifact");
        fs::create_dir_all(&target_pkg).unwrap();
        fs::write(target_pkg.join("Package.swift"), "// swift-tools-version:5.5").unwrap();

        let modules = discover_swift_modules(dir.path());
        assert!(modules.modules.contains_key("RealPkg"));
        assert!(
            !modules.modules.contains_key("BuildArtifact"),
            "packages under target/ must be pruned from discovery"
        );
    }

    #[test]
    fn skips_gitignored_directories() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".gitignore"), "vendor/\n").unwrap();

        let pkg_dir = dir.path().join("App");
        fs::create_dir_all(pkg_dir.join("Sources")).unwrap();
        fs::write(pkg_dir.join("Package.swift"), "// swift-tools-version:5.5").unwrap();

        let ignored = dir.path().join("vendor").join("Ignored");
        fs::create_dir_all(&ignored).unwrap();
        fs::write(ignored.join("Package.swift"), "// swift-tools-version:5.5").unwrap();

        let modules = discover_swift_modules(dir.path());
        assert!(modules.modules.contains_key("App"));
        assert!(
            !modules.modules.contains_key("Ignored"),
            "gitignored directories must be skipped during discovery"
        );
    }

    #[test]
    fn nested_package_is_registered_once() {
        let dir = tempfile::tempdir().unwrap();
        let outer = dir.path().join("Outer");
        fs::create_dir_all(outer.join("Sources")).unwrap();
        fs::write(outer.join("Package.swift"), "// swift-tools-version:5.5").unwrap();

        // A package nested inside another package's tree should not surface.
        let inner = outer.join("Sources").join("Inner");
        fs::create_dir_all(&inner).unwrap();
        fs::write(inner.join("Package.swift"), "// swift-tools-version:5.5").unwrap();

        let modules = discover_swift_modules(dir.path());
        assert!(modules.modules.contains_key("Outer"));
        assert!(
            !modules.modules.contains_key("Inner"),
            "packages nested inside another package are not registered"
        );
    }
}
