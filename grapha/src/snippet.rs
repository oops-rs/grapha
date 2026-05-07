use grapha_core::graph::{NodeKind, Span};

const MAX_COMPACT_SYMBOL_SNIPPET_CHARS: usize = 600;

#[cfg(test)]
use std::cell::Cell;

#[cfg(test)]
thread_local! {
    static DECLARATION_BLOCK_SCAN_COUNT: Cell<usize> = const { Cell::new(0) };
}

pub fn should_extract_snippet(kind: NodeKind) -> bool {
    !matches!(kind, NodeKind::Field | NodeKind::View | NodeKind::Branch)
}

pub fn trim_snippet_indentation(snippet: &str) -> String {
    let lines: Vec<&str> = snippet.lines().collect();
    let min_indent = lines
        .iter()
        .filter_map(|line| {
            let trimmed = line.trim_end();
            (!trimmed.is_empty())
                .then_some(line.chars().take_while(|ch| ch.is_whitespace()).count())
        })
        .min()
        .unwrap_or(0);

    lines
        .iter()
        .map(|line| {
            if line.trim_end().is_empty() {
                String::new()
            } else {
                line.chars()
                    .skip(min_indent)
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim_matches('\n')
        .to_string()
}

pub fn compact_symbol_snippet(snippet: &str) -> String {
    let deindented = trim_snippet_indentation(snippet);
    let compact = deindented
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");

    if compact.len() <= MAX_COMPACT_SYMBOL_SNIPPET_CHARS {
        return compact;
    }

    let mut truncate_at = MAX_COMPACT_SYMBOL_SNIPPET_CHARS;
    while !compact.is_char_boundary(truncate_at) {
        truncate_at -= 1;
    }
    format!("{} ...", compact[..truncate_at].trim_end())
}

/// Pre-computed line index for a source file. Build once, query many times.
pub struct LineIndex<'a> {
    source: &'a str,
    /// Byte offsets of the start of each line.
    line_starts: Vec<usize>,
    /// Cached split lines for declaration scanning.
    lines: Vec<&'a str>,
}

struct SnippetCandidate {
    snippet: String,
    anchor_line: usize,
}

impl<'a> LineIndex<'a> {
    pub fn new(source: &'a str) -> Self {
        let mut line_starts = vec![0usize];
        for (i, b) in source.bytes().enumerate() {
            if b == b'\n' && i + 1 < source.len() {
                line_starts.push(i + 1);
            }
        }
        let lines: Vec<&str> = source.lines().collect();
        Self {
            source,
            line_starts,
            lines,
        }
    }

    fn normalize_line(&self, line: usize, one_based: bool) -> Option<usize> {
        if one_based {
            line.checked_sub(1)
        } else {
            Some(line)
        }
    }

    fn byte_offset(&self, line: usize, column: usize) -> Option<usize> {
        let line_start = *self.line_starts.get(line)?;
        let line_limit = self
            .line_starts
            .get(line + 1)
            .copied()
            .unwrap_or(self.source.len());
        Some((line_start + column).min(line_limit))
    }

    fn extract_exact_span_with_base(&self, span: &Span, one_based_lines: bool) -> Option<String> {
        let start_line = self.normalize_line(span.start[0], one_based_lines)?;
        let end_line = self.normalize_line(span.end[0], one_based_lines)?;
        let byte_start = self.byte_offset(start_line, span.start[1])?;
        let byte_end = self.byte_offset(end_line, span.end[1])?;
        if byte_start > byte_end || byte_end > self.source.len() {
            return None;
        }

        let slice = &self.source.as_bytes()[byte_start..byte_end];
        let snippet = String::from_utf8_lossy(slice)
            .trim_end_matches(['\n', '\r'])
            .to_string();
        if snippet.is_empty() {
            None
        } else {
            Some(snippet)
        }
    }

    fn declaration_line_for_symbol(
        &self,
        symbol: &str,
        kind: NodeKind,
        preferred_line: usize,
    ) -> Option<SnippetCandidate> {
        let symbol = symbol.trim();
        if symbol.is_empty() {
            return None;
        }
        search_outward_from(&self.lines, preferred_line, |idx, line| {
            let trimmed = line.trim_end();
            declaration_matches_symbol(trimmed, symbol, kind).then(|| SnippetCandidate {
                snippet: trimmed.to_string(),
                anchor_line: idx,
            })
        })
    }

    fn declaration_block_for_symbol(
        &self,
        symbol: &str,
        kind: NodeKind,
        preferred_line: usize,
    ) -> Option<SnippetCandidate> {
        #[cfg(test)]
        DECLARATION_BLOCK_SCAN_COUNT.with(|count| count.set(count.get() + 1));

        if kind != NodeKind::Function {
            return None;
        }

        let symbol = symbol.trim();
        if symbol.is_empty() {
            return None;
        }

        let start_idx = search_outward_from(&self.lines, preferred_line, |idx, line| {
            declaration_matches_symbol(line.trim_end(), symbol, kind).then_some(idx)
        })?;

        let mut collected = Vec::new();
        let mut brace_depth = 0usize;
        let mut saw_open_brace = false;

        for line in self.lines.iter().skip(start_idx) {
            let trimmed = line.trim_end();
            collected.push(*line);

            for ch in trimmed.chars() {
                match ch {
                    '{' => {
                        saw_open_brace = true;
                        brace_depth += 1;
                    }
                    '}' => {
                        brace_depth = brace_depth.saturating_sub(1);
                    }
                    _ => {}
                }
            }

            if saw_open_brace && brace_depth == 0 {
                return Some(SnippetCandidate {
                    snippet: trim_snippet_indentation(&collected.join("\n")),
                    anchor_line: start_idx,
                });
            }
        }

        None
    }

    fn score_candidate(
        candidate: &SnippetCandidate,
        symbol: &str,
        kind: NodeKind,
        preferred_line: usize,
    ) -> (usize, usize, usize, usize, usize) {
        let trimmed = candidate.snippet.trim();
        let symbol_match = usize::from(!symbol.is_empty() && trimmed.contains(symbol));
        let declaration_head_match = usize::from(
            trimmed
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty() && !line.starts_with('@'))
                .is_some_and(|line| declaration_matches_symbol(line, symbol, kind)),
        );
        let kind_match = usize::from(match kind {
            NodeKind::Function => {
                trimmed.contains("func ")
                    || trimmed.contains("init(")
                    || trimmed.contains("subscript")
                    || trimmed.contains("var ")
            }
            NodeKind::Class => trimmed.contains("class "),
            NodeKind::Property | NodeKind::Field | NodeKind::Constant => {
                trimmed.contains("var ") || trimmed.contains("let ")
            }
            NodeKind::Struct => trimmed.contains("struct "),
            NodeKind::Trait => trimmed.contains("trait "),
            NodeKind::Impl => trimmed.contains("impl "),
            NodeKind::Enum => trimmed.contains("enum "),
            NodeKind::Protocol => trimmed.contains("protocol "),
            NodeKind::Extension => trimmed.contains("extension "),
            _ => true,
        });
        let body_match = usize::from(
            kind == NodeKind::Function && trimmed.contains('{') && trimmed.contains('}'),
        );
        let distance = candidate.anchor_line.abs_diff(preferred_line);

        (
            symbol_match,
            declaration_head_match,
            kind_match,
            body_match,
            usize::MAX.saturating_sub(distance),
        )
    }

    fn extract_full_lines_with_base(&self, span: &Span, one_based_lines: bool) -> Option<String> {
        let start_line = self.normalize_line(span.start[0], one_based_lines)?;
        let end_line = self.normalize_line(span.end[0], one_based_lines)?;

        if start_line >= self.line_starts.len() {
            return None;
        }

        let end_line = end_line.min(self.line_starts.len().saturating_sub(1));
        let byte_start = self.line_starts[start_line];
        let byte_end = if end_line + 1 < self.line_starts.len() {
            self.line_starts[end_line + 1].saturating_sub(1)
        } else {
            self.source.len()
        };

        let slice = &self.source.as_bytes()[byte_start..byte_end];
        Some(
            String::from_utf8_lossy(slice)
                .trim_end_matches(['\n', '\r'])
                .to_string(),
        )
    }

    pub fn extract_full_snippet(&self, span: &Span) -> Option<String> {
        self.extract_exact_span_with_base(span, false)
            .or_else(|| self.extract_exact_span_with_base(span, true))
            .or_else(|| self.extract_full_lines_with_base(span, false))
            .or_else(|| self.extract_full_lines_with_base(span, true))
    }

    pub fn extract_symbol_snippet(
        &self,
        span: &Span,
        symbol_name: &str,
        kind: NodeKind,
    ) -> Option<String> {
        let preferred_line = span.start[0];
        let symbol = symbol_name
            .strip_prefix("getter:")
            .or_else(|| symbol_name.strip_prefix("setter:"))
            .unwrap_or(symbol_name)
            .split('(')
            .next()
            .unwrap_or(symbol_name)
            .trim();

        let mut candidates = Vec::new();
        for candidate in [
            self.extract_exact_span_with_base(span, false)
                .map(|snippet| SnippetCandidate {
                    snippet,
                    anchor_line: span.start[0],
                }),
            self.extract_exact_span_with_base(span, true)
                .map(|snippet| SnippetCandidate {
                    snippet,
                    anchor_line: span.start[0].saturating_sub(1),
                }),
            self.extract_full_lines_with_base(span, false)
                .map(|snippet| SnippetCandidate {
                    snippet,
                    anchor_line: span.start[0],
                }),
            self.extract_full_lines_with_base(span, true)
                .map(|snippet| SnippetCandidate {
                    snippet,
                    anchor_line: span.start[0].saturating_sub(1),
                }),
        ]
        .into_iter()
        .flatten()
        {
            push_unique_candidate(&mut candidates, candidate);
        }

        if let Some(best) = best_snippet_candidate(&candidates, symbol, kind, preferred_line) {
            let best_score = Self::score_candidate(&best, symbol, kind, preferred_line);
            if is_sufficient_snippet_match(&best_score, kind) {
                return Some(trim_snippet_indentation(&best.snippet));
            }
        }

        if let Some(candidate) = self.declaration_block_for_symbol(symbol, kind, preferred_line) {
            push_unique_candidate(&mut candidates, candidate);
            if let Some(best) = best_snippet_candidate(&candidates, symbol, kind, preferred_line) {
                let best_score = Self::score_candidate(&best, symbol, kind, preferred_line);
                if is_sufficient_snippet_match(&best_score, kind) {
                    return Some(trim_snippet_indentation(&best.snippet));
                }
            }
        }

        self.declaration_line_for_symbol(symbol, kind, preferred_line)
            .map(|candidate| trim_snippet_indentation(&candidate.snippet))
    }

    #[allow(dead_code)]
    pub fn extract_snippet(&self, span: &Span, max_len: usize) -> Option<String> {
        let full = self.extract_full_snippet(span)?;

        if full.len() <= max_len {
            return Some(full);
        }

        let mut truncate_at = max_len;
        while !full.is_char_boundary(truncate_at) {
            truncate_at -= 1;
        }

        let truncated = &full[..truncate_at];
        match truncated.rfind('\n') {
            Some(pos) if pos > 0 => Some(truncated[..pos].to_string()),
            _ => Some(truncated.to_string()),
        }
    }

    #[cfg(test)]
    fn reset_declaration_block_scan_count() {
        DECLARATION_BLOCK_SCAN_COUNT.with(|count| count.set(0));
    }

    #[cfg(test)]
    fn declaration_block_scan_count() -> usize {
        DECLARATION_BLOCK_SCAN_COUNT.with(Cell::get)
    }
}

const MAX_DECLARATION_SEARCH_RADIUS: usize = 200;

fn search_outward_from<T>(
    lines: &[&str],
    preferred: usize,
    mut check: impl FnMut(usize, &str) -> Option<T>,
) -> Option<T> {
    let len = lines.len();
    if len == 0 {
        return None;
    }
    let center = preferred.min(len - 1);
    if let Some(result) = check(center, lines[center]) {
        return Some(result);
    }
    let max_delta = MAX_DECLARATION_SEARCH_RADIUS.min(len);
    for delta in 1..max_delta {
        let lo = center.checked_sub(delta);
        let hi = center + delta;
        if let Some(lo) = lo
            && let Some(result) = check(lo, lines[lo])
        {
            return Some(result);
        }
        if hi < len
            && let Some(result) = check(hi, lines[hi])
        {
            return Some(result);
        }
        if lo.is_none() && hi >= len {
            break;
        }
    }
    None
}

fn push_unique_candidate(candidates: &mut Vec<SnippetCandidate>, candidate: SnippetCandidate) {
    if !candidate.snippet.trim().is_empty()
        && !candidates
            .iter()
            .any(|existing: &SnippetCandidate| existing.snippet == candidate.snippet)
    {
        candidates.push(candidate);
    }
}

fn best_snippet_candidate(
    candidates: &[SnippetCandidate],
    symbol: &str,
    kind: NodeKind,
    preferred_line: usize,
) -> Option<SnippetCandidate> {
    candidates
        .iter()
        .max_by_key(|candidate| LineIndex::score_candidate(candidate, symbol, kind, preferred_line))
        .map(|candidate| SnippetCandidate {
            snippet: candidate.snippet.clone(),
            anchor_line: candidate.anchor_line,
        })
}

fn is_sufficient_snippet_match(
    best_score: &(usize, usize, usize, usize, usize),
    kind: NodeKind,
) -> bool {
    match kind {
        NodeKind::Function => best_score.3 > 0 && (best_score.0 > 0 || best_score.1 > 0),
        _ => best_score.0 > 0 || best_score.1 > 0,
    }
}

fn declaration_matches_symbol(line: &str, symbol: &str, kind: NodeKind) -> bool {
    if !line.contains(symbol) {
        return false;
    }

    match kind {
        NodeKind::Function => {
            line.contains("func ")
                || line.contains("init(")
                || line.contains("subscript")
                || line.contains("var ")
        }
        NodeKind::Class => line.contains("class "),
        NodeKind::Property | NodeKind::Field | NodeKind::Constant => {
            line.contains("var ") || line.contains("let ")
        }
        NodeKind::Struct => line.contains("struct "),
        NodeKind::Trait => line.contains("trait "),
        NodeKind::Impl => line.contains("impl "),
        NodeKind::Enum => line.contains("enum "),
        NodeKind::Protocol => line.contains("protocol "),
        NodeKind::Extension => line.contains("extension "),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::{LineIndex, should_extract_snippet, trim_snippet_indentation};
    use grapha_core::graph::{NodeKind, Span};

    #[test]
    fn extract_snippet_truncates_single_line_at_utf8_boundary() {
        let source = "abc中def";
        let index = LineIndex::new(source);
        let span = Span {
            start: [0, 0],
            end: [0, 0],
        };

        assert_eq!(index.extract_snippet(&span, 4), Some("abc".to_string()));
    }

    #[test]
    fn extract_snippet_truncates_multiline_at_newline_before_utf8_cutoff() {
        let source = "alpha\n中文beta";
        let index = LineIndex::new(source);
        let span = Span {
            start: [0, 0],
            end: [1, 0],
        };

        assert_eq!(index.extract_snippet(&span, 8), Some("alpha".to_string()));
    }

    #[test]
    fn extract_full_snippet_uses_exact_columns() {
        let source = "let before = 1; fn hello() {\n    println!(\"hi\");\n} let after = 2;";
        let index = LineIndex::new(source);
        let span = Span {
            start: [0, 16],
            end: [2, 1],
        };

        assert_eq!(
            index.extract_full_snippet(&span),
            Some("fn hello() {\n    println!(\"hi\");\n}".to_string())
        );
    }

    #[test]
    fn extract_full_snippet_accepts_one_based_lines_as_fallback() {
        let source = "struct A {}\nfn greet() {\n    println!(\"hi\");\n}\n";
        let index = LineIndex::new(source);
        let span = Span {
            start: [2, 0],
            end: [4, 1],
        };

        assert_eq!(
            index.extract_full_snippet(&span),
            Some("fn greet() {\n    println!(\"hi\");\n}".to_string())
        );
    }

    #[test]
    fn extract_symbol_snippet_prefers_candidate_that_contains_symbol_name() {
        let source = "@Published private(set) var homeEffect: UserHomeDynamicInfo?\n@Published private(set) var hasInBlackList: Bool = false\n";
        let index = LineIndex::new(source);
        let span = Span {
            start: [1, 0],
            end: [1, 54],
        };

        assert_eq!(
            index.extract_symbol_snippet(&span, "homeEffect", NodeKind::Property),
            Some("@Published private(set) var homeEffect: UserHomeDynamicInfo?".to_string())
        );
    }

    #[test]
    fn extract_symbol_snippet_falls_back_to_matching_declaration_line() {
        let source = "@Published private(set) var remarkName: String = \"\"\n";
        let index = LineIndex::new(source);
        let span = Span {
            start: [35, 0],
            end: [35, 0],
        };

        assert_eq!(
            index.extract_symbol_snippet(&span, "remarkName", NodeKind::Property),
            Some("@Published private(set) var remarkName: String = \"\"".to_string())
        );
    }

    #[test]
    fn extract_symbol_snippet_handles_enum_variants() {
        let source = "public enum ActivityTaskCode: Int {\n    case newUserRoomTask = 11\n}\n";
        let index = LineIndex::new(source);
        let span = Span {
            start: [2, 10],
            end: [2, 10],
        };

        assert!(should_extract_snippet(NodeKind::Variant));
        assert_eq!(
            index.extract_symbol_snippet(&span, "newUserRoomTask", NodeKind::Variant),
            Some("case newUserRoomTask = 11".to_string())
        );
    }

    #[test]
    fn extract_symbol_snippet_recovers_full_function_block_from_source() {
        let source = "    @inline(__always) private func requestGetUser(\n        _ data: SingleUserRequest\n    ) async throws(RequestError) -> UserInfo {\n        try await request(\n            \"user/getUserInfoByUid/\\\\(data.id)\",\n            data: [\"attrs\": data.attrs.map(\\\\.rawValue).sorted()]\n        )\n    }\n";
        let index = LineIndex::new(source);
        let span = Span {
            start: [0, 4],
            end: [0, 39],
        };

        assert_eq!(
            index.extract_symbol_snippet(&span, "requestGetUser(_:)", NodeKind::Function),
            Some(
                "@inline(__always) private func requestGetUser(\n    _ data: SingleUserRequest\n) async throws(RequestError) -> UserInfo {\n    try await request(\n        \"user/getUserInfoByUid/\\\\(data.id)\",\n        data: [\"attrs\": data.attrs.map(\\\\.rawValue).sorted()]\n    )\n}"
                    .to_string()
            )
        );
    }

    #[test]
    fn extract_symbol_snippet_prefers_nearest_matching_function_block() {
        let source = "extension Worker {\n    func load() {\n        first()\n    }\n}\n\nextension Worker {\n    func load() {\n        second()\n    }\n}\n";
        let index = LineIndex::new(source);
        let span = Span {
            start: [7, 4],
            end: [9, 5],
        };

        assert_eq!(
            index.extract_symbol_snippet(&span, "load()", NodeKind::Function),
            Some("func load() {\n    second()\n}".to_string())
        );
    }

    #[test]
    fn extract_symbol_snippet_skips_declaration_scan_when_exact_function_block_already_matches() {
        let source = "struct Worker {\n    func load() {\n        second()\n    }\n}\n";
        let index = LineIndex::new(source);
        let span = Span {
            start: [1, 4],
            end: [3, 5],
        };

        LineIndex::reset_declaration_block_scan_count();

        assert_eq!(
            index.extract_symbol_snippet(&span, "load()", NodeKind::Function),
            Some("func load() {\n    second()\n}".to_string())
        );
        assert_eq!(LineIndex::declaration_block_scan_count(), 0);
    }

    #[test]
    fn extract_symbol_snippet_uses_declaration_scan_when_span_only_covers_signature() {
        let source = "    @inline(__always) private func requestGetUser(\n        _ data: SingleUserRequest\n    ) async throws(RequestError) -> UserInfo {\n        try await request(\n            \"user/getUserInfoByUid/\\\\(data.id)\",\n            data: [\"attrs\": data.attrs.map(\\\\.rawValue).sorted()]\n        )\n    }\n";
        let index = LineIndex::new(source);
        let span = Span {
            start: [0, 4],
            end: [0, 39],
        };

        LineIndex::reset_declaration_block_scan_count();

        assert_eq!(
            index.extract_symbol_snippet(&span, "requestGetUser(_:)", NodeKind::Function),
            Some(
                "@inline(__always) private func requestGetUser(\n    _ data: SingleUserRequest\n) async throws(RequestError) -> UserInfo {\n    try await request(\n        \"user/getUserInfoByUid/\\\\(data.id)\",\n        data: [\"attrs\": data.attrs.map(\\\\.rawValue).sorted()]\n    )\n}"
                    .to_string()
            )
        );
        assert_eq!(LineIndex::declaration_block_scan_count(), 1);
    }

    #[test]
    fn trim_snippet_indentation_removes_shared_leading_spaces() {
        assert_eq!(
            trim_snippet_indentation("    func demo() {\n        work()\n    }"),
            "func demo() {\n    work()\n}".to_string()
        );
    }
}
