use std::{fs, io, path::Path};

use anyhow::Context;
use rusqlite::Connection;

use crate::workspace::Workspace;

use super::{InitError, InitOptions, InitReporter, prompt};

pub(super) const REGISTRY_SCHEMA_SQL: &str = r#"
BEGIN;
CREATE TABLE IF NOT EXISTS tasks (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    content TEXT,
    status TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    priority INTEGER NOT NULL DEFAULT 0,
    tags TEXT,
    meta TEXT
);
CREATE TABLE IF NOT EXISTS runs (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL,
    stage TEXT NOT NULL,
    started_at TEXT NOT NULL,
    finished_at TEXT,
    model TEXT,
    profile TEXT,
    tokens_in INTEGER NOT NULL DEFAULT 0,
    tokens_out INTEGER NOT NULL DEFAULT 0,
    success INTEGER NOT NULL DEFAULT 0,
    meta TEXT,
    FOREIGN KEY(task_id) REFERENCES tasks(id)
);
CREATE TABLE IF NOT EXISTS artifacts (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    path TEXT NOT NULL,
    sha256 TEXT,
    FOREIGN KEY(run_id) REFERENCES runs(id)
);
CREATE INDEX IF NOT EXISTS idx_tasks_status_updated_at ON tasks(status, updated_at DESC);
CREATE INDEX IF NOT EXISTS idx_runs_task_stage_started_at ON runs(task_id, stage, started_at DESC);
CREATE VIRTUAL TABLE IF NOT EXISTS tasks_fts USING fts5(title, content, tokenize = 'unicode61');
COMMIT;
"#;

pub(super) const REGISTRY_TABLES: &[&str] = &["tasks", "runs", "artifacts"];

pub(super) const AUDIT_INDEX_SCHEMA_SQL: &str = r#"
BEGIN;
CREATE TABLE IF NOT EXISTS events (
    day TEXT NOT NULL,
    offset INTEGER NOT NULL,
    ts TEXT NOT NULL,
    event TEXT NOT NULL,
    task_id TEXT,
    run_id TEXT,
    PRIMARY KEY(day, offset)
);
CREATE INDEX IF NOT EXISTS idx_events_ts ON events(ts);
CREATE INDEX IF NOT EXISTS idx_events_event ON events(event);
CREATE INDEX IF NOT EXISTS idx_events_task ON events(task_id);
COMMIT;
"#;

pub(super) const AUDIT_TABLES: &[&str] = &["events"];

pub(super) const RAG_INDEX_SCHEMA_SQL: &str = r#"
BEGIN;
CREATE TABLE IF NOT EXISTS docs (
    id TEXT PRIMARY KEY,
    source TEXT NOT NULL,
    meta TEXT
);
CREATE VIRTUAL TABLE IF NOT EXISTS docs_fts USING fts5(doc_id UNINDEXED, content, tokenize = 'unicode61');
COMMIT;
"#;

pub(super) const RAG_TABLES: &[&str] = &["docs", "docs_fts"];

pub(super) fn ensure_registry_database(
    workspace: &Workspace,
    opts: &InitOptions,
    reporter: &mut InitReporter,
) -> Result<(), InitError> {
    ensure_sqlite_artifact(
        &workspace.registry_path(),
        opts,
        reporter,
        "registry database",
        REGISTRY_SCHEMA_SQL,
        REGISTRY_TABLES,
    )
}

pub(super) fn ensure_audit_index_database(
    workspace: &Workspace,
    opts: &InitOptions,
    reporter: &mut InitReporter,
) -> Result<(), InitError> {
    ensure_sqlite_artifact(
        &workspace.audit_index_path(),
        opts,
        reporter,
        "audit index",
        AUDIT_INDEX_SCHEMA_SQL,
        AUDIT_TABLES,
    )
}

pub(super) fn ensure_rag_index_database(
    workspace: &Workspace,
    opts: &InitOptions,
    reporter: &mut InitReporter,
) -> Result<(), InitError> {
    ensure_sqlite_artifact(
        &workspace.rag_index_path(),
        opts,
        reporter,
        "RAG index",
        RAG_INDEX_SCHEMA_SQL,
        RAG_TABLES,
    )
}

pub(super) fn validate_sqlite_tables(
    path: &Path,
    tables: &[&str],
    label: &str,
) -> Result<(), String> {
    if !path.exists() {
        return Err(format!("Missing {label} at {}", path.display()));
    }

    let conn = Connection::open(path)
        .map_err(|err| format!("Failed to open {label} at {}: {err}", path.display()))?;

    for table in tables {
        let exists: i64 = conn
            .query_row(
                "SELECT count(1) FROM sqlite_master WHERE name = ?1",
                [table],
                |row| row.get(0),
            )
            .map_err(|err| format!("Failed to inspect {label} ({table}): {err}"))?;

        if exists == 0 {
            return Err(format!(
                "Table `{table}` missing in {label} at {}",
                path.display()
            ));
        }
    }

    Ok(())
}

fn ensure_sqlite_artifact(
    path: &Path,
    opts: &InitOptions,
    reporter: &mut InitReporter,
    label: &str,
    schema_sql: &str,
    expected_tables: &[&str],
) -> Result<(), InitError> {
    let existed = path.exists();
    let label_with_suffix = format!("{label} (SQLite)");

    if !existed {
        if opts.dry_run {
            reporter.planned(&format!("Create {label}"), path);
            return Ok(());
        }

        create_database(path, schema_sql, label)?;
        reporter.created(&label_with_suffix, path);
        return Ok(());
    }

    if opts.force {
        if opts.dry_run {
            reporter.planned(&format!("Refresh {label} schema"), path);
            return Ok(());
        }

        apply_schema(path, schema_sql, label)?;
        reporter.updated(&format!("{label} schema"), path);
        return Ok(());
    }

    if let Err(reason) = validate_sqlite_tables(path, expected_tables, label) {
        return prompt::handle_corrupted_artifact(
            &label_with_suffix,
            path,
            &reason,
            opts,
            reporter,
            move |reporter| recreate_sqlite_database(path, schema_sql, label, reporter),
        );
    }

    reporter.exists(&label_with_suffix, path);
    Ok(())
}

fn create_database(path: &Path, schema_sql: &str, label: &str) -> Result<(), InitError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to prepare directory {}", parent.display()))
            .map_err(InitError::Other)?;
    }

    let conn = Connection::open(path)
        .with_context(|| format!("Failed to create {label} at {}", path.display()))
        .map_err(InitError::Other)?;
    conn.execute_batch(schema_sql)
        .with_context(|| format!("Failed to initialize {label} schema"))
        .map_err(InitError::Other)?;
    Ok(())
}

fn recreate_sqlite_database(
    path: &Path,
    schema_sql: &str,
    label: &str,
    reporter: &mut InitReporter,
) -> Result<(), InitError> {
    if path.exists() {
        remove_path(path)
            .with_context(|| format!("Failed to remove corrupted {label} at {}", path.display()))
            .map_err(InitError::Other)?;
    }

    create_database(path, schema_sql, label)?;
    reporter.updated(&format!("{label} (SQLite)"), path);
    Ok(())
}

fn apply_schema(path: &Path, schema_sql: &str, label: &str) -> Result<(), InitError> {
    let conn = Connection::open(path)
        .with_context(|| format!("Failed to open {label} at {}", path.display()))
        .map_err(InitError::Other)?;
    conn.execute_batch(schema_sql)
        .with_context(|| format!("Failed to refresh {label} schema"))
        .map_err(InitError::Other)?;
    Ok(())
}

fn remove_path(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::IsADirectory => fs::remove_dir_all(path),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}
