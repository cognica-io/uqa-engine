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
        Self {
            conn,
            fts_storage_was_reset: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub(in crate::catalog) fn initialize_storage(&self) -> Result<()> {
        let reset = self.run_migrations()?;
        self.fts_storage_was_reset
            .fetch_or(reset, std::sync::atomic::Ordering::Release);
        Ok(())
    }

    pub fn connection(&self) -> ManagedConnection {
        self.conn.clone()
    }

    pub(super) fn run_migrations(&self) -> Result<bool> {
        self.conn.with_mut(|conn| {
            // A savepoint joins Engine's initial restore or owns the entire standalone catalog open. Later migration failures must also restore the original schema and postings.
            let mut conn = conn.savepoint()?;
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
                    let tx = conn.savepoint()?;
                    match migration.action {
                        MigrationAction::Sql(sql) => tx.execute_batch(sql)?,
                        MigrationAction::Custom(migrate) => migrate(&tx)?,
                    }
                    tx.execute(
                        "INSERT OR REPLACE INTO _metadata (key, value) \
                         VALUES ('schema_version', ?1)",
                        params![migration.version.to_string()],
                    )?;
                    tx.commit()?;
                }
            }
            let repair = conn.savepoint()?;
            let schema_before_repair: i64 =
                repair.pragma_query_value(None, "schema_version", |row| row.get(0))?;
            Self::ensure_column_stats_shape(&repair)?;
            let fts_storage_was_reset = Self::ensure_fts_storage_shape(&repair)?;
            let schema_after_repair: i64 =
                repair.pragma_query_value(None, "schema_version", |row| row.get(0))?;
            if schema_before_repair != schema_after_repair {
                Self::install_cache_revision_tracking(&repair)?;
            }
            repair.commit()?;
            conn.commit()?;
            Ok(fts_storage_was_reset)
        })
    }
}
