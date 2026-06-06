use rusqlite::{Connection, params_from_iter};

use crate::store::sqlite::{
    BINARY_PROVENANCE_SCHEMA_VERSION, STORE_SCHEMA_VERSION, edge_kind_str, str_to_enum,
};
use grapha_core::graph::{Edge, EdgeKind, EdgeProvenance};

pub(super) fn serialize_provenance(provenance: &[EdgeProvenance]) -> anyhow::Result<Vec<u8>> {
    if provenance.is_empty() {
        return Ok(Vec::new());
    }
    Ok(bincode::serde::encode_to_vec(
        provenance,
        bincode::config::legacy(),
    )?)
}

fn deserialize_provenance(blob: &[u8]) -> anyhow::Result<Vec<EdgeProvenance>> {
    if blob.is_empty() {
        return Ok(Vec::new());
    }
    Ok(bincode::serde::decode_from_slice(blob, bincode::config::legacy())?.0)
}

pub(super) fn load_edges(
    conn: &Connection,
    schema_version: Option<&str>,
    edge_where: &str,
    edge_kind_params: &[&'static str],
) -> anyhow::Result<Vec<Edge>> {
    if schema_version == Some(STORE_SCHEMA_VERSION) {
        let sql = format!(
            "SELECT source, target, kind, confidence,
                    direction, operation, condition, async_boundary, provenance, repo
             FROM edges{edge_where}"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = if edge_kind_params.is_empty() {
            stmt.query([])?
        } else {
            stmt.query(params_from_iter(edge_kind_params.iter().copied()))?
        };

        let mut edges = Vec::new();
        while let Some(row) = rows.next()? {
            let kind_str: String = row.get(2)?;
            let kind: EdgeKind = str_to_enum(&kind_str)
                .map_err(|e| anyhow::anyhow!("invalid edge kind '{kind_str}': {e}"))?;
            let direction = row
                .get::<_, Option<String>>(4)?
                .map(|s| str_to_enum(&s))
                .transpose()
                .map_err(|e| anyhow::anyhow!("invalid flow direction: {e}"))?;
            edges.push(Edge {
                source: row.get(0)?,
                target: row.get(1)?,
                kind,
                confidence: row.get(3)?,
                direction,
                operation: row.get(5)?,
                condition: row.get(6)?,
                async_boundary: row.get::<_, Option<i64>>(7)?.map(|v| v != 0),
                provenance: deserialize_provenance(&row.get::<_, Vec<u8>>(8)?)?,
                repo: row.get(9)?,
            });
        }
        return Ok(edges);
    }

    if matches!(
        schema_version,
        Some("6") | Some(BINARY_PROVENANCE_SCHEMA_VERSION)
    ) {
        let sql = format!(
            "SELECT source, target, kind, confidence,
                    direction, operation, condition, async_boundary, provenance
             FROM edges{edge_where}"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = if edge_kind_params.is_empty() {
            stmt.query([])?
        } else {
            stmt.query(params_from_iter(edge_kind_params.iter().copied()))?
        };

        let mut edges = Vec::new();
        while let Some(row) = rows.next()? {
            let kind_str: String = row.get(2)?;
            let kind: EdgeKind = str_to_enum(&kind_str)
                .map_err(|e| anyhow::anyhow!("invalid edge kind '{kind_str}': {e}"))?;
            let direction = row
                .get::<_, Option<String>>(4)?
                .map(|s| str_to_enum(&s))
                .transpose()
                .map_err(|e| anyhow::anyhow!("invalid flow direction: {e}"))?;
            edges.push(Edge {
                source: row.get(0)?,
                target: row.get(1)?,
                kind,
                confidence: row.get(3)?,
                direction,
                operation: row.get(5)?,
                condition: row.get(6)?,
                async_boundary: row.get::<_, Option<i64>>(7)?.map(|v| v != 0),
                provenance: deserialize_provenance(&row.get::<_, Vec<u8>>(8)?)?,
                repo: None,
            });
        }
        return Ok(edges);
    }

    if schema_version == Some("4") {
        let sql = format!(
            "SELECT source, target, kind, confidence,
                    direction, operation, condition, async_boundary, provenance
             FROM edges{edge_where}"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = if edge_kind_params.is_empty() {
            stmt.query([])?
        } else {
            stmt.query(params_from_iter(edge_kind_params.iter().copied()))?
        };

        let mut edges = Vec::new();
        while let Some(row) = rows.next()? {
            let kind_str: String = row.get(2)?;
            let kind: EdgeKind = str_to_enum(&kind_str)
                .map_err(|e| anyhow::anyhow!("invalid edge kind '{kind_str}': {e}"))?;
            let direction = row
                .get::<_, Option<String>>(4)?
                .map(|s| str_to_enum(&s))
                .transpose()
                .map_err(|e| anyhow::anyhow!("invalid flow direction: {e}"))?;
            edges.push(Edge {
                source: row.get(0)?,
                target: row.get(1)?,
                kind,
                confidence: row.get(3)?,
                direction,
                operation: row.get(5)?,
                condition: row.get(6)?,
                async_boundary: row.get::<_, Option<i64>>(7)?.map(|v| v != 0),
                provenance: serde_json::from_str(&row.get::<_, String>(8)?)?,
                repo: None,
            });
        }
        return Ok(edges);
    }

    let sql = format!(
        "SELECT source, target, kind, confidence,
                direction, operation, condition, async_boundary
         FROM edges{edge_where}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = if edge_kind_params.is_empty() {
        stmt.query([])?
    } else {
        stmt.query(params_from_iter(edge_kind_params.iter().copied()))?
    };

    let mut edges = Vec::new();
    while let Some(row) = rows.next()? {
        let kind_str: String = row.get(2)?;
        let kind: EdgeKind = str_to_enum(&kind_str)
            .map_err(|e| anyhow::anyhow!("invalid edge kind '{kind_str}': {e}"))?;
        let direction = row
            .get::<_, Option<String>>(4)?
            .map(|s| str_to_enum(&s))
            .transpose()
            .map_err(|e| anyhow::anyhow!("invalid flow direction: {e}"))?;
        edges.push(Edge {
            source: row.get(0)?,
            target: row.get(1)?,
            kind,
            confidence: row.get(3)?,
            direction,
            operation: row.get(5)?,
            condition: row.get(6)?,
            async_boundary: row.get::<_, Option<i64>>(7)?.map(|v| v != 0),
            provenance: Vec::new(),
            repo: None,
        });
    }
    Ok(edges)
}

#[allow(dead_code)]
fn _edge_kind_label(kind: &EdgeKind) -> &'static str {
    edge_kind_str(kind)
}
