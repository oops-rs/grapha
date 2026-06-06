use rusqlite::{Connection, OptionalExtension, Transaction};

use grapha_core::graph::Graph;

pub(super) const STORE_SCHEMA_VERSION: &str = "7";
pub(super) const BINARY_PROVENANCE_SCHEMA_VERSION: &str = "5";

pub(super) fn create_tables(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS meta (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS nodes (
            id         TEXT PRIMARY KEY,
            kind       TEXT NOT NULL,
            name       TEXT NOT NULL,
            file       TEXT NOT NULL,
            span_start_line   INTEGER NOT NULL,
            span_start_col    INTEGER NOT NULL,
            span_end_line     INTEGER NOT NULL,
            span_end_col      INTEGER NOT NULL,
            visibility TEXT NOT NULL,
            metadata   TEXT NOT NULL,
            role       TEXT,
            signature  TEXT,
            doc_comment TEXT,
            module     TEXT,
            snippet    TEXT,
            repo       TEXT
        );
        CREATE TABLE IF NOT EXISTS edges (
            edge_id    TEXT PRIMARY KEY,
            source     TEXT NOT NULL,
            target     TEXT NOT NULL,
            kind       TEXT NOT NULL,
            confidence REAL NOT NULL,
            direction  TEXT,
            operation  TEXT,
            condition  TEXT,
            async_boundary INTEGER,
            provenance BLOB NOT NULL,
            repo       TEXT
        );",
    )?;
    Ok(())
}

pub(super) fn schema_version(conn: &Connection) -> anyhow::Result<Option<String>> {
    let has_meta = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'meta'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !has_meta {
        return Ok(None);
    }

    Ok(conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'store_schema_version'",
            [],
            |row| row.get(0),
        )
        .optional()?)
}

pub(super) fn write_meta(tx: &Transaction<'_>, graph: &Graph) -> anyhow::Result<()> {
    tx.execute(
        "INSERT INTO meta (key, value) VALUES ('version', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [&graph.version],
    )?;
    tx.execute(
        "INSERT INTO meta (key, value) VALUES ('store_schema_version', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [STORE_SCHEMA_VERSION],
    )?;
    Ok(())
}

pub(super) fn drop_tables(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "DROP TABLE IF EXISTS edges;
         DROP TABLE IF EXISTS nodes;
         DROP TABLE IF EXISTS meta;",
    )?;
    Ok(())
}

pub(super) fn create_bulk_load_tables(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "CREATE TABLE meta (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        CREATE TABLE nodes (
            id         TEXT NOT NULL,
            kind       TEXT NOT NULL,
            name       TEXT NOT NULL,
            file       TEXT NOT NULL,
            span_start_line   INTEGER NOT NULL,
            span_start_col    INTEGER NOT NULL,
            span_end_line     INTEGER NOT NULL,
            span_end_col      INTEGER NOT NULL,
            visibility TEXT NOT NULL,
            metadata   TEXT NOT NULL,
            role       TEXT,
            signature  TEXT,
            doc_comment TEXT,
            module     TEXT,
            snippet    TEXT,
            repo       TEXT
        );
        CREATE TABLE edges (
            edge_id    TEXT NOT NULL,
            source     TEXT NOT NULL,
            target     TEXT NOT NULL,
            kind       TEXT NOT NULL,
            confidence REAL NOT NULL,
            direction  TEXT,
            operation  TEXT,
            condition  TEXT,
            async_boundary INTEGER,
            provenance BLOB NOT NULL,
            repo       TEXT
        );",
    )?;
    Ok(())
}

pub(super) fn create_bulk_load_indexes(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "CREATE UNIQUE INDEX idx_nodes_id ON nodes(id);
         CREATE UNIQUE INDEX idx_edges_id ON edges(edge_id);",
    )?;
    Ok(())
}
