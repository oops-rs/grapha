use std::path::Path;

use grapha_core::{
    ExtractionResult, FileContext, LanguagePlugin, LanguageRegistry, ModuleMap, ProjectContext,
    SemanticDocument,
};

use crate::classify::rust::terminal_effect_for_target;
use crate::extract::rust::RustExtractor;

pub struct RustPlugin;

impl LanguagePlugin for RustPlugin {
    fn id(&self) -> &'static str {
        "rust"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["rs"]
    }

    fn discover_modules(&self, context: &ProjectContext) -> anyhow::Result<ModuleMap> {
        Ok(discover_cargo_modules(&context.project_root))
    }

    fn extract(&self, source: &[u8], context: &FileContext) -> anyhow::Result<ExtractionResult> {
        let extractor = RustExtractor;
        use grapha_core::LanguageExtractor;
        extractor.extract(source, &context.relative_path)
    }

    fn extract_semantics(
        &self,
        source: &[u8],
        context: &FileContext,
    ) -> anyhow::Result<SemanticDocument> {
        let mut document = SemanticDocument::from_extraction_result(self.extract(source, context)?);
        document.annotate_call_relations(|relation, _source| {
            terminal_effect_for_target(relation.target.as_raw())
        });
        Ok(document)
    }
}

pub fn register_builtin(registry: &mut LanguageRegistry) -> anyhow::Result<()> {
    registry.register(RustPlugin)
}

fn discover_cargo_modules(root: &Path) -> ModuleMap {
    let cargo_toml = root.join("Cargo.toml");
    if !cargo_toml.is_file() {
        return ModuleMap::new();
    }

    let content = match std::fs::read_to_string(&cargo_toml) {
        Ok(content) => content,
        Err(_) => return ModuleMap::new(),
    };
    let parsed = match content.parse::<toml::Table>() {
        Ok(table) => table,
        Err(_) => return ModuleMap::new(),
    };

    let mut modules = ModuleMap::new();
    if let Some(workspace) = parsed
        .get("workspace")
        .and_then(|workspace| workspace.as_table())
    {
        if let Some(members) = workspace
            .get("members")
            .and_then(|members| members.as_array())
        {
            for member in members {
                if let Some(pattern) = member.as_str() {
                    expand_workspace_member(root, pattern, &mut modules);
                }
            }
        }
    } else {
        let name = parsed
            .get("package")
            .and_then(|package| package.as_table())
            .and_then(|package| package.get("name"))
            .and_then(|name| name.as_str())
            .or_else(|| root.file_name().and_then(|name| name.to_str()))
            .unwrap_or("root")
            .to_string();
        let src_dir = root.join("src");
        let source_dir = if src_dir.is_dir() {
            src_dir
        } else {
            root.to_path_buf()
        };
        modules.modules.entry(name).or_default().push(source_dir);
    }

    modules
}

fn expand_workspace_member(root: &Path, pattern: &str, modules: &mut ModuleMap) {
    if pattern.contains('*') {
        let prefix = pattern.trim_end_matches('*').trim_end_matches('/');
        let parent_dir = root.join(prefix);
        if parent_dir.is_dir()
            && let Ok(entries) = std::fs::read_dir(&parent_dir)
        {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    add_cargo_member(&path, modules);
                }
            }
        }
    } else {
        let member_path = root.join(pattern);
        if member_path.is_dir() {
            add_cargo_member(&member_path, modules);
        }
    }
}

fn add_cargo_member(member_path: &Path, modules: &mut ModuleMap) {
    let name = member_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("unknown")
        .to_string();
    let src_dir = member_path.join("src");
    let source_dir = if src_dir.is_dir() {
        src_dir
    } else {
        member_path.to_path_buf()
    };
    modules.modules.entry(name).or_default().push(source_dir);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn discovers_single_cargo_package() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            r#"
[package]
name = "demo"
"#,
        )
        .unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();

        let modules = discover_cargo_modules(dir.path());
        assert!(modules.modules.contains_key("demo"));
    }

    #[test]
    fn discovers_workspace_members() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("crates/one/src")).unwrap();
        fs::create_dir_all(dir.path().join("crates/two/src")).unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            r#"
[workspace]
members = ["crates/*"]
"#,
        )
        .unwrap();

        let modules = discover_cargo_modules(dir.path());
        assert!(modules.modules.contains_key("one"));
        assert!(modules.modules.contains_key("two"));
    }
}
