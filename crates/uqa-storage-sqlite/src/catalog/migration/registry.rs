//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Single ordered catalog-migration dispatcher.

use super::super::{
    params, Catalog, ManagedConnection, OptionalExtension, Result, SQLiteError,
    CURRENT_SCHEMA_VERSION,
};
use super::steps::{MigrationAction, MIGRATIONS};

impl Catalog {
    /// Open (or create) the catalog and run any pending migrations.
    pub fn open(conn: ManagedConnection) -> Result<Self> {
        let cat = Self::for_initial_restore(conn);
        cat.initialize_storage()?;
        Ok(cat)
    }

    pub(crate) fn for_initial_restore(conn: ManagedConnection) -> Self {
        Self { conn }
    }

    pub(in crate::catalog) fn initialize_storage(&self) -> Result<()> {
        if self.conn.is_native_record_session() {
            // Binding already validated the complete mapped format. Never run legacy physical migrations inside a logical session.
            self.conn.native_snapshot()?;
            return Ok(());
        }
        self.run_migrations()
    }

    pub fn connection(&self) -> ManagedConnection {
        self.conn.clone()
    }

    pub(super) fn run_migrations(&self) -> Result<()> {
        self.conn.with_mut(|conn| {
            // A savepoint joins an existing restore or owns the complete catalog migration.
            let transaction = conn.savepoint()?;
            Self::migrate_storage_in(&transaction)?;
            transaction.commit()?;
            Ok(())
        })
    }

    /// Join the caller's physical transaction so provider format conversion and catalog bootstrap commit or roll back together.
    pub(crate) fn migrate_storage_in(conn: &rusqlite::Connection) -> Result<()> {
        if conn.is_autocommit() {
            return Err(SQLiteError::StorageBackend(
                "catalog migration requires a physical transaction".into(),
            ));
        }
        // Older catalogs (pre-v7) used the table name `_meta`. v7
        // renames it to `_metadata`; promote the legacy table before
        // any migration query touches it.
        let legacy_meta_only: bool = conn
            .query_row(
                "SELECT \
                    (SELECT COUNT(*) FROM sqlite_master \
                      WHERE type='table' AND name='_meta') > 0 \
                 AND (SELECT COUNT(*) FROM sqlite_master \
                        WHERE type='table' AND name='_metadata') = 0",
                [],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .is_some_and(|n| n != 0);
        if legacy_meta_only {
            conn.execute("ALTER TABLE _meta RENAME TO _metadata", [])?;
        }
        conn.execute(
            "CREATE TABLE IF NOT EXISTS _metadata (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            )",
            [],
        )?;
        let current = conn
            .query_row(
                "SELECT value FROM _metadata WHERE key = 'schema_version'",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        let current = match current {
            Some(version) => version
                .parse::<u32>()
                .map_err(|_| SQLiteError::InvalidSchemaVersion(version))?,
            None => 0,
        };
        if current > CURRENT_SCHEMA_VERSION {
            return Err(SQLiteError::UnsupportedSchemaVersion {
                found: current,
                supported: CURRENT_SCHEMA_VERSION,
            });
        }

        debug_assert_eq!(
            MIGRATIONS.last().map(|migration| migration.version),
            Some(CURRENT_SCHEMA_VERSION)
        );
        for migration in &MIGRATIONS {
            if migration.version > current {
                match migration.action {
                    MigrationAction::Sql(sql) => conn.execute_batch(sql)?,
                    MigrationAction::Custom(migrate) => migrate(conn)?,
                }
                conn.execute(
                    "INSERT OR REPLACE INTO _metadata (key, value) \
                     VALUES ('schema_version', ?1)",
                    params![migration.version.to_string()],
                )?;
            }
        }
        let schema_before_repair: i64 =
            conn.pragma_query_value(None, "schema_version", |row| row.get(0))?;
        Self::ensure_column_stats_shape(conn)?;
        let schema_after_repair: i64 =
            conn.pragma_query_value(None, "schema_version", |row| row.get(0))?;
        if schema_before_repair != schema_after_repair {
            Self::install_cache_revision_tracking(conn)?;
        }
        Self::upgrade_metadata_cache_triggers(conn)?;
        Ok(())
    }
}
