//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Database opening and Python-database migration reporting.

use super::{io, Engine, IsTerminal, Path, PythonMigrationReport, Write};

pub(super) fn print_migration_report_stdout(report: &PythonMigrationReport) -> io::Result<()> {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    print_migration_report(report, &mut out)
}

pub(super) fn print_migration_report(
    report: &PythonMigrationReport,
    out: &mut impl Write,
) -> io::Result<()> {
    writeln!(out, "migrated {}", report.source_path.display())?;
    writeln!(out, "destination {}", report.destination_path.display())?;
    writeln!(
        out,
        "tables={} documents={} fts_fields={} vector_fields={} indexes={}",
        report.tables, report.documents, report.fts_fields, report.vector_fields, report.indexes
    )?;
    writeln!(
        out,
        "graphs={} vertices={} edges={} path_indexes={}",
        report.graphs, report.graph_vertices, report.graph_edges, report.path_indexes
    )?;
    writeln!(
        out,
        "analyzers={} table_field_analyzers={} scoring_params={} models={}",
        report.analyzers, report.table_field_analyzers, report.scoring_params, report.models
    )?;
    writeln!(
        out,
        "foreign_servers={} foreign_tables={} column_stats={}",
        report.foreign_servers, report.foreign_tables, report.column_stats
    )
}

/// Open `path` with encryption support. When the file requires a key
/// (encrypted compressed container, or an unrecognized header that is
/// most likely `SQLCipher`) and no key was supplied, prompt on the
/// controlling terminal; without a terminal, fail with instructions.
/// Returns the engine, the display location, and the key that was
/// actually used so `\reset` can reopen the same way.
pub(super) fn open_engine_with_key(
    path: &Path,
    key: Option<&str>,
) -> Result<(Engine, String, Option<String>), String> {
    let mut key = key.map(str::to_string);
    if key.is_none() {
        let format = Engine::detect_database_file(path)
            .map_err(|err| format!("open failed: {}: {err}", path.display()))?;
        if format.requires_key() {
            if io::stdin().is_terminal() {
                let prompt = format!("Encryption key for {}: ", path.display());
                let entered = rpassword::prompt_password(prompt)
                    .map_err(|err| format!("failed to read encryption key: {err}"))?;
                if entered.is_empty() {
                    return Err(format!(
                        "open failed: {}: database requires an encryption key",
                        path.display()
                    ));
                }
                key = Some(entered);
            } else {
                return Err(format!(
                    "open failed: {}: database requires an encryption key \
                     (pass --key / --key-file or set UQA_KEY)",
                    path.display()
                ));
            }
        }
    }
    let engine = Engine::open_auto(path, key.as_deref()).map_err(|err| {
        let hint = if key.is_some() && matches!(err, uqa_engine::SQLiteError::SQLite(_)) {
            " (wrong encryption key, or the file is not a database)"
        } else {
            ""
        };
        format!("open failed: {}: {err}{hint}", path.display())
    })?;
    Ok((engine, path.display().to_string(), key))
}

pub(super) fn open_engine(
    db_path: Option<&Path>,
    key: Option<&str>,
) -> Result<(Engine, String, Option<String>), String> {
    match db_path {
        Some(path) => open_engine_with_key(path, key),
        None => Ok((Engine::new(), ":memory:".into(), None)),
    }
}
