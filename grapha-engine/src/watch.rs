use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use notify_debouncer_mini::{DebouncedEventKind, new_debouncer};

/// Events emitted by the file watcher.
#[derive(Debug)]
pub enum WatchEvent {
    FilesChanged(Vec<PathBuf>),
}

/// Start watching a project directory for source file changes.
/// Returns a receiver for debounced change events and a guard that keeps the watcher alive.
pub fn start_watcher(
    project_path: &Path,
    source_extensions: &[&str],
) -> anyhow::Result<(mpsc::Receiver<WatchEvent>, WatcherGuard)> {
    let (tx, rx) = mpsc::channel();
    let extensions: Vec<String> = source_extensions.iter().map(|s| s.to_string()).collect();
    let project = project_path.to_path_buf();

    let (debounced_tx, debounced_rx) = mpsc::channel();

    let mut debouncer = new_debouncer(Duration::from_millis(500), debounced_tx)?;

    debouncer
        .watcher()
        .watch(project_path, notify::RecursiveMode::Recursive)?;

    // Spawn a thread to filter debounced events and forward relevant ones
    let filter_thread = std::thread::Builder::new()
        .name("grapha-watch-filter".into())
        .spawn(move || {
            for result in debounced_rx {
                match result {
                    Ok(events) => {
                        let changed: Vec<PathBuf> = events
                            .into_iter()
                            .filter(|e| e.kind == DebouncedEventKind::Any)
                            .map(|e| e.path)
                            .filter(|p| is_source_file(p, &extensions, &project))
                            .collect();

                        if !changed.is_empty() {
                            // Deduplicate
                            let mut deduped = changed;
                            deduped.sort();
                            deduped.dedup();

                            if tx.send(WatchEvent::FilesChanged(deduped)).is_err() {
                                break; // receiver dropped
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("watch error: {e}");
                    }
                }
            }
        })?;

    Ok((
        rx,
        WatcherGuard {
            _debouncer: debouncer,
            _filter_thread: Some(filter_thread),
        },
    ))
}

/// Guard that keeps the watcher and filter thread alive.
/// Drop this to stop watching.
pub struct WatcherGuard {
    _debouncer: notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>,
    _filter_thread: Option<std::thread::JoinHandle<()>>,
}

fn is_source_file(path: &Path, extensions: &[String], project: &Path) -> bool {
    // Must be under the project directory
    if !path.starts_with(project) {
        return false;
    }

    // Skip hidden directories and common build artifacts
    let rel = path.strip_prefix(project).unwrap_or(path);
    for component in rel.components() {
        let s = component.as_os_str().to_string_lossy();
        if s.starts_with('.')
            || s == "target"
            || s == "node_modules"
            || s == "DerivedData"
            || s == ".build"
            || s == "Pods"
        {
            return false;
        }
    }

    // Check extension
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| extensions.iter().any(|e| e == ext))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_source_files() {
        let project = PathBuf::from("/project");
        let extensions = vec!["swift".to_string(), "rs".to_string()];

        assert!(is_source_file(
            &PathBuf::from("/project/src/main.rs"),
            &extensions,
            &project
        ));
        assert!(is_source_file(
            &PathBuf::from("/project/Foo.swift"),
            &extensions,
            &project
        ));
        assert!(!is_source_file(
            &PathBuf::from("/project/target/debug/main.rs"),
            &extensions,
            &project
        ));
        assert!(!is_source_file(
            &PathBuf::from("/project/.build/file.swift"),
            &extensions,
            &project
        ));
        assert!(!is_source_file(
            &PathBuf::from("/project/readme.md"),
            &extensions,
            &project
        ));
    }
}
