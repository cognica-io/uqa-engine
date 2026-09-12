//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! DDL target resolution, relation dependencies, and catalog index references.

mod regclass;
mod routines;

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
        Ok(
            uqa_sql::schema::columns::alteration::generated_columns_referencing_column(
                &columns, column,
            ),
        )
    }

    pub(crate) fn resolve_table_ddl_target(
        &self,
        name: &str,
        action: &str,
    ) -> StorageBackendResult<Option<String>> {
        uqa_sql::schema::removal::tables::resolved_table_ddl_target(
            self.try_resolve_relation_kind(name)?,
            action,
        )
        .map_err(StorageBackendError::Other)
    }

    pub(super) fn catalog_index_columns(
        row: &CatalogIndexRow,
    ) -> StorageBackendResult<Vec<uqa_sql::ast::IndexKey>> {
        serde_json::from_str(&row.columns_json).map_err(StorageBackendError::from)
    }

    pub(super) fn catalog_index_references_column(
        row: &CatalogIndexRow,
        column: &str,
    ) -> StorageBackendResult<bool> {
        uqa_execution::catalog::index::index_references_column(row, column)
    }

    pub(super) fn catalog_index_with_renamed_column(
        mut row: CatalogIndexRow,
        from: &str,
        to: &str,
    ) -> StorageBackendResult<CatalogIndexRow> {
        let mut columns = Self::catalog_index_columns(&row)?;
        let mut definition = crate::catalog_indexes::index_definition(&row)?;
        if definition.key_names.is_empty() {
            definition.key_names = columns
                .iter()
                .filter_map(uqa_sql::ast::IndexKey::column)
                .chain(definition.included_columns.iter().map(String::as_str))
                .map(str::to_owned)
                .collect();
        }
        for column in &mut columns {
            match column {
                uqa_sql::ast::IndexKey::Column(name) => {
                    if name == from {
                        *name = to.into();
                    }
                }
                uqa_sql::ast::IndexKey::Expression(expression) => {
                    rename_schema_expr_column(expression, from, to)?;
                }
            }
        }
        row.columns_json = serde_json::to_string(&columns)?;
        for column in &mut definition.included_columns {
            if column == from {
                *column = to.into();
            }
        }
        if let Some(predicate) = definition.predicate.as_deref_mut() {
            rename_schema_expr_column(predicate, from, to)?;
        }
        row.definition_json = Some(serde_json::to_string(&definition)?);
        Ok(row)
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
                updates.push((name.clone(), renamed));
            }
        }
        for (name, renamed) in updates {
            if let Some(catalog) = self.storage.catalog.as_ref() {
                catalog.save_catalog_index_row(&renamed)?;
            }
            rows.insert(name, renamed);
        }
        Ok(())
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
        uqa_sql::schema::removal::tables::foreign_key_targets(foreign_key, target)
    }

    pub(crate) fn persist_constraint_candidate(
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
            columns_declared: Some(*table.columns_declared.read()),
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
