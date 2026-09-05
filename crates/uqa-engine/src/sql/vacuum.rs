//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! PostgreSQL-compatible `VACUUM` command validation and storage maintenance.

use std::collections::BTreeSet;
use std::sync::atomic::Ordering;

use uqa_sql::ast::{VacuumOption, VacuumOptionValue, VacuumStmt};
use uqa_sql::{SQLError, SQLResult};
use uqa_storage::StorageBackendError;

use super::Engine;

struct VacuumExecution {
    flags: VacuumFlags,
}

struct ResolvedVacuumTarget {
    table: String,
    include_descendants: bool,
    columns: Vec<String>,
}

fn vacuum_storage_error(context: &str, error: impl std::fmt::Display) -> StorageBackendError {
    StorageBackendError::Other(format!("{context}: {error}"))
}

fn rewrite_full_vacuum_targets(
    engine: &Engine,
    targets: &[ResolvedVacuumTarget],
) -> Result<(), SQLError> {
    let mut tables = BTreeSet::new();
    for target in targets {
        tables.extend(
            engine
                .hierarchy_scan_tables(&target.table, target.include_descendants)?
                .into_iter(),
        );
    }
    for table in &tables {
        if let Err(error) =
            engine.lock_relation(table, crate::row_locks::RelationLockMode::AccessExclusive)
        {
            engine.row_locks.release_session(engine.session_id);
            return Err(SQLError::Internal(format!(
                "VACUUM FULL failed: lock relation: {error}"
            )));
        }
    }
    let result = engine
        .with_read_only_compatible_storage_transaction(|engine| {
            for table in &tables {
                rewrite_full_vacuum_table(engine, table)?;
            }
            Ok(())
        })
        .and_then(|()| {
            if let Some(backend) = engine.storage.backend.as_ref() {
                backend.vacuum()?;
            }
            Ok(())
        })
        .map_err(|error| SQLError::Internal(format!("VACUUM FULL failed: {error}")));
    engine.row_locks.release_session(engine.session_id);
    result
}

fn rewrite_full_vacuum_table(engine: &Engine, table_name: &str) -> Result<(), StorageBackendError> {
    let table = engine
        .require_table(table_name)
        .map_err(|error| vacuum_storage_error("resolve VACUUM FULL relation", error))?;
    let stats = table.column_stats.read().clone();
    let stats_loaded = table.column_stats_loaded.load(Ordering::Acquire);
    let stats_dirty = table.column_stats_dirty.load(Ordering::Acquire);
    let documents = {
        let store = table.document_store.read();
        let mut ids = store.doc_ids()?;
        ids.sort_unstable();
        let documents = store.get_stored_many(&ids)?;
        let mut rows = Vec::with_capacity(ids.len());
        for doc_id in ids {
            let document = documents.get(&doc_id).cloned().ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "VACUUM FULL relation `{table_name}` listed document {doc_id} but did not return it"
                ))
            })?;
            let vectors = Engine::document_vector_values(&table, document.fields())
                .map_err(|error| vacuum_storage_error("snapshot VACUUM FULL vectors", error))?;
            rows.push((doc_id, document, vectors));
        }
        rows
    };
    table.document_store.write().clear()?;
    table.inverted_index.write().clear()?;
    for index in table.vector_indexes.write().values_mut() {
        index.clear()?;
    }
    if let Some(backend) = engine.storage.backend.as_ref() {
        backend.clear_btree_indexes(table_name)?;
    }
    Engine::value_indexes_clear(&table);
    for (doc_id, document, vectors) in documents {
        engine
            .add_prepared_stored_document_with_vector_values_inner(
                table_name, doc_id, document, vectors, true,
            )
            .map_err(|error| vacuum_storage_error("rewrite VACUUM FULL row", error))?;
    }
    engine
        .refresh_value_indexes_for_table(table_name)
        .map_err(|error| vacuum_storage_error("rebuild VACUUM FULL indexes", error))?;
    *table.column_stats.write() = stats.clone();
    table
        .column_stats_loaded
        .store(stats_loaded, Ordering::Release);
    table
        .column_stats_dirty
        .store(stats_dirty, Ordering::Release);
    if stats_loaded
        && !stats_dirty
        && table.persistence != uqa_sql::ast::RelationPersistence::Temporary
    {
        if let Some(catalog) = engine.storage.catalog.as_ref() {
            Engine::persist_column_stats(catalog.as_ref(), table_name, &stats)?;
        }
    }
    table.doc_count_dirty.store(true, Ordering::Release);
    engine.note_table_data_changed();
    Ok(())
}

#[derive(Clone, Copy, Default)]
struct VacuumFlags(u8);

impl VacuumFlags {
    const ANALYZE: u8 = 1 << 0;
    const FULL: u8 = 1 << 1;
    const DISABLE_PAGE_SKIPPING: u8 = 1 << 2;
    const PROCESS_TOAST: u8 = 1 << 3;
    const ONLY_DATABASE_STATS: u8 = 1 << 4;
    const ONLY_DATABASE_STATS_CONFLICT: u8 = 1 << 5;

    fn insert(&mut self, flag: u8, enabled: bool) {
        if enabled {
            self.0 |= flag;
        }
    }

    const fn contains(self, flag: u8) -> bool {
        self.0 & flag != 0
    }
}

impl VacuumExecution {
    const fn analyze(&self) -> bool {
        self.flags.contains(VacuumFlags::ANALYZE)
    }

    const fn full(&self) -> bool {
        self.flags.contains(VacuumFlags::FULL)
    }

    const fn disable_page_skipping(&self) -> bool {
        self.flags.contains(VacuumFlags::DISABLE_PAGE_SKIPPING)
    }

    const fn process_toast(&self) -> bool {
        self.flags.contains(VacuumFlags::PROCESS_TOAST)
    }

    const fn only_database_stats(&self) -> bool {
        self.flags.contains(VacuumFlags::ONLY_DATABASE_STATS)
    }

    const fn has_only_database_stats_conflict(&self) -> bool {
        self.flags
            .contains(VacuumFlags::ONLY_DATABASE_STATS_CONFLICT)
    }
}

fn vacuum_syntax_error(message: impl Into<String>) -> SQLError {
    SQLError::Routine {
        sqlstate: "42601".into(),
        message: message.into(),
    }
}

fn invalid_buffer_usage_limit() -> SQLError {
    SQLError::Routine {
        sqlstate: "22023".into(),
        message: "BUFFER_USAGE_LIMIT option must be 0 or between 128 kB and 16777216 kB".into(),
    }
}

fn vacuum_feature_error(message: impl Into<String>) -> SQLError {
    SQLError::Routine {
        sqlstate: "0A000".into(),
        message: message.into(),
    }
}

fn buffer_usage_limit_kib(value: &VacuumOptionValue) -> Result<u64, SQLError> {
    let (amount, multiplier) = match value {
        VacuumOptionValue::Integer(value) => {
            let amount = u64::try_from(*value).map_err(|_| invalid_buffer_usage_limit())?;
            return Ok(amount);
        }
        VacuumOptionValue::String(value) => {
            let value = value.trim();
            let split = value
                .find(|character: char| !(character.is_ascii_digit() || character == '.'))
                .unwrap_or(value.len());
            let amount = value[..split]
                .parse::<f64>()
                .map_err(|_| invalid_buffer_usage_limit())?;
            let unit = value[split..].trim().to_ascii_lowercase();
            let multiplier = match unit.as_str() {
                "" | "kb" => 1_f64,
                "mb" => 1024_f64,
                "gb" => 1024_f64 * 1024_f64,
                "tb" => 1024_f64 * 1024_f64 * 1024_f64,
                _ => return Err(invalid_buffer_usage_limit()),
            };
            (amount, multiplier)
        }
        VacuumOptionValue::Boolean(_) => return Err(invalid_buffer_usage_limit()),
    };
    let kib = amount * multiplier;
    if !kib.is_finite() || kib < 0.0 || kib.round() > u64::MAX as f64 {
        return Err(invalid_buffer_usage_limit());
    }
    Ok(kib.round() as u64)
}

fn boolean_option(option: &VacuumOption) -> Result<bool, SQLError> {
    let Some(value) = option.value.as_ref() else {
        return Ok(true);
    };
    match value {
        VacuumOptionValue::Boolean(value) => Ok(*value),
        VacuumOptionValue::String(value) => match value.to_ascii_lowercase().as_str() {
            "true" | "on" => Ok(true),
            "false" | "off" => Ok(false),
            _ => Err(vacuum_syntax_error(format!(
                "{} requires a Boolean value",
                option.name
            ))),
        },
        VacuumOptionValue::Integer(0) => Ok(false),
        VacuumOptionValue::Integer(1) => Ok(true),
        VacuumOptionValue::Integer(_) => Err(vacuum_syntax_error(format!(
            "{} requires a Boolean value",
            option.name
        ))),
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves catalog and storage cleanup order"
)]
fn validate_options(options: &[VacuumOption]) -> Result<VacuumExecution, SQLError> {
    let mut analyze = false;
    let mut full = false;
    let mut parallel = 0_i64;
    let mut buffer_usage_limit_specified = false;
    let mut disable_page_skipping = false;
    let mut process_toast = true;
    let mut only_database_stats = false;
    let mut effective_options = BTreeSet::new();
    for option in options {
        match option.name.as_str() {
            "analyze" => {
                analyze = boolean_option(option)?;
                if analyze {
                    effective_options.insert(option.name.as_str());
                } else {
                    effective_options.remove(option.name.as_str());
                }
            }
            "full" => {
                full = boolean_option(option)?;
                if full {
                    effective_options.insert(option.name.as_str());
                } else {
                    effective_options.remove(option.name.as_str());
                }
            }
            "only_database_stats" => only_database_stats = boolean_option(option)?,
            "freeze" | "skip_locked" | "skip_database_stats" => {
                if boolean_option(option)? {
                    effective_options.insert(option.name.as_str());
                } else {
                    effective_options.remove(option.name.as_str());
                }
            }
            "disable_page_skipping" => {
                disable_page_skipping = boolean_option(option)?;
                if disable_page_skipping {
                    effective_options.insert(option.name.as_str());
                } else {
                    effective_options.remove(option.name.as_str());
                }
            }
            "process_toast" => process_toast = boolean_option(option)?,
            // PostgreSQL 18 permits these with ONLY_DATABASE_STATS; they are not part of the VACOPT conflict mask used by that check.
            "verbose" | "process_main" | "truncate" => {
                boolean_option(option)?;
            }
            "index_cleanup" => {
                if !matches!(
                    option.value.as_ref(),
                    Some(VacuumOptionValue::String(value)) if value.eq_ignore_ascii_case("auto")
                ) {
                    boolean_option(option)?;
                }
            }
            "parallel" => {
                let Some(VacuumOptionValue::Integer(workers)) = option.value.as_ref() else {
                    return Err(vacuum_syntax_error(
                        "parallel requires a non-negative integer value",
                    ));
                };
                if !(0..=1024).contains(workers) {
                    return Err(vacuum_syntax_error(
                        "parallel workers for vacuum must be between 0 and 1024",
                    ));
                }
                parallel = i64::from(*workers);
            }
            "buffer_usage_limit" => {
                buffer_usage_limit_specified = true;
                let kib = option
                    .value
                    .as_ref()
                    .ok_or_else(invalid_buffer_usage_limit)
                    .and_then(buffer_usage_limit_kib)?;
                if kib != 0 && !(128..=16_777_216).contains(&kib) {
                    return Err(invalid_buffer_usage_limit());
                }
            }
            name => {
                return Err(vacuum_syntax_error(format!(
                    "unrecognized VACUUM option \"{name}\""
                )));
            }
        }
    }
    if full && parallel > 0 {
        return Err(vacuum_feature_error(
            "VACUUM FULL cannot be performed in parallel",
        ));
    }
    if buffer_usage_limit_specified && full && !analyze {
        return Err(vacuum_feature_error(
            "BUFFER_USAGE_LIMIT cannot be specified for VACUUM FULL",
        ));
    }
    let mut flags = VacuumFlags::default();
    flags.insert(VacuumFlags::ANALYZE, analyze);
    flags.insert(VacuumFlags::FULL, full);
    flags.insert(VacuumFlags::DISABLE_PAGE_SKIPPING, disable_page_skipping);
    flags.insert(VacuumFlags::PROCESS_TOAST, process_toast);
    flags.insert(VacuumFlags::ONLY_DATABASE_STATS, only_database_stats);
    flags.insert(
        VacuumFlags::ONLY_DATABASE_STATS_CONFLICT,
        !effective_options.is_empty(),
    );
    Ok(VacuumExecution { flags })
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves catalog and storage cleanup order"
)]
pub(super) fn run_vacuum(engine: &Engine, statement: &VacuumStmt) -> Result<SQLResult, SQLError> {
    if engine.transaction_depth() != 0 {
        return Err(SQLError::Routine {
            sqlstate: "25001".into(),
            message: "VACUUM cannot run inside a transaction block".into(),
        });
    }

    let execution = validate_options(&statement.options)?;
    if !execution.analyze()
        && statement
            .targets
            .iter()
            .any(|target| !target.columns.is_empty())
    {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "ANALYZE option must be specified when a column list is provided".into(),
        });
    }
    if execution.full() && execution.disable_page_skipping() {
        return Err(vacuum_feature_error(
            "VACUUM option DISABLE_PAGE_SKIPPING cannot be used with FULL",
        ));
    }
    if execution.full() && !execution.process_toast() {
        return Err(vacuum_feature_error(
            "PROCESS_TOAST required with VACUUM FULL",
        ));
    }
    if execution.only_database_stats() && !statement.targets.is_empty() {
        return Err(vacuum_feature_error(
            "ONLY_DATABASE_STATS cannot be specified with a list of tables",
        ));
    }
    if execution.only_database_stats() && execution.has_only_database_stats_conflict() {
        return Err(vacuum_feature_error(
            "ONLY_DATABASE_STATS cannot be specified with other VACUUM options",
        ));
    }

    let mut resolved_targets = Vec::with_capacity(statement.targets.len());
    for target in &statement.targets {
        if target
            .catalog
            .as_deref()
            .is_some_and(|catalog| catalog != "uqa")
        {
            let qualified = format!(
                "{}.{}",
                target.catalog.as_deref().expect("checked catalog"),
                target.table
            );
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: format!("cross-database references are not implemented: \"{qualified}\""),
            });
        }
        let canonical = match engine.try_resolve_visible_relation_kind(&target.table)? {
            Some((canonical, "table")) => canonical,
            Some(_) | None => return Err(SQLError::UnknownTable(target.table.clone())),
        };
        let table = engine.require_table(&canonical)?;
        if !target.columns.is_empty() {
            let available = table
                .columns
                .read()
                .iter()
                .map(|column| column.name.clone())
                .collect::<BTreeSet<_>>();
            if let Some(column) = target
                .columns
                .iter()
                .find(|column| !available.contains(column.as_str()))
            {
                return Err(SQLError::Routine {
                    sqlstate: "42703".into(),
                    message: format!(
                        "column \"{column}\" of relation \"{}\" does not exist",
                        target.table
                    ),
                });
            }
        }
        engine.ensure_table_privilege(
            &canonical,
            crate::engine_table_security::TableAclPrivilege::Maintain,
        )?;
        resolved_targets.push(ResolvedVacuumTarget {
            table: canonical,
            include_descendants: target.include_descendants,
            columns: target.columns.clone(),
        });
    }

    if execution.only_database_stats() {
        return Ok(SQLResult::empty());
    }

    if execution.full() {
        if resolved_targets.is_empty() {
            if let Some(backend) = engine.storage.backend.as_ref() {
                backend
                    .vacuum()
                    .map_err(|error| SQLError::Internal(format!("VACUUM failed: {error}")))?;
            }
        } else {
            rewrite_full_vacuum_targets(engine, &resolved_targets)?;
        }
    }

    if execution.analyze() {
        if resolved_targets.is_empty() {
            for table in engine.maintenance_table_names("vacuum")? {
                engine
                    .run_analyze_target(&table, &[], true)
                    .map_err(|error| {
                        SQLError::Internal(format!("VACUUM ANALYZE failed: {error}"))
                    })?;
            }
        } else {
            for target in &resolved_targets {
                engine
                    .run_analyze_target(&target.table, &target.columns, target.include_descendants)
                    .map_err(|error| {
                        SQLError::Internal(format!("VACUUM ANALYZE failed: {error}"))
                    })?;
            }
        }
    }

    Ok(SQLResult::empty())
}
