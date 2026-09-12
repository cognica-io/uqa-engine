//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execute ordinary-table DROP through native dependency owners and retained table state.
pub mod context;
mod dependencies;
use context::{TableDropCandidate, TableRemovalContext, TableRemovalEntry};
use uqa_core::RelationIdentity;
use uqa_sql::schema::removal::tables::detach_inbound_foreign_keys;
use uqa_storage::{StorageBackendError, StorageBackendResult};
fn resolved_relation_identity(name: &str) -> StorageBackendResult<RelationIdentity> {
    RelationIdentity::from_legacy_name(name).map_err(StorageBackendError::Other)
}
fn table_not_found(table: &str) -> StorageBackendError {
    StorageBackendError::Other(format!("table `{table}` does not exist"))
}
impl TableRemovalContext<'_> {
    pub fn drop_table(&self, name: &str) -> StorageBackendResult<bool> {
        let Some(name) = self.resolve_table_ddl_target(name, "DROP TABLE")? else {
            return Ok(false);
        };
        self.try_drop_tables_inner(&[name], false)?;
        Ok(true)
    }
    pub fn try_drop_tables(&self, names: &[String], cascade: bool) -> StorageBackendResult<()> {
        self.transactions
            .with_table_removal_write(Box::new(move |context| {
                context.try_drop_tables_inner(names, cascade)
            }))
    }
    fn resolve_table_ddl_target(
        &self,
        name: &str,
        action: &str,
    ) -> StorageBackendResult<Option<String>> {
        uqa_sql::schema::removal::tables::resolved_table_ddl_target(
            self.catalog.relation_kind(name)?,
            action,
        )
        .map_err(StorageBackendError::Other)
    }
    pub fn hierarchy_drop_targets(
        &self,
        roots: &[String],
        cascade: bool,
    ) -> (Vec<String>, Vec<String>) {
        uqa_sql::schema::removal::hierarchy::hierarchy_drop_targets(self.hierarchy, roots, cascade)
    }
    pub fn try_drop_tables_inner(
        &self,
        names: &[String],
        cascade: bool,
    ) -> StorageBackendResult<()> {
        let canonical_names = self.canonical_hierarchy_drop_targets(names, cascade)?;
        crate::routines::removal::drop_relation_routine_dependents(
            &self.routines,
            &canonical_names,
            cascade,
            "table",
        )
        .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        let (target_names, targets) = Self::drop_target_sets(&canonical_names)?;
        let entries = self.catalog.table_entries();

        if !cascade {
            self.ensure_no_drop_restrict_dependents(
                &canonical_names,
                &target_names,
                &targets,
                &entries,
            )?;
        }

        // Finish every dependency check before mutating a referrer or target. CASCADE removes the complete dependent-view closure after this preflight instead of treating a rewritable catalog dependency as an error.
        if !cascade {
            self.ensure_no_drop_view_dependencies(&canonical_names)?;
        }
        Self::ensure_drop_targets_unreferenced(&target_names, &targets, &entries)?;
        let owned_sequences = self.owned_sequences_for_drop(&target_names, &entries)?;

        let (mut inbound, updates) =
            Self::prepare_inbound_candidates(entries, &target_names, &targets);
        if !cascade && !inbound.is_empty() {
            inbound.sort_unstable();
            inbound.dedup();
            return Err(StorageBackendError::Other(format!(
                "DROP TABLE rejected: still referenced by foreign key(s) on `{}`; use CASCADE",
                inbound.join("`, `")
            )));
        }
        if cascade {
            self.events
                .drop_rules_depending_on_relations_inner(&canonical_names)?;
            crate::schema::view_removal::drop_views_depending_on_relations(
                &self.views,
                &canonical_names,
            )?;
            for (_, table, columns, checks, foreign_keys, key_constraints) in &updates {
                table.persist_constraints(columns, checks, foreign_keys, key_constraints)?;
            }
            for (_, table, columns, checks, foreign_keys, _) in updates {
                **table.columns_write() = columns;
                **table.checks_write() = checks;
                **table.foreign_keys_write() = foreign_keys;
            }
        }
        for name in canonical_names {
            self.drop_table_state_inner(&name)?;
        }
        self.publication
            .prune_constraint_modes()
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        for sequence in owned_sequences {
            self.sequences.drop_owned_sequence(&sequence, cascade)?;
        }
        Ok(())
    }
    fn prepare_inbound_candidates<'a>(
        entries: Vec<TableRemovalEntry<'a>>,
        target_names: &std::collections::BTreeSet<String>,
        targets: &[RelationIdentity],
    ) -> (Vec<String>, Vec<TableDropCandidate<'a>>) {
        let mut inbound = Vec::new();
        let mut updates = Vec::new();
        for (candidate_name, table) in entries {
            if target_names.contains(&candidate_name) {
                continue;
            }
            let mut columns = table.columns().clone();
            let checks = table.table_checks().clone();
            let mut foreign_keys = table.foreign_keys().clone();
            let key_constraints = table.key_constraints().clone();
            let changed = detach_inbound_foreign_keys(&mut columns, &mut foreign_keys, targets);
            if changed {
                inbound.push(candidate_name.clone());
                updates.push((
                    candidate_name,
                    table,
                    columns,
                    checks,
                    foreign_keys,
                    key_constraints,
                ));
            }
        }
        (inbound, updates)
    }
    pub fn drop_temporary_table_on_commit_inner(&self, name: &str) -> StorageBackendResult<()> {
        crate::schema::view_removal::drop_temporary_views_depending_on_relation_inner(
            &self.views,
            name,
        )?;
        self.try_drop_tables_inner(&[name.to_string()], true)
    }
    fn drop_table_state_inner(&self, name: &str) -> StorageBackendResult<()> {
        let relation = resolved_relation_identity(name)?;
        if !self.catalog.contains_relation(&relation) {
            return Err(table_not_found(name));
        }
        self.events.drop_relation_events_inner(&relation)?;
        self.publication.remove_state(name, &relation)
    }
}
