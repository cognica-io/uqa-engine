//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! DDL target resolution, relation dependencies, and catalog index references.

mod routines;
mod sequences;

use super::{
    rename_schema_expr_column, rename_schema_expr_qualified_column, rename_schema_expr_relation,
    schema_expr_references_column, schema_expr_references_relation,
    stored_relation_reference_matches, table_not_found, Arc, BTreeMap, CatalogIndexRow, Engine,
    IVFIndexParams, RelationIdentity, StorageBackendError, StorageBackendResult, TableState,
};
use crate::{HNSWIndexParams, VectorIndexSpec};

impl Engine {
    pub(crate) fn generated_columns_referencing_column(
        &self,
        table_name: &str,
        column: &str,
    ) -> StorageBackendResult<Vec<String>> {
        let table = self
            .table_entries()
            .into_iter()
            .find(|(name, _)| name == table_name)
            .map(|(_, state)| state)
            .ok_or_else(|| table_not_found(table_name))?;
        let columns = table.columns.read();
        let dependents = columns
            .iter()
            .filter(|candidate| candidate.name != column)
            .filter(|candidate| {
                candidate.generated.as_ref().is_some_and(|generated| {
                    schema_expr_references_column(&generated.expression, column)
                })
            })
            .map(|candidate| candidate.name.clone())
            .collect();
        Ok(dependents)
    }

    pub(super) fn resolve_table_ddl_target(
        &self,
        name: &str,
        action: &str,
    ) -> StorageBackendResult<Option<String>> {
        match self.try_resolve_relation_kind(name)? {
            Some((canonical, "table")) => Ok(Some(canonical)),
            Some((canonical, kind)) => Err(StorageBackendError::Other(format!(
                "{action}: relation `{canonical}` is a {kind}, not a table"
            ))),
            None => Ok(None),
        }
    }

    pub(super) fn catalog_index_columns(
        row: &CatalogIndexRow,
    ) -> StorageBackendResult<Vec<String>> {
        serde_json::from_str(&row.columns_json).map_err(StorageBackendError::from)
    }

    pub(super) fn catalog_index_references_column(
        row: &CatalogIndexRow,
        column: &str,
    ) -> StorageBackendResult<bool> {
        Ok(Self::catalog_index_columns(row)?
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(column)))
    }

    pub(super) fn catalog_index_with_renamed_column(
        mut row: CatalogIndexRow,
        from: &str,
        to: &str,
    ) -> StorageBackendResult<CatalogIndexRow> {
        let mut columns = Self::catalog_index_columns(&row)?;
        let mut changed = false;
        for column in &mut columns {
            if column.eq_ignore_ascii_case(from) {
                *column = to.to_string();
                changed = true;
            }
        }
        if changed {
            row.columns_json =
                serde_json::to_string(&columns).map_err(StorageBackendError::from)?;
        }
        Ok(row)
    }

    pub(super) fn remove_catalog_indexes_for_column(
        &self,
        table: &str,
        column: &str,
    ) -> StorageBackendResult<()> {
        let mut rows = self.durable.catalog_indexes.write();
        let mut removals = Vec::new();
        for (name, row) in rows.iter() {
            if row.table_name == table && Self::catalog_index_references_column(row, column)? {
                removals.push(name.clone());
            }
        }
        for name in removals {
            rows.remove(&name);
        }
        Ok(())
    }

    pub(super) fn rename_catalog_index_table_refs(&self, from: &str, to: &str) {
        for row in self.durable.catalog_indexes.write().values_mut() {
            if row.table_name == from {
                row.table_name = to.to_string();
            }
        }
    }

    pub(super) fn rename_catalog_index_column_refs(
        &self,
        table: &str,
        from: &str,
        to: &str,
    ) -> StorageBackendResult<()> {
        let mut rows = self.durable.catalog_indexes.write();
        let mut updates = Vec::new();
        for (name, row) in rows.iter() {
            if row.table_name == table && Self::catalog_index_references_column(row, from)? {
                let renamed = Self::catalog_index_with_renamed_column(row.clone(), from, to)?;
                updates.push((name.clone(), renamed.columns_json));
            }
        }
        for (name, columns_json) in updates {
            if let Some(row) = rows.get_mut(&name) {
                row.columns_json = columns_json;
            }
        }
        Ok(())
    }

    pub(super) fn ensure_no_dependent_views(
        &self,
        action: &str,
        canonical_name: &str,
    ) -> StorageBackendResult<()> {
        let dependents = self.views_depending_on_relation(canonical_name)?;
        if dependents.is_empty() {
            return Ok(());
        }
        Err(StorageBackendError::Other(format!(
            "{action} `{canonical_name}` rejected: dependent view(s) `{}` use stored relation names that cannot be rewritten safely",
            dependents.join("`, `")
        )))
    }

    pub(crate) fn table_entries(&self) -> Vec<(String, Arc<TableState>)> {
        self.storage
            .tables
            .read()
            .iter()
            .map(|(relation, state)| (relation.qualified_name(), state.clone()))
            .collect()
    }

    pub(super) fn foreign_key_targets(
        foreign_key: &uqa_sql::ast::ForeignKey,
        target: &RelationIdentity,
    ) -> bool {
        stored_relation_reference_matches(&foreign_key.ref_table, target)
    }

    pub(super) fn canonical_foreign_key_target(
        &self,
        reference: &str,
    ) -> StorageBackendResult<String> {
        self.try_resolve_table_name(reference)?
            .ok_or_else(|| table_not_found(reference))
    }

    pub(super) fn canonical_stored_foreign_key_target(
        &self,
        reference: &str,
    ) -> StorageBackendResult<String> {
        let (schema, local_name) =
            RelationIdentity::parse_reference(reference).map_err(|error| {
                StorageBackendError::Other(format!(
                    "invalid persisted foreign-key target `{reference}`: {error}"
                ))
            })?;
        let tables = self.storage.tables.read();
        if let Some(schema) = schema {
            let target = RelationIdentity::new(schema, local_name);
            if tables.contains_key(&target) {
                return Ok(target.qualified_name());
            }
            return Err(StorageBackendError::Other(format!(
                "dangling persisted foreign-key target `{reference}`"
            )));
        }

        let candidates = tables
            .keys()
            .filter(|candidate| candidate.name == local_name)
            .map(RelationIdentity::qualified_name)
            .collect::<Vec<_>>();
        match candidates.as_slice() {
            [target] => Ok(target.clone()),
            [] => Err(StorageBackendError::Other(format!(
                "dangling persisted foreign-key target `{reference}`"
            ))),
            _ => Err(StorageBackendError::Other(format!(
                "ambiguous persisted foreign-key target `{reference}` matches {}",
                candidates.join(", ")
            ))),
        }
    }

    pub(super) fn table_schema_references_relation(
        table: &TableState,
        target: &RelationIdentity,
    ) -> bool {
        table.columns.read().iter().any(|column| {
            column
                .default
                .as_ref()
                .is_some_and(|expr| schema_expr_references_relation(expr, target))
                || column
                    .check
                    .as_ref()
                    .is_some_and(|expr| schema_expr_references_relation(expr, target))
                || column.generated.as_ref().is_some_and(|generated| {
                    schema_expr_references_relation(&generated.expression, target)
                })
        }) || table
            .table_checks
            .read()
            .iter()
            .any(|check| schema_expr_references_relation(&check.expr, target))
    }

    pub(super) fn persist_constraint_candidate(
        &self,
        name: &str,
        table: &TableState,
        columns: &[uqa_sql::ast::ColumnDef],
        checks: &[uqa_sql::ast::TableCheck],
        foreign_keys: &[uqa_sql::ast::ForeignKey],
        key_constraints: &[uqa_sql::ast::TableKeyConstraint],
    ) -> StorageBackendResult<()> {
        self.persist_constraint_candidate_with_hierarchy(
            name,
            table,
            columns,
            checks,
            foreign_keys,
            key_constraints,
            &table.hierarchy.read(),
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "keeps persisted write inputs aligned"
    )]
    pub(super) fn persist_constraint_candidate_with_hierarchy(
        &self,
        name: &str,
        table: &TableState,
        columns: &[uqa_sql::ast::ColumnDef],
        checks: &[uqa_sql::ast::TableCheck],
        foreign_keys: &[uqa_sql::ast::ForeignKey],
        key_constraints: &[uqa_sql::ast::TableKeyConstraint],
        hierarchy: &uqa_sql::ast::TableHierarchy,
    ) -> StorageBackendResult<()> {
        let constraints = uqa_sql::ast::TableConstraintSet {
            persistence: table.persistence,
            on_commit: table.on_commit,
            checks: checks.to_vec(),
            foreign_keys: foreign_keys.to_vec(),
            key_constraints: key_constraints.to_vec(),
            hierarchy: hierarchy.clone(),
        };
        self.try_save_table_schema_with_components(name, table, columns, &constraints)
    }

    pub(super) fn rewrite_table_rename_dependencies(
        &self,
        from: &str,
        to: &str,
    ) -> StorageBackendResult<()> {
        let from_relation = Self::resolved_relation_identity(from)?;
        let mut updates = Vec::new();
        for (table_name, table) in self.table_entries() {
            let mut columns = table.columns.read().clone();
            let mut checks = table.table_checks.read().clone();
            let mut foreign_keys = table.foreign_keys.read().clone();
            let key_constraints = table.key_constraints.read().clone();
            let mut hierarchy = table.hierarchy.read().clone();
            let mut changed = false;

            for column in &mut columns {
                if let Some(owner) = column
                    .auto_increment
                    .as_mut()
                    .and_then(|provenance| provenance.owner.as_mut())
                {
                    if stored_relation_reference_matches(&owner.table, &from_relation) {
                        owner.table = to.to_string();
                        changed = true;
                    }
                }
                for expression in [&mut column.default, &mut column.check]
                    .into_iter()
                    .flatten()
                {
                    if schema_expr_references_relation(expression, &from_relation) {
                        rename_schema_expr_relation(expression, &from_relation, to)?;
                        changed = true;
                    }
                }
                if let Some(generated) = &mut column.generated {
                    if schema_expr_references_relation(&generated.expression, &from_relation) {
                        rename_schema_expr_relation(&mut generated.expression, &from_relation, to)?;
                        changed = true;
                    }
                }
                if let Some(reference) = &mut column.references {
                    if stored_relation_reference_matches(&reference.table, &from_relation) {
                        reference.table = to.to_string();
                        changed = true;
                    }
                }
            }
            for check in &mut checks {
                if schema_expr_references_relation(&check.expr, &from_relation) {
                    rename_schema_expr_relation(&mut check.expr, &from_relation, to)?;
                    changed = true;
                }
            }
            for foreign_key in &mut foreign_keys {
                if Self::foreign_key_targets(foreign_key, &from_relation) {
                    foreign_key.ref_table = to.to_string();
                    changed = true;
                }
            }
            for parent in &mut hierarchy.parents {
                if stored_relation_reference_matches(parent, &from_relation) {
                    *parent = to.to_string();
                    changed = true;
                }
            }
            if changed {
                self.persist_constraint_candidate_with_hierarchy(
                    &table_name,
                    &table,
                    &columns,
                    &checks,
                    &foreign_keys,
                    &key_constraints,
                    &hierarchy,
                )?;
                updates.push((table, columns, checks, foreign_keys, hierarchy));
            }
        }
        for (table, columns, checks, foreign_keys, hierarchy) in updates {
            *table.columns.write() = columns;
            *table.table_checks.write() = checks;
            *table.foreign_keys.write() = foreign_keys;
            *table.hierarchy.write() = hierarchy;
        }
        Ok(())
    }

    pub(super) fn rewrite_column_rename_dependencies(
        &self,
        table_name: &str,
        from: &str,
        to: &str,
    ) -> StorageBackendResult<()> {
        self.rewrite_view_column_references(table_name, from, to)?;
        let target = Self::resolved_relation_identity(table_name)?;
        let mut updates = Vec::new();
        for (candidate_name, table) in self.table_entries() {
            let is_target = candidate_name == table_name;
            let mut columns = table.columns.read().clone();
            let mut checks = table.table_checks.read().clone();
            let mut foreign_keys = table.foreign_keys.read().clone();
            let key_constraints = table.key_constraints.read().clone();
            let mut changed = false;

            for column in &mut columns {
                changed |= Self::rewrite_auto_increment_owner_column(column, &target, from, to);
                for expression in [&mut column.default, &mut column.check]
                    .into_iter()
                    .flatten()
                {
                    if is_target && schema_expr_references_column(expression, from) {
                        rename_schema_expr_column(expression, from, to)?;
                        changed = true;
                    } else if !is_target && schema_expr_references_relation(expression, &target) {
                        rename_schema_expr_qualified_column(expression, &target, from, to)?;
                        changed = true;
                    }
                }
                if let Some(generated) = &mut column.generated {
                    if is_target && schema_expr_references_column(&generated.expression, from) {
                        rename_schema_expr_column(&mut generated.expression, from, to)?;
                        changed = true;
                    } else if !is_target
                        && schema_expr_references_relation(&generated.expression, &target)
                    {
                        rename_schema_expr_qualified_column(
                            &mut generated.expression,
                            &target,
                            from,
                            to,
                        )?;
                        changed = true;
                    }
                }
                if let Some(reference) = &mut column.references {
                    if stored_relation_reference_matches(&reference.table, &target)
                        && reference.column.as_deref() == Some(from)
                    {
                        reference.column = Some(to.to_string());
                        changed = true;
                    }
                }
            }
            for check in &mut checks {
                if is_target && schema_expr_references_column(&check.expr, from) {
                    rename_schema_expr_column(&mut check.expr, from, to)?;
                    changed = true;
                } else if !is_target && schema_expr_references_relation(&check.expr, &target) {
                    rename_schema_expr_qualified_column(&mut check.expr, &target, from, to)?;
                    changed = true;
                }
            }
            for foreign_key in &mut foreign_keys {
                if is_target {
                    for column in &mut foreign_key.local_columns {
                        if column == from {
                            *column = to.to_string();
                            changed = true;
                        }
                    }
                    for column in &mut foreign_key.on_delete_set_columns {
                        if column == from {
                            *column = to.to_string();
                            changed = true;
                        }
                    }
                }
                if Self::foreign_key_targets(foreign_key, &target) {
                    for column in &mut foreign_key.ref_columns {
                        if column == from {
                            *column = to.to_string();
                            changed = true;
                        }
                    }
                }
            }
            if changed {
                self.persist_constraint_candidate(
                    &candidate_name,
                    &table,
                    &columns,
                    &checks,
                    &foreign_keys,
                    &key_constraints,
                )?;
                updates.push((table, columns, checks, foreign_keys));
            }
        }
        for (table, columns, checks, foreign_keys) in updates {
            *table.columns.write() = columns;
            *table.table_checks.write() = checks;
            *table.foreign_keys.write() = foreign_keys;
        }
        Ok(())
    }

    fn rewrite_auto_increment_owner_column(
        column: &mut uqa_sql::ast::ColumnDef,
        target: &RelationIdentity,
        from: &str,
        to: &str,
    ) -> bool {
        let Some(owner) = column
            .auto_increment
            .as_mut()
            .and_then(|provenance| provenance.owner.as_mut())
        else {
            return false;
        };
        if !stored_relation_reference_matches(&owner.table, target) || owner.column != from {
            return false;
        }
        owner.column = to.to_string();
        true
    }

    pub(super) fn preflight_drop_column_dependencies(
        &self,
        table_name: &str,
        column: &str,
    ) -> StorageBackendResult<()> {
        self.ensure_no_dependent_views("ALTER TABLE DROP COLUMN", table_name)?;
        let target = Self::resolved_relation_identity(table_name)?;
        let entries = self.table_entries();
        let target_state = entries
            .iter()
            .find(|(name, _)| name == table_name)
            .map(|(_, state)| state)
            .ok_or_else(|| table_not_found(table_name))?;

        for candidate in target_state.columns.read().iter() {
            if candidate.name == column {
                continue;
            }
            if candidate
                .default
                .as_ref()
                .is_some_and(|expr| schema_expr_references_column(expr, column))
                || candidate.generated.as_ref().is_some_and(|generated| {
                    schema_expr_references_column(&generated.expression, column)
                })
            {
                return Err(StorageBackendError::Other(format!(
                    "ALTER TABLE DROP COLUMN `{table_name}`.`{column}` rejected: column `{}` has a dependent DEFAULT/generation expression",
                    candidate.name
                )));
            }
        }

        let mut inbound = Vec::new();
        for (candidate_name, table) in &entries {
            for foreign_key in table.foreign_keys.read().iter() {
                let local_dependency = candidate_name == table_name
                    && (foreign_key.local_columns.iter().any(|name| name == column)
                        || foreign_key
                            .on_delete_set_columns
                            .iter()
                            .any(|name| name == column));
                let referenced_dependency = Self::foreign_key_targets(foreign_key, &target)
                    && foreign_key.ref_columns.iter().any(|name| name == column);
                if referenced_dependency && !local_dependency {
                    inbound.push(candidate_name.clone());
                }
            }
            for candidate in table.columns.read().iter() {
                if candidate_name == table_name && candidate.name == column {
                    continue;
                }
                if candidate.references.as_ref().is_some_and(|reference| {
                    stored_relation_reference_matches(&reference.table, &target)
                        && reference.column.as_deref() == Some(column)
                }) {
                    inbound.push(candidate_name.clone());
                }
            }
        }
        inbound.sort_unstable();
        inbound.dedup();
        if !inbound.is_empty() {
            return Err(StorageBackendError::Other(format!(
                "ALTER TABLE DROP COLUMN `{table_name}`.`{column}` rejected: referenced by foreign key(s) on `{}`",
                inbound.join("`, `")
            )));
        }
        // Parse every owned index before any mutation so malformed catalog
        // metadata cannot turn a failed drop into a partial in-memory change.
        for row in self.durable.catalog_indexes.read().values() {
            if row.table_name == table_name {
                let _ = Self::catalog_index_references_column(row, column)?;
            }
        }
        Ok(())
    }

    pub(super) fn vector_index_spec_for_column(
        &self,
        table: &str,
        column: &str,
    ) -> StorageBackendResult<Option<VectorIndexSpec>> {
        let mut found = None;
        for row in self.durable.catalog_indexes.read().values() {
            let is_vector_index = row.index_type.eq_ignore_ascii_case("ivf")
                || row.index_type.eq_ignore_ascii_case("hnsw");
            if row.table_name == table
                && is_vector_index
                && Self::catalog_index_references_column(row, column)?
            {
                let parameters: BTreeMap<String, String> =
                    serde_json::from_str(&row.parameters_json)
                        .map_err(StorageBackendError::from)?;
                let spec = if row.index_type.eq_ignore_ascii_case("ivf") {
                    VectorIndexSpec::IVF(IVFIndexParams::from_catalog_map(&parameters)?)
                } else {
                    VectorIndexSpec::HNSW(HNSWIndexParams::from_catalog_map(&parameters)?)
                };
                if found.replace(spec).is_some() {
                    return Err(StorageBackendError::Other(format!(
                        "multiple physical vector indexes target `{table}`.`{column}`"
                    )));
                }
            }
        }
        Ok(found)
    }

    pub(crate) fn vector_catalog_index_names_for_column(
        &self,
        table: &str,
        column: &str,
    ) -> StorageBackendResult<Vec<String>> {
        let mut names = Vec::new();
        for row in self.durable.catalog_indexes.read().values() {
            if row.table_name == table
                && (row.index_type.eq_ignore_ascii_case("ivf")
                    || row.index_type.eq_ignore_ascii_case("hnsw"))
                && Self::catalog_index_references_column(row, column)?
            {
                names.push(row.relation.qualified_name());
            }
        }
        Ok(names)
    }
}
