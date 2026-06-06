use std::collections::HashMap;

use grapha_core::graph::{Graph, Node, NodeKind, Span, Visibility};
use grapha_engine::config::GraphaConfig;
use grapha_engine::index::GraphaIndexHandle;
use grapha_engine::index_status;
use grapha_engine::search;
use grapha_engine::store::Store;
use grapha_engine::store::sqlite::SqliteStore;

fn test_graph() -> Graph {
    Graph {
        version: "0.1.0".to_string(),
        nodes: vec![Node {
            id: "main".to_string(),
            kind: NodeKind::Function,
            name: "main".to_string(),
            file: "src/main.rs".into(),
            span: Span {
                start: [1, 0],
                end: [1, 9],
            },
            visibility: Visibility::Public,
            metadata: HashMap::new(),
            role: None,
            signature: Some("fn main()".to_string()),
            doc_comment: None,
            module: Some("demo".to_string()),
            snippet: Some("fn main() {}".to_string()),
            repo: None,
        }],
        edges: Vec::new(),
    }
}

#[test]
fn read_only_handle_opens_real_index_and_searches_symbols() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let project_root = temp.path();
    let store_dir = project_root.join(".grapha");
    std::fs::create_dir_all(&store_dir)?;

    let graph = test_graph();
    let store = SqliteStore::new(store_dir.join("grapha.db"));
    store.save(&graph)?;
    search::build_index(&graph, &store_dir.join("search_index"))?;
    index_status::save_index_status(
        project_root,
        &store_dir,
        graph.nodes.len(),
        graph.edges.len(),
        &GraphaConfig::default(),
    )?;

    let handle = GraphaIndexHandle::open_read_only(project_root)?;
    let results = handle.search_symbols("main", 10)?;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, "main");

    drop(handle);
    Ok(())
}
