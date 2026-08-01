//! Source-catalog discovery, validation, and destination preflight.

use super::{
    fs, quote_ident, Connection, OpenFlags, OptionalExtension, Path, PathBuf, PythonMigrationError,
};

pub(super) fn resolve_source_database(source: &Path) -> Result<PathBuf, PythonMigrationError> {
    if !source.exists() {
        return Err(PythonMigrationError::SourceMissing(source.to_path_buf()));
    }
    if source.is_file() {
        return Ok(source.to_path_buf());
    }
    let mut candidates = Vec::new();
    collect_sqlite_catalogs(source, &mut candidates)?;
    match candidates.len() {
        0 => Err(PythonMigrationError::SourceCatalogMissing(
            source.to_path_buf(),
        )),
        1 => Ok(candidates.remove(0)),
        _ => Err(PythonMigrationError::MultipleSourceCatalogs(
            candidates
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", "),
        )),
    }
}

pub(super) fn collect_sqlite_catalogs(
    dir: &Path,
    out: &mut Vec<PathBuf>,
) -> Result<(), PythonMigrationError> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_sqlite_catalogs(&path, out)?;
        } else if file_type.is_file()
            && has_sqlite_extension(&path)
            && is_python_catalog_file(&path)?
        {
            out.push(path);
        }
    }
    Ok(())
}

pub(super) fn has_sqlite_extension(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|s| s.to_str()) else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "db" | "sqlite" | "sqlite3"
    )
}

pub(super) fn is_python_catalog_file(path: &Path) -> Result<bool, PythonMigrationError> {
    let Ok(conn) = open_read_only(path) else {
        return Ok(false);
    };
    is_python_catalog(&conn).map_err(PythonMigrationError::from)
}

pub(super) fn open_read_only(path: &Path) -> rusqlite::Result<Connection> {
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
}

pub(super) fn reject_same_path(
    source: &Path,
    destination: &Path,
) -> Result<(), PythonMigrationError> {
    if destination.exists() {
        let src = fs::canonicalize(source)?;
        let dst = fs::canonicalize(destination)?;
        if src == dst {
            return Err(PythonMigrationError::SameSourceAndDestination(dst));
        }
    }
    Ok(())
}

pub(super) fn ensure_destination_empty(destination: &Path) -> Result<(), PythonMigrationError> {
    if !destination.exists() || fs::metadata(destination)?.len() == 0 {
        return Ok(());
    }
    let conn = open_read_only(destination)?;
    let table_names = sqlite_table_names(&conn)?;
    if table_names.is_empty() {
        return Ok(());
    }
    if !table_names.iter().any(|name| name == "_tables") {
        return Err(PythonMigrationError::DestinationNotEmptyCatalog(
            destination.to_path_buf(),
        ));
    }
    let checked = [
        "_tables",
        "_documents",
        "_postings",
        "_vectors",
        "_named_graphs",
        "_graph_vertices",
        "_graph_edges",
        "_graph_membership",
        "_catalog_indexes",
        "_scoring_params",
        "_models",
        "_analyzers",
        "_foreign_servers",
        "_foreign_tables",
    ];
    let mut non_empty = Vec::new();
    for table in checked {
        if table_names.iter().any(|name| name == table) && row_count(&conn, table)? > 0 {
            non_empty.push(table.to_string());
        }
    }
    if non_empty.is_empty() {
        Ok(())
    } else {
        Err(PythonMigrationError::DestinationNotEmpty(
            non_empty.join(", "),
        ))
    }
}

pub(super) fn sqlite_table_names(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt =
        conn.prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub(super) fn is_python_catalog(conn: &Connection) -> rusqlite::Result<bool> {
    table_exists(conn, "_catalog_tables")
}

pub(super) fn table_exists(conn: &Connection, name: &str) -> rusqlite::Result<bool> {
    let exists: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1 LIMIT 1",
            [name],
            |row| row.get(0),
        )
        .optional()?;
    Ok(exists.is_some())
}

pub(super) fn row_count(conn: &Connection, table: &str) -> rusqlite::Result<i64> {
    let sql = format!("SELECT COUNT(*) FROM {}", quote_ident(table));
    conn.query_row(&sql, [], |row| row.get(0))
}
