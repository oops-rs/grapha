use std::path::{Path, PathBuf};

use ignore::WalkBuilder;

/// Discover source files under `path` matching the given extensions.
/// Respects .gitignore and skips hidden directories.
pub fn discover_files(path: &Path, extensions: &[&str]) -> anyhow::Result<Vec<PathBuf>> {
    if path.is_file() {
        return Ok(vec![path.to_path_buf()]);
    }

    let mut files = Vec::new();
    let walker = WalkBuilder::new(path).hidden(true).git_ignore(true).build();

    for entry in walker {
        let entry = entry?;
        let p = entry.path();
        if p.is_file()
            && let Some(ext) = p.extension().and_then(|e| e.to_str())
            && extensions.contains(&ext)
        {
            files.push(p.to_path_buf());
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

        let result = discover_files(&file, &["rs"]).unwrap();
        assert_eq!(result, vec![file]);
    }

    #[test]
    fn discovers_files_in_directory() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.rs"), "").unwrap();
        fs::write(dir.path().join("b.rs"), "").unwrap();
        fs::write(dir.path().join("c.txt"), "").unwrap();

        let result = discover_files(dir.path(), &["rs"]).unwrap();
        assert_eq!(result.len(), 2);
        assert!(result.iter().all(|p| p.extension().unwrap() == "rs"));
    }

    #[test]
    fn skips_hidden_directories() {
        let dir = tempfile::tempdir().unwrap();
        let hidden = dir.path().join(".hidden");
        fs::create_dir(&hidden).unwrap();
        fs::write(hidden.join("secret.rs"), "").unwrap();
        fs::write(dir.path().join("visible.rs"), "").unwrap();

        let result = discover_files(dir.path(), &["rs"]).unwrap();
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn empty_directory_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let result = discover_files(dir.path(), &["rs"]).unwrap();
        assert!(result.is_empty());
    }
}
