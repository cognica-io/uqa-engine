//! Transactional migration orchestration.

use super::{
    create_tables, ensure_destination_empty, install_secondary_indexes, is_python_catalog,
    load_catalog_indexes, load_table_specs, migrate_analyzers, migrate_column_stats,
    migrate_documents, migrate_foreign_servers, migrate_foreign_tables, migrate_graphs,
    migrate_models, migrate_path_indexes, migrate_scoring_params, migrate_table_field_analyzers,
    open_read_only, persist_catalog_indexes, reject_same_path, resolve_source_database, Engine,
    Path, PythonMigrationError, PythonMigrationReport,
};

pub fn migrate_python_database(
    source: &Path,
    destination: &Path,
) -> Result<PythonMigrationReport, PythonMigrationError> {
    let source_path = resolve_source_database(source)?;
    reject_same_path(&source_path, destination)?;
    ensure_destination_empty(destination)?;

    let source_conn = open_read_only(&source_path)?;
    if !is_python_catalog(&source_conn)? {
        return Err(PythonMigrationError::NotPythonCatalog(source_path));
    }

    let index_rows = load_catalog_indexes(&source_conn)?;
    let specs = load_table_specs(&source_conn, &index_rows)?;
    let engine = Engine::open(destination)?;

    if !engine.table_names()?.is_empty() || !engine.list_graphs()?.is_empty() {
        return Err(PythonMigrationError::DestinationNotEmpty(
            destination.display().to_string(),
        ));
    }

    let mut report = PythonMigrationReport {
        source_path,
        destination_path: destination.to_path_buf(),
        tables: 0,
        documents: 0,
        fts_fields: 0,
        vector_fields: 0,
        indexes: 0,
        analyzers: 0,
        table_field_analyzers: 0,
        foreign_servers: 0,
        foreign_tables: 0,
        graphs: 0,
        graph_vertices: 0,
        graph_edges: 0,
        path_indexes: 0,
        scoring_params: 0,
        models: 0,
        column_stats: 0,
    };

    // The destination starts empty, so migration is one logical catalog/data
    // replacement. Keep every table, index, registry, document, and graph
    // write on the engine's pinned connection; a late corrupt source row must
    // not leave a valid-looking partial destination behind.
    engine.with_implicit_mapped_transaction(
        |engine| {
            report.analyzers = migrate_analyzers(&source_conn, engine)?;
            create_tables(&source_conn, engine, &specs, &mut report)?;
            report.table_field_analyzers = migrate_table_field_analyzers(&source_conn, engine)?;
            install_secondary_indexes(engine, &specs, &mut report)?;
            migrate_documents(&source_conn, engine, &specs, &mut report)?;
            report.indexes = persist_catalog_indexes(engine, &index_rows)?;
            report.column_stats = migrate_column_stats(&source_conn, engine)?;
            report.scoring_params = migrate_scoring_params(&source_conn, engine)?;
            report.models = migrate_models(&source_conn, engine)?;
            report.foreign_servers = migrate_foreign_servers(&source_conn, engine)?;
            report.foreign_tables = migrate_foreign_tables(&source_conn, engine)?;
            migrate_graphs(&source_conn, engine, &mut report)?;
            report.path_indexes = migrate_path_indexes(&source_conn, engine)?;
            Ok(report)
        },
        PythonMigrationError::Invalid,
    )
}
