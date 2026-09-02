//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::collections::BTreeSet;

use super::{DocId, Engine, SQLError, SQLResult};
use crate::engine_table_storage::document_store_write_error;

impl Engine {
    pub fn truncate_table(&self, name: &str) -> Result<(), SQLError> {
        self.truncate_tables(std::slice::from_ref(&name.to_string()))
    }

    pub(crate) fn truncate_tables(&self, names: &[String]) -> Result<(), SQLError> {
        self.truncate_tables_with_identity(names, true)
    }

    pub(crate) fn truncate_tables_with_identity(
        &self,
        names: &[String],
        restart_identity: bool,
    ) -> Result<(), SQLError> {
        if self.transaction_depth() != 0 {
            return self.truncate_tables_inner(names, restart_identity);
        }
        if self.storage.backend.is_none() {
            return self.truncate_tables_inner(names, restart_identity);
        }
        self.begin_implicit_statement_transaction(false)?;
        match self.truncate_tables_inner(names, restart_identity) {
            Ok(()) => self.run_transaction_statement(uqa_sql::ast::TransactionStmt::Commit),
            Err(error) => {
                match self.run_transaction_statement(uqa_sql::ast::TransactionStmt::Rollback) {
                    Ok(()) => Err(error),
                    Err(rollback_error) => Err(SQLError::Internal(format!(
                        "TRUNCATE failed: {error}; rollback also failed: {rollback_error}"
                    ))),
                }
            }
        }
    }

    fn truncate_tables_inner(
        &self,
        names: &[String],
        restart_identity: bool,
    ) -> Result<(), SQLError> {
        let mut ordered = Vec::new();
        let mut lock_order = std::collections::BTreeSet::new();
        for name in names {
            let Some((table_name, "table")) = self.try_resolve_visible_relation_kind(name)? else {
                return Err(SQLError::UnknownTable(name.to_string()));
            };
            if lock_order.insert(table_name.clone()) {
                ordered.push(table_name);
            }
        }
        for table_name in &lock_order {
            self.lock_relation(
                table_name,
                crate::row_locks::RelationLockMode::AccessExclusive,
            )?;
        }
        if !ordered.is_empty() {
            self.prepare_explicit_transaction_writer()?;
        }
        for table_name in ordered {
            self.truncate_locked_table(&table_name, restart_identity)?;
        }
        Ok(())
    }

    pub(crate) fn truncate_locked_table(
        &self,
        table_name: &str,
        restart_identity: bool,
    ) -> Result<(), SQLError> {
        let t = self.require_table(table_name)?;
        *t.storage_generation.write() = crate::new_table_storage_generation().map_err(|error| {
            SQLError::Internal(format!("rotate TRUNCATE storage generation: {error}"))
        })?;
        self.try_save_table_schema(table_name, &t)
            .map_err(|error| {
                SQLError::Internal(format!("persist TRUNCATE storage generation: {error}"))
            })?;
        // Snapshot the doc id set before grabbing any write locks so
        // we do not deadlock against the read guard inside the loop.
        let ids: Vec<DocId> = t
            .document_store
            .read()
            .doc_ids()
            .map_err(|error| SQLError::Internal(format!("read document ids: {error}")))?;
        for doc_id in ids {
            t.document_store
                .write()
                .delete(doc_id)
                .map_err(|err| document_store_write_error(&err))?;
            t.inverted_index
                .write()
                .remove_document(doc_id)
                .map_err(|error| SQLError::Internal(format!("remove indexed document: {error}")))?;
            for idx in t.vector_indexes.write().values_mut() {
                idx.as_mut().delete(doc_id).map_err(|error| {
                    SQLError::Internal(format!("delete indexed vector: {error}"))
                })?;
            }
            self.note_row_deleted(table_name, doc_id)?;
        }
        if restart_identity {
            *t.next_id.lock() = 1;
            self.persist_next_id(table_name).map_err(|error| {
                SQLError::Internal(format!("persist TRUNCATE identity: {error}"))
            })?;
            let owned_sequences = self
                .sequence_names_owned_by_tables(&std::collections::BTreeSet::from([t.object_id()]))
                .map_err(|error| SQLError::Internal(format!("load owned sequences: {error}")))?;
            for sequence in owned_sequences {
                self.restart_owned_sequence(&sequence).map_err(|error| {
                    SQLError::Internal(format!("restart owned sequence `{sequence}`: {error}"))
                })?;
            }
        }
        self.value_indexes_truncate(table_name, &t)?;
        self.mark_column_stats_dirty(table_name, &t)
            .map_err(|err| SQLError::Internal(format!("invalidate column stats: {err}")))?;
        Ok(())
    }
}

pub(crate) fn execute_sql_truncate(
    engine: &Engine,
    tables: &[uqa_sql::ast::TruncateTarget],
    cascade: bool,
    restart_identity: bool,
) -> Result<SQLResult, SQLError> {
    let targets = resolve_sql_truncate_targets(engine, tables, cascade)?;
    if engine.transaction_depth() == 0 {
        engine.transaction(|engine| run_sql_truncate(engine, &targets, restart_identity))?;
    } else {
        run_sql_truncate(engine, &targets, restart_identity)?;
    }
    Ok(SQLResult::empty())
}

struct SQLTruncateTargets {
    all: BTreeSet<String>,
    trigger_order: Vec<String>,
}

fn resolve_sql_truncate_targets(
    engine: &Engine,
    tables: &[uqa_sql::ast::TruncateTarget],
    cascade: bool,
) -> Result<SQLTruncateTargets, SQLError> {
    let mut targets = BTreeSet::new();
    let mut trigger_targets = Vec::new();
    for requested in tables {
        let Some((table, "table")) = engine.try_resolve_visible_relation_kind(&requested.table)?
        else {
            return Err(SQLError::Unsupported(format!(
                "TRUNCATE TABLE: relation `{}` does not exist",
                requested.table
            )));
        };
        let hierarchy = engine
            .try_table_hierarchy(&table)
            .map_err(|err| SQLError::Internal(format!("read table hierarchy: {err}")))?;
        if !requested.include_descendants && hierarchy.partition_spec.is_some() {
            return Err(SQLError::Routine {
                sqlstate: "42809".into(),
                message: "cannot truncate only a partitioned table".into(),
            });
        }
        for target in engine.hierarchy_scan_tables(&table, requested.include_descendants)? {
            if targets.insert(target.clone()) {
                trigger_targets.push(target);
            }
        }
    }
    if cascade {
        let mut cursor = 0;
        while let Some(table) = trigger_targets.get(cursor).cloned() {
            cursor += 1;
            for (referrer, _) in engine
                .referrers_to(&table)
                .map_err(|err| SQLError::Internal(format!("read foreign keys: {err}")))?
            {
                if targets.insert(referrer.clone()) {
                    trigger_targets.push(referrer);
                }
            }
        }
    }
    for table in &trigger_targets {
        engine.ensure_no_pending_trigger_events(table, "TRUNCATE")?;
    }
    if !cascade {
        for table in &targets {
            if let Some((referrer, _)) = engine
                .referrers_to(table)
                .map_err(|err| SQLError::Internal(format!("read foreign keys: {err}")))?
                .into_iter()
                .find(|(referrer, _)| !targets.contains(referrer))
            {
                return Err(SQLError::TypeMismatch(format!(
                        "cannot truncate `{table}` because `{referrer}` references it; truncate both tables or use CASCADE"
                    )));
            }
        }
    }
    Ok(SQLTruncateTargets {
        all: targets,
        trigger_order: trigger_targets,
    })
}

fn run_sql_truncate(
    engine: &Engine,
    targets: &SQLTruncateTargets,
    restart_identity: bool,
) -> Result<(), SQLError> {
    for table in &targets.trigger_order {
        crate::sql::fire_statement_triggers(
            engine,
            table,
            uqa_sql::ast::TriggerTiming::Before,
            uqa_sql::ast::TriggerEvent::Truncate,
            &[],
        )?;
    }
    let ordered = truncate_dependency_order(engine, targets)?;
    engine.truncate_tables_with_identity(&ordered, restart_identity)?;
    for table in &targets.trigger_order {
        crate::sql::fire_statement_triggers(
            engine,
            table,
            uqa_sql::ast::TriggerTiming::After,
            uqa_sql::ast::TriggerEvent::Truncate,
            &[],
        )?;
    }
    Ok(())
}

/// Referencing relations precede their targets even though the low-level clear does not evaluate row foreign keys.
fn truncate_dependency_order(
    engine: &Engine,
    targets: &SQLTruncateTargets,
) -> Result<Vec<String>, SQLError> {
    let mut ordered = Vec::with_capacity(targets.all.len());
    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    for table in &targets.trigger_order {
        visit_truncate_target(
            engine,
            table,
            &targets.all,
            &mut visiting,
            &mut visited,
            &mut ordered,
        )?;
    }
    Ok(ordered)
}

fn visit_truncate_target(
    engine: &Engine,
    table: &str,
    targets: &BTreeSet<String>,
    visiting: &mut BTreeSet<String>,
    visited: &mut BTreeSet<String>,
    ordered: &mut Vec<String>,
) -> Result<(), SQLError> {
    if visited.contains(table) || !visiting.insert(table.to_string()) {
        return Ok(());
    }
    for (referrer, _) in engine
        .referrers_to(table)
        .map_err(|err| SQLError::Internal(format!("read foreign keys: {err}")))?
    {
        if targets.contains(&referrer) {
            visit_truncate_target(engine, &referrer, targets, visiting, visited, ordered)?;
        }
    }
    visiting.remove(table);
    if visited.insert(table.to_string()) {
        ordered.push(table.to_string());
    }
    Ok(())
}
