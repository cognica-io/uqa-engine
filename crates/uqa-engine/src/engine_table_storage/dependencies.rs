//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! DDL target resolution, relation dependencies, and catalog index references.

mod routines;

use super::{
    rename_schema_expr_column, rename_schema_expr_qualified_column, rename_schema_expr_relation,
    rewrite_sequence_function_references, schema_expr_references_column,
    schema_expr_references_relation, stored_relation_reference_matches, table_not_found, Arc,
    BTreeMap, CatalogIndexRow, Engine, IVFIndexParams, RelationIdentity, StorageBackendError,
    StorageBackendResult, TableState,
};
use crate::engine_sequences::SequenceSchemaDependent;
use crate::{HNSWIndexParams, VectorIndexSpec};

fn expression_references_sequence(
    engine: &Engine,
    expression: Option<&uqa_sql::ast::Expr>,
    sequence: &str,
) -> StorageBackendResult<bool> {
    expression.map_or(Ok(false), |expression| {
        Ok(engine
            .stored_sequence_targets_in_loaded_expr(expression)?
            .contains(sequence))
    })
}

fn rewrite_sequence_schema_references(
    columns: &mut [uqa_sql::ast::ColumnDef],
    checks: &mut [uqa_sql::ast::TableCheck],
    from: &RelationIdentity,
    to: &str,
) -> StorageBackendResult<bool> {
    let mut changed = false;
    for column in columns {
        if let Some(sequence) = column
            .auto_increment
            .as_mut()
            .and_then(|provenance| provenance.sequence.as_mut())
        {
            if stored_relation_reference_matches(sequence, from) {
                *sequence = to.to_string();
                changed = true;
            }
        }
        for expression in [&mut column.default, &mut column.check]
            .into_iter()
            .flatten()
        {
            rewrite_sequence_function_references(expression, &mut |reference| {
                if stored_relation_reference_matches(reference, from) {
                    *reference = to.to_string();
                    changed = true;
                }
                Ok(())
            })?;
        }
        if let Some(generated) = &mut column.generated {
            rewrite_sequence_function_references(&mut generated.expression, &mut |reference| {
                if stored_relation_reference_matches(reference, from) {
                    *reference = to.to_string();
                    changed = true;
                }
                Ok(())
            })?;
        }
    }
    for check in checks {
        rewrite_sequence_function_references(&mut check.expr, &mut |reference| {
            if stored_relation_reference_matches(reference, from) {
                *reference = to.to_string();
                changed = true;
            }
            Ok(())
        })?;
    }
    Ok(changed)
}

fn append_sequence_schema_expression_dependents(
    engine: &Engine,
    table_name: &str,
    columns: &[uqa_sql::ast::ColumnDef],
    checks: &[uqa_sql::ast::TableCheck],
    sequence: &str,
    foreign: bool,
    dependents: &mut Vec<SequenceSchemaDependent>,
) -> StorageBackendResult<()> {
    let relation = if foreign {
        format!("foreign table `{table_name}`")
    } else {
        format!("`{table_name}`")
    };
    for column in columns {
        if expression_references_sequence(engine, column.default.as_ref(), sequence)? {
            dependents.push(SequenceSchemaDependent::Default {
                table: table_name.to_string(),
                column: column.name.clone(),
                foreign,
            });
        }
        if expression_references_sequence(
            engine,
            column
                .generated
                .as_ref()
                .map(|generated| generated.expression.as_ref()),
            sequence,
        )? {
            dependents.push(SequenceSchemaDependent::GeneratedColumn {
                table: table_name.to_string(),
                column: column.name.clone(),
                foreign,
            });
        }
        if expression_references_sequence(engine, column.check.as_ref(), sequence)? {
            dependents.push(SequenceSchemaDependent::CheckConstraint {
                table: table_name.to_string(),
                constraint: column.check_name.clone().ok_or_else(|| {
                    StorageBackendError::Other(format!(
                        "CHECK constraint on {relation}.`{}` has no catalog name",
                        column.name
                    ))
                })?,
                foreign,
            });
        }
    }
    for check in checks {
        if expression_references_sequence(engine, Some(&check.expr), sequence)? {
            dependents.push(SequenceSchemaDependent::CheckConstraint {
                table: table_name.to_string(),
                constraint: check.name.clone().ok_or_else(|| {
                    StorageBackendError::Other(format!(
                        "table CHECK constraint on {relation} has no catalog name"
                    ))
                })?,
                foreign,
            });
        }
    }
    Ok(())
}

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

    pub(crate) fn bind_sequence_references_in_expr(
        &self,
        expression: &mut uqa_sql::ast::Expr,
    ) -> StorageBackendResult<()> {
        rewrite_sequence_function_references(expression, &mut |reference| {
            *reference = self.resolve_sequence_reference_for_binding(reference)?;
            Ok(())
        })
    }

    pub(super) fn resolve_stored_sequence_references_in_expr(
        &self,
        expression: &mut uqa_sql::ast::Expr,
    ) -> StorageBackendResult<()> {
        let mut refreshed = false;
        rewrite_sequence_function_references(expression, &mut |reference| {
            if !refreshed {
                self.refresh_sequences_from_catalog()?;
                refreshed = true;
            }
            *reference = self.resolve_stored_sequence_reference_from_loaded_registry(reference)?;
            Ok(())
        })
    }

    pub(crate) fn resolve_loaded_sequence_references_in_expr(
        &self,
        expression: &mut uqa_sql::ast::Expr,
    ) -> StorageBackendResult<()> {
        rewrite_sequence_function_references(expression, &mut |reference| {
            *reference = self.resolve_stored_sequence_reference_from_loaded_registry(reference)?;
            Ok(())
        })
    }

    fn stored_sequence_targets_in_loaded_expr(
        &self,
        expression: &uqa_sql::ast::Expr,
    ) -> StorageBackendResult<std::collections::BTreeSet<String>> {
        let mut expression = expression.clone();
        let mut targets = std::collections::BTreeSet::new();
        rewrite_sequence_function_references(&mut expression, &mut |reference| {
            let canonical =
                self.resolve_stored_sequence_reference_from_loaded_registry(reference)?;
            targets.insert(canonical.clone());
            *reference = canonical;
            Ok(())
        })?;
        Ok(targets)
    }

    pub(crate) fn sequence_schema_expression_dependents(
        &self,
        sequence: &str,
    ) -> StorageBackendResult<Vec<SequenceSchemaDependent>> {
        self.synchronize_table_catalog()?;
        self.synchronize_catalog_registries()?;
        self.refresh_sequences_from_catalog()?;
        let mut dependents = Vec::new();
        for (table_name, table) in self.table_entries() {
            append_sequence_schema_expression_dependents(
                self,
                &table_name,
                &table.columns.read(),
                &table.table_checks.read(),
                sequence,
                false,
                &mut dependents,
            )?;
        }
        for (relation, table) in self.durable.foreign_tables.read().iter() {
            let table_name = relation.qualified_name();
            append_sequence_schema_expression_dependents(
                self,
                &table_name,
                &table.columns,
                &table.checks,
                sequence,
                true,
                &mut dependents,
            )?;
        }
        dependents.sort();
        dependents.dedup();
        Ok(dependents)
    }

    pub(crate) fn ensure_no_sequence_schema_dependencies(
        &self,
        sequence: &str,
    ) -> StorageBackendResult<()> {
        let dependents = self.sequence_schema_expression_dependents(sequence)?;
        if dependents.is_empty() {
            return Ok(());
        }
        Err(StorageBackendError::Other(format!(
            "column schema expression(s) `{}` depend on sequence `{sequence}`",
            dependents
                .iter()
                .map(SequenceSchemaDependent::object_label)
                .collect::<Vec<_>>()
                .join("`, `")
        )))
    }

    pub(crate) fn rewrite_sequence_schema_dependencies(
        &self,
        from: &RelationIdentity,
        to: &str,
    ) -> StorageBackendResult<()> {
        let mut table_updates = Vec::new();
        for (table_name, table) in self.table_entries() {
            let mut columns = table.columns.read().clone();
            let mut checks = table.table_checks.read().clone();
            let foreign_keys = table.foreign_keys.read().clone();
            let key_constraints = table.key_constraints.read().clone();
            let hierarchy = table.hierarchy.read().clone();
            if rewrite_sequence_schema_references(&mut columns, &mut checks, from, to)? {
                table_updates.push((
                    table_name,
                    table,
                    columns,
                    checks,
                    foreign_keys,
                    key_constraints,
                    hierarchy,
                ));
            }
        }
        let mut foreign_updates = Vec::new();
        for (relation, mut table) in self.durable.foreign_tables.read().clone() {
            if rewrite_sequence_schema_references(&mut table.columns, &mut table.checks, from, to)?
            {
                foreign_updates.push((relation, table));
            }
        }
        for (table_name, table, columns, checks, foreign_keys, key_constraints, hierarchy) in
            &table_updates
        {
            self.persist_constraint_candidate_with_hierarchy(
                table_name,
                table,
                columns,
                checks,
                foreign_keys,
                key_constraints,
                hierarchy,
            )?;
        }
        for (relation, table) in &foreign_updates {
            self.persist_foreign_table_definition(relation, table)?;
        }
        for (_, table, columns, checks, _, _, _) in &table_updates {
            (*table.columns.write()).clone_from(columns);
            (*table.table_checks.write()).clone_from(checks);
        }
        let foreign_tables_changed = !foreign_updates.is_empty();
        if foreign_tables_changed {
            let mut tables = self.durable.foreign_tables.write();
            for (relation, table) in foreign_updates {
                tables.insert(relation, table);
            }
        }
        if !table_updates.is_empty() {
            self.note_table_catalog_changed();
        }
        if foreign_tables_changed {
            self.note_catalog_registry_changed();
        }
        Ok(())
    }

    /// Retire the legacy name-based owner marker after an explicit `ALTER SEQUENCE ... OWNED BY` action. Generation provenance and the bound sequence reference remain on the original SERIAL or identity column, while the stable dependency stored on the sequence becomes authoritative for lifecycle operations.
    pub(crate) fn clear_auto_increment_owner_markers(
        &self,
        sequence: &str,
    ) -> StorageBackendResult<()> {
        let target =
            RelationIdentity::from_legacy_name(sequence).map_err(StorageBackendError::Other)?;
        let mut catalog_changed = false;
        for (table_name, table) in self.table_entries() {
            let mut columns = table.columns.read().clone();
            let mut changed = false;
            for column in &mut columns {
                let Some(provenance) = column.auto_increment.as_mut() else {
                    continue;
                };
                if provenance.owner.is_some()
                    && provenance.sequence.as_deref().is_some_and(|reference| {
                        stored_relation_reference_matches(reference, &target)
                    })
                {
                    provenance.owner = None;
                    changed = true;
                }
            }
            if !changed {
                continue;
            }
            if self.is_persistent() {
                self.try_save_table_schema_with_columns(&table_name, &table, &columns)?;
            }
            *table.columns.write() = columns;
            catalog_changed = true;
        }
        if catalog_changed {
            self.note_table_catalog_changed();
        }
        let mut foreign_updates = Vec::new();
        for (relation, mut table) in self.durable.foreign_tables.read().clone() {
            let mut changed = false;
            for column in &mut table.columns {
                let Some(provenance) = column.auto_increment.as_mut() else {
                    continue;
                };
                if provenance.owner.is_some()
                    && provenance.sequence.as_deref().is_some_and(|reference| {
                        stored_relation_reference_matches(reference, &target)
                    })
                {
                    provenance.owner = None;
                    changed = true;
                }
            }
            if changed {
                foreign_updates.push((relation, table));
            }
        }
        for (relation, table) in &foreign_updates {
            self.persist_foreign_table_definition(relation, table)?;
        }
        if !foreign_updates.is_empty() {
            let mut tables = self.durable.foreign_tables.write();
            for (relation, table) in foreign_updates {
                tables.insert(relation, table);
            }
            drop(tables);
            self.note_catalog_registry_changed();
        }
        Ok(())
    }

    fn drop_sequence_schema_dependencies(
        &self,
        dependencies: &[SequenceSchemaDependent],
    ) -> StorageBackendResult<()> {
        for dependency in dependencies {
            let SequenceSchemaDependent::CheckConstraint {
                table,
                constraint,
                foreign,
            } = dependency
            else {
                continue;
            };
            if *foreign {
                if self.drop_foreign_table_check_dependency(table, constraint)? != Some(true) {
                    return Err(StorageBackendError::Other(format!(
                        "constraint `{constraint}` on foreign table `{table}` disappeared after sequence DROP preflight"
                    )));
                }
            } else {
                crate::sql::drop_constraint_dependency(self, table, constraint)
                    .map_err(|error| StorageBackendError::Other(error.to_string()))?;
            }
        }
        for dependency in dependencies {
            let SequenceSchemaDependent::Default {
                table,
                column,
                foreign,
            } = dependency
            else {
                continue;
            };
            let dropped = if *foreign {
                self.clear_foreign_table_default_dependency(table, column)? == Some(true)
            } else {
                self.set_column_default_inner(table, column, None)?
            };
            if !dropped {
                return Err(StorageBackendError::Other(format!(
                    "default `{table}`.`{column}` disappeared after sequence DROP preflight"
                )));
            }
        }
        for dependency in dependencies {
            let SequenceSchemaDependent::GeneratedColumn {
                table,
                column,
                foreign,
            } = dependency
            else {
                continue;
            };
            let dropped = if *foreign {
                self.drop_foreign_table_generated_column_dependency(table, column)? == Some(true)
            } else {
                self.try_drop_column_inner(table, column)?
            };
            if !dropped {
                return Err(StorageBackendError::Other(format!(
                    "generated column `{table}`.`{column}` disappeared after sequence DROP preflight"
                )));
            }
        }
        Ok(())
    }

    /// Remove schema-expression dependencies requested by `DROP SEQUENCE CASCADE` and always detach serial ownership metadata whose sequence is being removed.
    pub(crate) fn detach_sequence_column_dependencies(
        &self,
        sequence: &str,
        cascade: bool,
    ) -> StorageBackendResult<()> {
        let dependencies = self.sequence_schema_expression_dependents(sequence)?;
        if !cascade && !dependencies.is_empty() {
            self.ensure_no_sequence_schema_dependencies(sequence)?;
        }
        if cascade {
            self.drop_sequence_schema_dependencies(&dependencies)?;
        }
        let mut catalog_changed = false;
        for (table_name, table) in self.table_entries() {
            let mut columns = table.columns.read().clone();
            let mut table_changed = false;
            for column in &mut columns {
                if column
                    .auto_increment
                    .as_ref()
                    .is_some_and(|provenance| provenance.sequence.as_deref() == Some(sequence))
                {
                    column.auto_increment = None;
                    table_changed = true;
                }
            }
            if !table_changed {
                continue;
            }
            if self.is_persistent() {
                self.try_save_table_schema_with_columns(&table_name, &table, &columns)?;
            }
            *table.columns.write() = columns;
            catalog_changed = true;
        }
        if catalog_changed {
            self.note_table_catalog_changed();
        }
        self.detach_foreign_table_sequence_provenance(sequence)?;
        Ok(())
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
        self.ensure_no_dependent_views("ALTER TABLE RENAME", from)?;
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
        self.ensure_no_dependent_views("ALTER TABLE RENAME COLUMN", table_name)?;
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
