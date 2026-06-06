use grapha_core::graph::Graph;
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};

#[derive(Debug, Serialize)]
pub struct DirectoryGroup {
    pub directory: String,
    pub file_count: usize,
    pub symbol_count: usize,
}

pub fn file_map(
    graph: &Graph,
    module_filter: Option<&str>,
) -> BTreeMap<String, Vec<DirectoryGroup>> {
    // module_name -> directory -> (unique_files, symbol_count)
    let mut module_dirs: BTreeMap<String, BTreeMap<String, (HashSet<String>, usize)>> =
        BTreeMap::new();

    for node in &graph.nodes {
        let module = node.module.as_deref().unwrap_or("(unknown)").to_string();

        if let Some(filter) = module_filter
            && !module.eq_ignore_ascii_case(filter)
        {
            continue;
        }

        let file_path = node.file.to_string_lossy();
        let directory = match file_path.rfind('/') {
            Some(pos) => file_path[..=pos].to_string(),
            None => String::new(),
        };

        let entry = module_dirs
            .entry(module)
            .or_default()
            .entry(directory)
            .or_insert_with(|| (HashSet::new(), 0));

        entry.0.insert(file_path.to_string());
        entry.1 += 1;
    }

    module_dirs
        .into_iter()
        .map(|(module, dirs)| {
            let mut groups: Vec<DirectoryGroup> = dirs
                .into_iter()
                .map(|(directory, (files, symbol_count))| DirectoryGroup {
                    directory,
                    file_count: files.len(),
                    symbol_count,
                })
                .collect();
            groups.sort_by_key(|group| std::cmp::Reverse(group.symbol_count));
            (module, groups)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapha_core::graph::{Node, NodeKind, Span, Visibility};
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn make_node(name: &str, file: &str, module: Option<&str>) -> Node {
        Node {
            id: format!("{}::{}", file, name),
            kind: NodeKind::Function,
            name: name.to_string(),
            file: PathBuf::from(file),
            span: Span {
                start: [0, 0],
                end: [1, 0],
            },
            visibility: Visibility::Public,
            metadata: HashMap::new(),
            role: None,
            signature: None,
            doc_comment: None,
            module: module.map(String::from),
            snippet: None,
            repo: None,
        }
    }

    fn make_graph(nodes: Vec<Node>) -> Graph {
        Graph {
            version: "0.1.0".to_string(),
            nodes,
            edges: Vec::new(),
        }
    }

    #[test]
    fn groups_by_module_and_directory() {
        let graph = make_graph(vec![
            make_node("foo", "src/core/foo.rs", Some("core")),
            make_node("bar", "src/core/bar.rs", Some("core")),
            make_node("baz", "src/cli/baz.rs", Some("cli")),
        ]);

        let map = file_map(&graph, None);

        assert_eq!(map.len(), 2);
        assert!(map.contains_key("core"));
        assert!(map.contains_key("cli"));

        let core_groups = &map["core"];
        assert_eq!(core_groups.len(), 1);
        assert_eq!(core_groups[0].directory, "src/core/");
        assert_eq!(core_groups[0].file_count, 2);
        assert_eq!(core_groups[0].symbol_count, 2);
    }

    #[test]
    fn counts_unique_files_and_total_symbols() {
        let graph = make_graph(vec![
            make_node("func_a", "src/lib.rs", Some("mymod")),
            make_node("func_b", "src/lib.rs", Some("mymod")),
            make_node("func_c", "src/util.rs", Some("mymod")),
        ]);

        let map = file_map(&graph, None);
        let groups = &map["mymod"];

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].directory, "src/");
        assert_eq!(groups[0].file_count, 2);
        assert_eq!(groups[0].symbol_count, 3);
    }

    #[test]
    fn filters_by_module() {
        let graph = make_graph(vec![
            make_node("foo", "src/a.rs", Some("alpha")),
            make_node("bar", "src/b.rs", Some("beta")),
        ]);

        let map = file_map(&graph, Some("alpha"));

        assert_eq!(map.len(), 1);
        assert!(map.contains_key("alpha"));
        assert!(!map.contains_key("beta"));
    }

    #[test]
    fn unknown_module_for_nodes_without_module() {
        let graph = make_graph(vec![make_node("orphan", "src/orphan.rs", None)]);

        let map = file_map(&graph, None);

        assert!(map.contains_key("(unknown)"));
        assert_eq!(map["(unknown)"][0].symbol_count, 1);
    }

    #[test]
    fn sorts_groups_by_symbol_count_descending() {
        let graph = make_graph(vec![
            make_node("a", "src/small/a.rs", Some("mod")),
            make_node("b", "src/big/b.rs", Some("mod")),
            make_node("c", "src/big/c.rs", Some("mod")),
            make_node("d", "src/big/d.rs", Some("mod")),
        ]);

        let map = file_map(&graph, None);
        let groups = &map["mod"];

        assert_eq!(groups[0].directory, "src/big/");
        assert_eq!(groups[0].symbol_count, 3);
        assert_eq!(groups[1].directory, "src/small/");
        assert_eq!(groups[1].symbol_count, 1);
    }

    #[test]
    fn file_without_slash_uses_empty_directory() {
        let graph = make_graph(vec![make_node("main", "main.rs", Some("root"))]);

        let map = file_map(&graph, None);
        let groups = &map["root"];

        assert_eq!(groups[0].directory, "");
        assert_eq!(groups[0].file_count, 1);
    }
}
