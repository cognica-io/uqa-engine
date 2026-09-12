//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered table, view, rule, schema-expression and owned-sequence DROP preflight.
use super::context::TableRemovalContext;
use super::{context::TableRemovalEntry, resolved_relation_identity, table_not_found};
use uqa_core::RelationIdentity;
use uqa_sql::schema::{
    dependencies::rewrites::stored_relation_reference_matches,
    removal::tables::{foreign_key_targets, table_schema_references_relation},
};
use uqa_storage::{StorageBackendError, StorageBackendResult};
impl TableRemovalContext<'_> {
    pub(super) fn canonical_drop_table_names(
        &self,
        names: &[String],
    ) -> StorageBackendResult<Vec<String>> {
        let mut canonical_names = Vec::with_capacity(names.len());
        for name in names {
            canonical_names.push(
                self.resolve_table_ddl_target(name, "DROP TABLE")?
                    .ok_or_else(|| table_not_found(name))?,
            );
        }
        canonical_names.sort_unstable();
        canonical_names.dedup();
        Ok(canonical_names)
    }
    pub(super) fn drop_target_sets(
        canonical_names: &[String],
    ) -> StorageBackendResult<(std::collections::BTreeSet<String>, Vec<RelationIdentity>)> {
        let target_names = canonical_names.iter().cloned().collect();
        let targets = canonical_names
            .iter()
            .map(|name| resolved_relation_identity(name))
            .collect::<StorageBackendResult<Vec<_>>>()?;
        Ok((target_names, targets))
    }
    pub(super) fn ensure_no_drop_view_dependencies(
        &self,
        canonical_names: &[String],
    ) -> StorageBackendResult<()> {
        for name in canonical_names {
            self.ensure_no_dependent_views("DROP TABLE", name)?;
        }
        Ok(())
    }
    pub(super) fn drop_table_restrict_dependents(
        &self,
        canonical_names: &[String],
        target_names: &std::collections::BTreeSet<String>,
        targets: &[RelationIdentity],
        entries: &[TableRemovalEntry<'_>],
    ) -> StorageBackendResult<Vec<String>> {
        let mut dependents = Vec::new();
        for name in canonical_names {
            dependents.extend(
                crate::schema::view_dependencies::views_depending_on_relation(
                    &self.views.dependencies,
                    name,
                )?
                .into_iter()
                .map(|view| format!("view {view}")),
            );
        }
        dependents.extend(
            self.events
                .lookup
                .rules_depending_on_relations(canonical_names)
                .map_err(uqa_storage::StorageBackendError::Other)?
                .into_iter()
                .map(|(table, rule)| format!("rule {rule} on table {}", table.qualified_name())),
        );
        for (candidate_name, table) in entries {
            if target_names.contains(candidate_name) {
                continue;
            }
            if targets
                .iter()
                .any(|target| table_schema_references_relation(table.as_ref(), target))
            {
                dependents.push(format!("schema expression on {candidate_name}"));
            }
            let table_foreign_key = table.foreign_keys().iter().any(|foreign_key| {
                targets
                    .iter()
                    .any(|target| foreign_key_targets(foreign_key, target))
            });
            let column_foreign_key = table.columns().iter().any(|column| {
                column.references.as_ref().is_some_and(|reference| {
                    targets
                        .iter()
                        .any(|target| stored_relation_reference_matches(&reference.table, target))
                })
            });
            if table_foreign_key || column_foreign_key {
                dependents.push(format!("foreign key on {candidate_name}"));
            }
        }
        dependents.sort_unstable();
        dependents.dedup();
        Ok(dependents)
    }
    pub fn try_drop_table_restrict_dependents(
        &self,
        names: &[String],
    ) -> StorageBackendResult<Vec<String>> {
        let canonical_names = self.canonical_drop_table_names(names)?;
        let (target_names, targets) = Self::drop_target_sets(&canonical_names)?;
        let entries = self.catalog.table_entries();
        let mut dependents = self.drop_table_restrict_dependents(
            &canonical_names,
            &target_names,
            &targets,
            &entries,
        )?;
        for sequence in self.owned_sequences_for_drop(&target_names, &entries)? {
            dependents.extend(
                self.sequences
                    .dependencies
                    .sequence_external_dependents_for_owner_drop(&sequence, &target_names)?,
            );
        }
        dependents.sort_unstable();
        dependents.dedup();
        Ok(dependents)
    }
    pub(super) fn ensure_no_drop_restrict_dependents(
        &self,
        canonical_names: &[String],
        target_names: &std::collections::BTreeSet<String>,
        targets: &[RelationIdentity],
        entries: &[TableRemovalEntry<'_>],
    ) -> StorageBackendResult<()> {
        let dependents =
            self.drop_table_restrict_dependents(canonical_names, target_names, targets, entries)?;
        if dependents.is_empty() {
            return Ok(());
        }
        Err(StorageBackendError::Other(format!(
            "DROP TABLE rejected: other objects depend on the target(s): `{}`; use CASCADE",
            dependents.join("`, `")
        )))
    }
    pub(super) fn owned_sequences_for_drop(
        &self,
        target_names: &std::collections::BTreeSet<String>,
        entries: &[TableRemovalEntry<'_>],
    ) -> StorageBackendResult<std::collections::BTreeSet<String>> {
        let table_object_ids = entries
            .iter()
            .filter(|(table, _)| target_names.contains(table))
            .map(|(_, state)| state.object_id())
            .collect::<std::collections::BTreeSet<_>>();
        crate::catalog::sequence_introspection::ownership::sequence_names_owned_by_tables(
            self.sequences.dependencies.sequences,
            &table_object_ids,
        )
    }
    pub(super) fn ensure_drop_targets_unreferenced(
        target_names: &std::collections::BTreeSet<String>,
        targets: &[RelationIdentity],
        entries: &[TableRemovalEntry<'_>],
    ) -> StorageBackendResult<()> {
        for (candidate_name, table) in entries {
            if target_names.contains(candidate_name) {
                continue;
            }
            if let Some(target) = targets
                .iter()
                .find(|target| table_schema_references_relation(table.as_ref(), target))
            {
                return Err(StorageBackendError::Other(format!(
                    "DROP TABLE `{}` rejected: schema expression on `{candidate_name}` may depend on it and cannot be rewritten safely",
                    target.qualified_name()
                )));
            }
        }
        Ok(())
    }
    pub(super) fn canonical_hierarchy_drop_targets(
        &self,
        names: &[String],
        cascade: bool,
    ) -> StorageBackendResult<Vec<String>> {
        let canonical_names = self.canonical_drop_table_names(names)?;
        let (canonical_names, hierarchy_dependents) =
            self.hierarchy_drop_targets(&canonical_names, cascade);
        if !hierarchy_dependents.is_empty() {
            return Err(StorageBackendError::Other(format!(
                "DROP TABLE rejected: table `{}` depends on the target through inheritance; use CASCADE",
                hierarchy_dependents.join("`, `")
            )));
        }
        Ok(canonical_names)
    }
    fn ensure_no_dependent_views(
        &self,
        action: &str,
        canonical_name: &str,
    ) -> StorageBackendResult<()> {
        let dependents = crate::schema::view_dependencies::views_depending_on_relation(
            &self.views.dependencies,
            canonical_name,
        )?;
        if dependents.is_empty() {
            return Ok(());
        }
        Err(StorageBackendError::Other(format!(
            "{action} `{canonical_name}` rejected: dependent view(s) `{}` use stored relation names that cannot be rewritten safely",
            dependents.join("`, `")
        )))
    }
}
