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

    let parsed = match crate::cargo_manifest::read_manifest_table(&cargo_toml) {
        Ok(table) => table,
        Err(_) => return ModuleMap::new(),
    };

    let mut modules = ModuleMap::new();
    let workspace_members = crate::cargo_manifest::workspace_member_paths_from_table(root, &parsed);
    if !workspace_members.is_empty() {
        for member_path in workspace_members {
            if member_path.is_dir() {
                add_cargo_member(&member_path, &mut modules);
            }
        }
    } else {
        let name = crate::cargo_manifest::package_name_from_table(&parsed, root);
        modules
            .modules
            .entry(name)
            .or_default()
            .push(root.to_path_buf());
    }

    modules
}

fn add_cargo_member(member_path: &Path, modules: &mut ModuleMap) {
    let manifest = member_path.join("Cargo.toml");
    let name = crate::cargo_manifest::read_manifest_table(&manifest)
        .ok()
        .map(|table| crate::cargo_manifest::package_name_from_table(&table, member_path))
        .unwrap_or_else(|| {
            member_path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("unknown")
                .to_string()
        });
    modules
        .modules
        .entry(name)
        .or_default()
        .push(member_path.to_path_buf());
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
        assert_eq!(
            modules
                .module_for_file(&dir.path().join("tests/cli.rs"))
                .as_deref(),
            Some("demo")
        );
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
        assert_eq!(
            modules
                .module_for_file(&dir.path().join("crates/one/tests/cli.rs"))
                .as_deref(),
            Some("one")
        );
        assert_eq!(
            modules
                .module_for_file(&dir.path().join("crates/two/examples/demo.rs"))
                .as_deref(),
            Some("two")
        );
    }
}
