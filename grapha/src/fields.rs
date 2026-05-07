/// Controls which optional fields appear in CLI output.
///
/// Fields can be selected via `--fields` flag (comma-separated) or configured
/// in `grapha.toml` under `[output] default_fields`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldSet {
    pub score: bool,
    pub file: bool,
    pub id: bool,
    pub locator: bool,
    pub module: bool,
    pub repo: bool,
    pub span: bool,
    pub snippet: bool,
    pub visibility: bool,
    pub signature: bool,
    pub doc_comment: bool,
    pub annotation: bool,
    pub role: bool,
}

impl Default for FieldSet {
    fn default() -> Self {
        Self {
            score: false,
            file: true,
            id: false,
            locator: false,
            module: false,
            repo: false,
            span: false,
            snippet: false,
            visibility: false,
            signature: false,
            doc_comment: false,
            annotation: false,
            role: false,
        }
    }
}

impl FieldSet {
    pub fn with_id(mut self) -> Self {
        self.id = true;
        self
    }

    pub fn with_locator(mut self) -> Self {
        self.locator = true;
        self
    }

    pub fn with_annotation(mut self) -> Self {
        self.annotation = true;
        self
    }

    pub fn with_doc_comment(mut self) -> Self {
        self.doc_comment = true;
        self
    }

    pub fn without_file(mut self) -> Self {
        self.file = false;
        self
    }

    pub fn without_span(mut self) -> Self {
        self.span = false;
        self
    }

    pub fn all() -> Self {
        Self {
            score: true,
            file: true,
            id: true,
            locator: true,
            module: true,
            repo: true,
            span: true,
            snippet: true,
            visibility: true,
            signature: true,
            doc_comment: true,
            annotation: true,
            role: true,
        }
    }

    pub fn none() -> Self {
        Self {
            score: false,
            file: false,
            id: false,
            locator: false,
            module: false,
            repo: false,
            span: false,
            snippet: false,
            visibility: false,
            signature: false,
            doc_comment: false,
            annotation: false,
            role: false,
        }
    }

    pub fn parse(input: &str) -> Self {
        match input.trim() {
            "all" | "full" => Self::all(),
            "none" => Self::none(),
            s => {
                let mut fs = Self::none();
                for field in s.split(',') {
                    match field.trim() {
                        "score" => fs.score = true,
                        "file" => fs.file = true,
                        "id" => fs.id = true,
                        "locator" => fs.locator = true,
                        "module" => fs.module = true,
                        "repo" => fs.repo = true,
                        "span" => fs.span = true,
                        "snippet" => fs.snippet = true,
                        "visibility" => fs.visibility = true,
                        "signature" => fs.signature = true,
                        "doc_comment" => fs.doc_comment = true,
                        "annotation" => fs.annotation = true,
                        "role" => fs.role = true,
                        _ => {}
                    }
                }
                fs
            }
        }
    }

    pub fn from_config(fields: &[String]) -> Self {
        Self::parse(&fields.join(","))
    }
}
