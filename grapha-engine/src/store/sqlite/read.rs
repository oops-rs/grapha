use std::{collections::HashMap, path::PathBuf};

use rusqlite::{Connection, params};

use grapha_core::graph::{EdgeKind, Graph, Node, NodeKind, NodeRole, Span, Visibility};

use super::{SqliteStore, compat, schema, str_to_enum};

pub(super) fn load_filtered(
    store: &SqliteStore,
    edge_kinds: Option<&[EdgeKind]>,
    metadata_key_prefix: Option<&str>,
) -> anyhow::Result<Graph> {
    let conn = store.open()?;
    load_filtered_from_connection(&conn, edge_kinds, metadata_key_prefix, true)
}

pub(super) fn load_filtered_read_only(
    store: &SqliteStore,
    edge_kinds: Option<&[EdgeKind]>,
    metadata_key_prefix: Option<&str>,
) -> anyhow::Result<Graph> {
    let conn = store.open_read_only()?;
    load_filtered_from_connection(&conn, edge_kinds, metadata_key_prefix, false)
}

fn load_filtered_from_connection(
    conn: &Connection,
    edge_kinds: Option<&[EdgeKind]>,
    metadata_key_prefix: Option<&str>,
    create_missing_tables: bool,
) -> anyhow::Result<Graph> {
    let schema_version = schema::schema_version(conn)?;
    if create_missing_tables {
        schema::create_tables(conn)?;
    }

    let version = load_version(conn)?;
    let nodes = load_nodes(conn, schema_version.as_deref(), metadata_key_prefix)?;
    let (edge_where, edge_kind_params) = build_edge_where(edge_kinds);
    let edges = compat::load_edges(
        conn,
        schema_version.as_deref(),
        &edge_where,
        &edge_kind_params,
    )?;

    Ok(Graph {
        version,
        nodes,
        edges,
    })
}

pub(super) fn load_version(conn: &Connection) -> anyhow::Result<String> {
    Ok(conn
        .query_row("SELECT value FROM meta WHERE key = 'version'", [], |row| {
            row.get(0)
        })
        .unwrap_or_else(|_| "0.1.0".to_string()))
}

pub(super) fn load_nodes(
    conn: &Connection,
    schema_version: Option<&str>,
    metadata_key_prefix: Option<&str>,
) -> anyhow::Result<Vec<Node>> {
    let mut nodes = Vec::new();
    let repo_expr = if schema_version == Some(schema::STORE_SCHEMA_VERSION) {
        "repo"
    } else {
        "NULL"
    };

    if let Some(prefix) = metadata_key_prefix {
        let mut stmt = conn.prepare(&format!(
            "SELECT id, kind, name, file,
                    span_start_line, span_start_col, span_end_line, span_end_col,
                    visibility,
                    CASE WHEN instr(metadata, ?1) > 0 THEN metadata ELSE '{{}}' END,
                    role, NULL, NULL, module, NULL, {repo_expr}
             FROM nodes",
        ))?;
        let mut rows = stmt.query(params![prefix])?;
        while let Some(row) = rows.next()? {
            nodes.push(decode_node_row(row)?);
        }
    } else {
        let mut stmt = conn.prepare(&format!(
            "SELECT id, kind, name, file,
                    span_start_line, span_start_col, span_end_line, span_end_col,
                    visibility, metadata, role, signature, doc_comment, module, snippet, {repo_expr}
             FROM nodes",
        ))?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            nodes.push(decode_node_row(row)?);
        }
    }

    Ok(nodes)
}

pub(super) fn build_edge_where(edge_kinds: Option<&[EdgeKind]>) -> (String, Vec<&'static str>) {
    let Some(edge_kinds) = edge_kinds.filter(|kinds| !kinds.is_empty()) else {
        return (String::new(), Vec::new());
    };

    let placeholders = (1..=edge_kinds.len())
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let params = edge_kinds
        .iter()
        .map(super::edge_kind_str)
        .collect::<Vec<_>>();
    (format!(" WHERE kind IN ({placeholders})"), params)
}

fn decode_node_row(row: &rusqlite::Row<'_>) -> anyhow::Result<Node> {
    let kind_str: String = row.get(1)?;
    let kind: NodeKind = str_to_enum(&kind_str)
        .map_err(|e| anyhow::anyhow!("invalid node kind '{kind_str}': {e}"))?;
    let vis_str: String = row.get(8)?;
    let visibility: Visibility = str_to_enum(&vis_str)
        .map_err(|e| anyhow::anyhow!("invalid visibility '{vis_str}': {e}"))?;
    let meta_str: String = row.get(9)?;
    let metadata: HashMap<String, String> = serde_json::from_str(&meta_str)?;
    let role: Option<NodeRole> = row
        .get::<_, Option<String>>(10)?
        .map(|s| serde_json::from_str(&s))
        .transpose()
        .map_err(|e| anyhow::anyhow!("invalid node role: {e}"))?;

    Ok(Node {
        id: row.get(0)?,
        kind,
        name: row.get(2)?,
        file: PathBuf::from(row.get::<_, String>(3)?),
        span: Span {
            start: [
                row.get::<_, i64>(4)? as usize,
                row.get::<_, i64>(5)? as usize,
            ],
            end: [
                row.get::<_, i64>(6)? as usize,
                row.get::<_, i64>(7)? as usize,
            ],
        },
        visibility,
        metadata,
        role,
        signature: row.get(11)?,
        doc_comment: row.get(12)?,
        module: row.get(13)?,
        snippet: row.get(14)?,
        repo: row.get(15)?,
    })
}
