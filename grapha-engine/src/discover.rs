use std::io::Read;
use std::path::{Path, PathBuf};

use ignore::WalkBuilder;

/// Extensions whose contents we never treat as generated/minified, regardless of
/// shape. Rust source can legitimately contain long string/array literals, so we
/// never want the minification heuristic to drop a `.rs` file.
const NEVER_GENERATED_EXTENSIONS: &[&str] = &["rs"];

/// Filename suffixes that unambiguously denote generated or bundled artifacts.
/// These are matched case-insensitively against the lowercased file name.
const GENERATED_FILENAME_SUFFIXES: &[&str] = &[
    ".min.js",
    ".min.css",
    ".min.mjs",
    ".bundle.js",
    "-bundle.js",
    ".bundle.css",
    "-bundle.css",
    ".map",
];

/// A line longer than this many characters is a strong minification signal:
/// hand-written source (even Rust string literals) virtually never approaches it.
const MAX_LINE_LEN_THRESHOLD: usize = 2_000;

/// The minified-line signal only fires for files at least this large, so we never
/// drop a small file that merely happens to contain one long line.
const MINIFIED_MIN_FILE_BYTES: u64 = 50_000;

/// The "enormous max line length" signal also requires the file's *average* line
/// to be far longer than hand-written source. Real code — even when it embeds one
/// long literal (a base64 data-URI, an inlined font/SVG/JSON blob, a long regex)
/// on a single line — stays well under this because its many other lines are
/// short; minified bundles cram everything onto a handful of lines and sit far
/// above it. This is what keeps a real source file that merely *leads* with one
/// long literal from being mistaken for a bundle.
const MIN_AVG_LINE_BYTES: usize = 1_000;

/// A large file packed into very few lines (huge bytes-per-line ratio) is the
/// classic shape of a single-line bundle. This is the lower file-size bound for
/// that signal.
const DENSE_FILE_MIN_BYTES: u64 = 30_000;

/// Upper bound on line count for the dense-file signal: a 30KB+ file with fewer
/// than this many lines is almost certainly a generated bundle.
const DENSE_FILE_MAX_LINES: usize = 20;

/// Number of leading bytes inspected to characterize a file's shape. Large enough
/// to read *past* a single embedded blob (e.g. a base64 data-URI) and still see
/// the surrounding hand-written lines, so a real source file that leads with one
/// long literal is not mistaken for a bundle. Still bounded, so a multi-MB
/// generated file is never fully loaded.
const SCAN_PREFIX_BYTES: usize = 1024 * 1024;

/// Shape metrics computed from a bounded prefix of a file.
struct PrefixShape {
    /// Length (in bytes) of the longest line seen within the scanned prefix.
    /// A final fragment without a trailing newline counts as a line.
    max_line_len: usize,
    /// Number of newline-delimited lines within the scanned prefix.
    line_count: usize,
    /// Total bytes scanned. Combined with `line_count` this yields the average
    /// bytes-per-line used to gate the long-line signal.
    scanned_bytes: usize,
}

/// Discover source files under `path` matching the given extensions.
///
/// Respects .gitignore and skips hidden directories. Files that look like
/// generated or minified bundles (see [`is_generated_or_minified`]) are skipped
/// before extraction so they cannot pollute the graph.
pub fn discover_files(path: &Path, extensions: &[&str]) -> anyhow::Result<Vec<PathBuf>> {
    discover_files_filtered(path, extensions, false)
}

/// Like [`discover_files`], but emits a diagnostic line for each skipped
/// generated/minified file when `verbose` is set.
pub fn discover_files_filtered(
    path: &Path,
    extensions: &[&str],
    verbose: bool,
) -> anyhow::Result<Vec<PathBuf>> {
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
            if is_generated_or_minified(p, verbose) {
                continue;
            }
            files.push(p.to_path_buf());
        }
    }

    files.sort();
    Ok(files)
}

/// Conservative heuristic that returns `true` when `path` looks like a generated
/// or minified source bundle that would only pollute the graph.
///
/// The check is intentionally biased toward keeping files: it requires a strong
/// combination of signals (or an unambiguous generated filename) before skipping,
/// and never skips extensions in [`NEVER_GENERATED_EXTENSIONS`] (e.g. `.rs`).
///
/// When `verbose` is set, a diagnostic line is printed describing why a file was
/// skipped. I/O errors are treated as "not generated" (fail open) so that a file
/// we cannot characterize is still handed to extraction rather than silently
/// dropped.
pub fn is_generated_or_minified(path: &Path, verbose: bool) -> bool {
    if let Some(ext) = path.extension().and_then(|e| e.to_str())
        && NEVER_GENERATED_EXTENSIONS.contains(&ext)
    {
        return false;
    }

    if let Some(reason) = generated_filename_reason(path) {
        log_skip(path, reason, verbose);
        return true;
    }

    let file_size = match std::fs::metadata(path) {
        Ok(meta) => meta.len(),
        Err(_) => return false,
    };

    // Both shape signals require a reasonably large file; skip the read entirely
    // for small files so we never touch tiny hand-written sources.
    if file_size < DENSE_FILE_MIN_BYTES {
        return false;
    }

    let shape = match scan_prefix_shape(path) {
        Ok(shape) => shape,
        Err(_) => return false,
    };

    if let Some(reason) = minified_shape_reason(file_size, &shape) {
        log_skip(path, reason, verbose);
        return true;
    }

    false
}

/// Returns a human-readable reason if `path`'s filename matches a generated
/// artifact pattern, else `None`.
fn generated_filename_reason(path: &Path) -> Option<&'static str> {
    let name = path.file_name()?.to_str()?.to_ascii_lowercase();
    GENERATED_FILENAME_SUFFIXES
        .iter()
        .find(|suffix| name.ends_with(*suffix))
        .map(|_| "generated filename pattern")
}

/// Returns a human-readable reason if the file's `size` and prefix `shape` match
/// a minified-bundle signal, else `None`.
fn minified_shape_reason(size: u64, shape: &PrefixShape) -> Option<&'static str> {
    // A large file packed into very few lines is the classic single-line bundle.
    if size >= DENSE_FILE_MIN_BYTES && shape.line_count < DENSE_FILE_MAX_LINES {
        return Some("dense bundle (large file, very few lines)");
    }
    // A very long line is only a minification signal when the file as a whole is
    // dense — i.e. its average line is also far longer than hand-written code.
    // Requiring the average gate keeps real source that embeds one long literal
    // among many normal lines (the long line alone is never sufficient).
    let avg_line_bytes = shape.scanned_bytes / shape.line_count.max(1);
    if shape.max_line_len > MAX_LINE_LEN_THRESHOLD
        && size >= MINIFIED_MIN_FILE_BYTES
        && avg_line_bytes >= MIN_AVG_LINE_BYTES
    {
        return Some("minified (long lines, very high average line length)");
    }
    None
}

/// Read up to [`SCAN_PREFIX_BYTES`] from `path` and compute its line shape.
///
/// Only a bounded prefix is read so a multi-hundred-KB bundle is never fully
/// loaded. A trailing fragment without a newline (which is exactly what a single
/// long minified line looks like within the prefix) counts as a line, so the
/// detected `max_line_len` is at least the prefix size for such files.
fn scan_prefix_shape(path: &Path) -> std::io::Result<PrefixShape> {
    let file = std::fs::File::open(path)?;
    // `read_to_end` over a bounded `take` allocates only what is actually read
    // (so a small file never allocates the full cap) and reads the whole prefix
    // even when the underlying reader returns it in several chunks.
    let mut prefix = Vec::new();
    file.take(SCAN_PREFIX_BYTES as u64)
        .read_to_end(&mut prefix)?;
    Ok(shape_of_bytes(&prefix))
}

/// Compute line shape (max line length and line count) of a byte buffer.
fn shape_of_bytes(bytes: &[u8]) -> PrefixShape {
    let mut max_line_len = 0usize;
    let mut line_count = 0usize;
    let mut current = 0usize;

    for &byte in bytes {
        if byte == b'\n' {
            max_line_len = max_line_len.max(current);
            line_count += 1;
            current = 0;
        } else {
            current += 1;
        }
    }

    // Account for a final line without a trailing newline.
    if current > 0 {
        max_line_len = max_line_len.max(current);
        line_count += 1;
    }

    PrefixShape {
        max_line_len,
        line_count,
        scanned_bytes: bytes.len(),
    }
}

/// Emit a diagnostic line for a skipped file when `verbose` is set, matching the
/// pipeline's warning style.
fn log_skip(path: &Path, reason: &str, verbose: bool) {
    if verbose {
        eprintln!(
            "  \x1b[33m!\x1b[0m skipping generated/minified file {} ({reason})",
            path.display()
        );
    }
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

    /// Build a single-line minified JS blob of roughly `target_bytes`.
    fn minified_blob(target_bytes: usize) -> String {
        let unit = "var a=function(b){return b+1};";
        let mut blob = String::with_capacity(target_bytes + unit.len());
        while blob.len() < target_bytes {
            blob.push_str(unit);
        }
        blob
    }

    #[test]
    fn skips_minified_single_line_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("console.js");
        fs::write(&file, minified_blob(120_000)).unwrap();

        assert!(is_generated_or_minified(&file, false));
    }

    #[test]
    fn keeps_large_source_with_one_embedded_long_literal() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("data.ts");
        // One very long embedded literal (a base64-style blob) on a single line,
        // followed by many lines of ordinary hand-written source. The whole file
        // is above the size bound and the one line far exceeds the max-line
        // bound, but the average line is short — so it must be KEPT.
        let blob = "x".repeat(60_000);
        let mut source = format!("export const ICON = \"{blob}\";\n");
        let line = "export function helper(value: number): number { return value + 1; }\n";
        for _ in 0..500 {
            source.push_str(line);
        }
        assert!(source.len() as u64 > MINIFIED_MIN_FILE_BYTES);
        fs::write(&file, &source).unwrap();

        assert!(!is_generated_or_minified(&file, false));
    }

    #[test]
    fn keeps_normal_multiline_ts_source() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("app.ts");
        // Many short lines, file comfortably above the dense-file size bound.
        let line = "export function compute(value: number): number { return value + 1; }\n";
        let source: String = std::iter::repeat(line).take(1_000).collect();
        assert!(source.len() as u64 > DENSE_FILE_MIN_BYTES);
        fs::write(&file, source).unwrap();

        assert!(!is_generated_or_minified(&file, false));
    }

    #[test]
    fn keeps_normal_multiline_rust_source_even_when_large() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("big.rs");
        let line = "    let result = compute(value) + offset; // a normal line of rust\n";
        let source: String = std::iter::repeat(line).take(2_000).collect();
        fs::write(&file, source).unwrap();

        assert!(!is_generated_or_minified(&file, false));
    }

    #[test]
    fn never_skips_rust_even_when_minified_shaped() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("generated.rs");
        // A pathological single-line .rs (e.g. a generated table) must still be kept.
        fs::write(&file, minified_blob(120_000)).unwrap();

        assert!(!is_generated_or_minified(&file, false));
    }

    #[test]
    fn keeps_small_js_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("tiny.js");
        fs::write(&file, "const x = 1;\nconsole.log(x);\n").unwrap();

        assert!(!is_generated_or_minified(&file, false));
    }

    #[test]
    fn keeps_small_single_line_js_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("oneline.js");
        // One long-ish line but well under the file-size bounds: must be kept.
        let source = "const data=[".to_string() + &"1,".repeat(500) + "0];";
        assert!((source.len() as u64) < DENSE_FILE_MIN_BYTES);
        fs::write(&file, source).unwrap();

        assert!(!is_generated_or_minified(&file, false));
    }

    #[test]
    fn skips_min_js_by_filename_even_when_multiline() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("vendor.min.js");
        // Multi-line and small, but the filename is unambiguous.
        fs::write(&file, "a();\nb();\nc();\n").unwrap();

        assert!(is_generated_or_minified(&file, false));
    }

    #[test]
    fn skips_source_map_by_filename() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("console.js.map");
        fs::write(&file, "{\"version\":3}\n").unwrap();

        assert!(is_generated_or_minified(&file, false));
    }

    #[test]
    fn skips_bundle_js_by_filename() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("main.bundle.js");
        fs::write(&file, "x();\n").unwrap();
        assert!(is_generated_or_minified(&file, false));

        let dashed = dir.path().join("main-bundle.js");
        fs::write(&dashed, "x();\n").unwrap();
        assert!(is_generated_or_minified(&dashed, false));
    }

    #[test]
    fn discover_filters_minified_bundle_in_directory() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("real.js"), "const x = 1;\nconst y = 2;\n").unwrap();
        fs::write(dir.path().join("console.js"), minified_blob(120_000)).unwrap();

        let result = discover_files(dir.path(), &["js"]).unwrap();
        let names: Vec<_> = result
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap().to_string())
            .collect();
        assert_eq!(names, vec!["real.js".to_string()]);
    }

    #[test]
    fn dense_few_line_bundle_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("packed.css");
        // ~40KB across a handful of lines: dense-bundle shape (no single huge line).
        let chunk = "a{color:red}".repeat(700); // ~8.4KB, no newline
        let source = format!("{chunk}\n{chunk}\n{chunk}\n{chunk}\n{chunk}\n");
        assert!((source.len() as u64) > DENSE_FILE_MIN_BYTES);
        fs::write(&file, &source).unwrap();

        assert!(is_generated_or_minified(&file, false));
    }
}
