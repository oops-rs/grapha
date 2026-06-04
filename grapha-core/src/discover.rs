use std::path::{Path, PathBuf};

use ignore::WalkBuilder;

/// Discover source files under `path` matching the given extensions.
///
/// Directory discovery uses Git-aware ignore rules and skips hidden paths.
/// Explicit file inputs are returned directly.
pub fn discover_files(path: &Path, extensions: &[String]) -> anyhow::Result<Vec<PathBuf>> {
    if path.is_file() {
        return Ok(vec![path.to_path_buf()]);
    }

    let mut files = Vec::new();
    let walker = WalkBuilder::new(path).hidden(true).git_ignore(true).build();

    for entry in walker {
        let entry = entry?;
        let entry_path = entry.path();
        if entry_path.is_file()
            && let Some(ext) = entry_path.extension().and_then(|ext| ext.to_str())
            && extensions.iter().any(|candidate| candidate == ext)
        {
            files.push(entry_path.to_path_buf());
        }
    }

    files.sort();
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn discovers_single_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("main.rs");
        fs::write(&file, "fn main() {}").unwrap();

        let result = discover_files(&file, &["rs".to_string()]).unwrap();
        assert_eq!(result, vec![file]);
    }

    #[test]
    fn discovers_files_in_directory() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.rs"), "").unwrap();
        fs::write(dir.path().join("b.rs"), "").unwrap();
        fs::write(dir.path().join("c.txt"), "").unwrap();

        let result = discover_files(dir.path(), &["rs".to_string()]).unwrap();
        assert_eq!(result.len(), 2);
        assert!(result.iter().all(|path| path.extension().unwrap() == "rs"));
    }

    #[test]
    fn directory_discovery_respects_gitignore_inside_git_repo() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        fs::write(dir.path().join(".gitignore"), "ignored.rs\n").unwrap();
        let visible = dir.path().join("visible.rs");
        let ignored = dir.path().join("ignored.rs");
        fs::write(&visible, "fn visible() {}").unwrap();
        fs::write(ignored, "fn ignored() {}").unwrap();

        let result = discover_files(dir.path(), &["rs".to_string()]).unwrap();

        assert_eq!(result, vec![visible]);
    }
}
