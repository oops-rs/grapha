use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::delta::{GraphDelta, edge_fingerprint};
use crate::store::{StoreWriteStats, sqlite::schema};
use grapha_core::graph::{Edge, Node};

use super::{
    SqliteStore, compat::serialize_provenance, edge_kind_str, flow_direction_str, node_kind_str,
    visibility_str,
};

pub(super) fn remove_existing_store_files(path: &Path) -> anyhow::Result<()> {
    for candidate in [
        path.to_path_buf(),
        PathBuf::from(format!("{}-wal", path.to_string_lossy())),
        PathBuf::from(format!("{}-shm", path.to_string_lossy())),
    ] {
        match std::fs::remove_file(&candidate) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

pub(super) fn insert_nodes(
    tx: &rusqlite::Transaction<'_>,
    nodes: &[Node],
    replace: bool,
) -> anyhow::Result<()> {
    let verb = if replace {
        "INSERT OR REPLACE"
    } else {
        "INSERT"
    };
    let sql = format!(
        "{verb} INTO nodes (id, kind, name, file,
            span_start_line, span_start_col, span_end_line, span_end_col,
            visibility, metadata, role, signature, doc_comment, module, snippet, repo)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)"
    );
    let mut stmt = tx.prepare_cached(&sql)?;
    let empty_meta = "{}".to_string();
    let mut meta_buf = String::new();
    for node in nodes {
        let role_json: Option<String> =
            node.role.as_ref().map(serde_json::to_string).transpose()?;
        let meta_ref: &str = if node.metadata.is_empty() {
            &empty_meta
        } else {
            meta_buf.clear();
            serde_json::to_writer(unsafe { meta_buf.as_mut_vec() }, &node.metadata)?;
            &meta_buf
        };
        let file_str = node.file.to_string_lossy();
        stmt.execute(rusqlite::params![
            node.id,
            node_kind_str(&node.kind),
            node.name,
            file_str.as_ref(),
            node.span.start[0] as i64,
            node.span.start[1] as i64,
            node.span.end[0] as i64,
            node.span.end[1] as i64,
            visibility_str(&node.visibility),
            meta_ref,
            role_json,
            node.signature,
            node.doc_comment,
            node.module,
            node.snippet,
            node.repo,
        ])?;
    }
    Ok(())
}

pub(super) fn insert_edges<'a>(
    tx: &rusqlite::Transaction<'_>,
    edges: impl Iterator<Item = (String, &'a Edge)>,
    replace: bool,
) -> anyhow::Result<()> {
    let verb = if replace {
        "INSERT OR REPLACE"
    } else {
        "INSERT"
    };
    let sql = format!(
        "{verb} INTO edges (edge_id, source, target, kind, confidence,
            direction, operation, condition, async_boundary, provenance, repo)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)"
    );
    let mut stmt = tx.prepare_cached(&sql)?;
    for (edge_id, edge) in edges {
        let direction_str = edge.direction.as_ref().map(flow_direction_str);
        let async_boundary_int = edge.async_boundary.map(|b| if b { 1 } else { 0 });
        let provenance = serialize_provenance(&edge.provenance)?;
        stmt.execute(rusqlite::params![
            edge_id,
            edge.source,
            edge.target,
            edge_kind_str(&edge.kind),
            edge.confidence,
            direction_str,
            edge.operation,
            edge.condition,
            async_boundary_int,
            provenance,
            edge.repo,
        ])?;
    }
    Ok(())
}

pub(super) fn save_full(
    store: &SqliteStore,
    graph: &grapha_core::graph::Graph,
) -> anyhow::Result<()> {
    remove_existing_store_files(&store.path)?;
    let conn = Connection::open(&store.path)?;
    conn.execute_batch(
        "PRAGMA journal_mode=OFF;
         PRAGMA synchronous=OFF;
         PRAGMA temp_store=MEMORY;
         PRAGMA cache_size=-64000;
         PRAGMA mmap_size=268435456;
         PRAGMA locking_mode=EXCLUSIVE;
         PRAGMA page_size=8192;",
    )?;

    schema::drop_tables(&conn)?;
    schema::create_bulk_load_tables(&conn)?;

    let tx = conn.unchecked_transaction()?;
    schema::write_meta(&tx, graph)?;
    insert_nodes(&tx, &graph.nodes, false)?;
    insert_edges(
        &tx,
        graph
            .edges
            .iter()
            .map(|edge| (edge_fingerprint(edge), edge)),
        false,
    )?;
    tx.commit()?;
    schema::create_bulk_load_indexes(&conn)?;
    conn.execute_batch("PRAGMA optimize;")?;
    Ok(())
}

pub(super) fn save_incremental(
    store: &SqliteStore,
    previous: Option<&grapha_core::graph::Graph>,
    graph: &grapha_core::graph::Graph,
) -> anyhow::Result<StoreWriteStats> {
    let conn = store.open_for_write()?;
    let schema_version = schema::schema_version(&conn)?;
    if previous.is_none() || schema_version.as_deref() != Some(schema::STORE_SCHEMA_VERSION) {
        let full_stats =
            StoreWriteStats::from_graphs(previous, graph, crate::delta::SyncMode::FullRebuild);
        drop(conn);
        save_full(store, graph)?;
        return Ok(full_stats);
    }

    let previous_graph = previous.expect("checked is_some above");
    let delta = GraphDelta::between(previous_graph, graph);

    let stats = StoreWriteStats {
        mode: crate::delta::SyncMode::Incremental,
        nodes: delta.node_stats(),
        edges: delta.edge_stats(),
    };

    if delta.is_empty() {
        return Ok(stats);
    }

    let tx = conn.unchecked_transaction()?;
    schema::write_meta(&tx, graph)?;

    {
        let mut delete_edges = tx.prepare("DELETE FROM edges WHERE edge_id = ?1")?;
        for edge_id in &delta.deleted_edge_ids {
            delete_edges.execute([edge_id])?;
        }
    }

    {
        let mut delete_nodes = tx.prepare("DELETE FROM nodes WHERE id = ?1")?;
        for node_id in &delta.deleted_node_ids {
            delete_nodes.execute([node_id])?;
        }
    }

    let mut changed_nodes = Vec::new();
    changed_nodes.extend(delta.added_nodes.iter().copied().cloned());
    changed_nodes.extend(delta.updated_nodes.iter().copied().cloned());
    insert_nodes(&tx, &changed_nodes, true)?;

    {
        let mut delete_edges = tx.prepare("DELETE FROM edges WHERE edge_id = ?1")?;
        for edge in &delta.updated_edges {
            delete_edges.execute([&edge.id])?;
        }
    }
    insert_edges(
        &tx,
        delta
            .added_edges
            .iter()
            .chain(delta.updated_edges.iter())
            .map(|edge| (edge.id.clone(), edge.edge)),
        true,
    )?;
    tx.commit()?;
    conn.execute_batch("PRAGMA optimize;")?;

    Ok(stats)
}
