//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Apply all transforms to one retained original row and publish each affected relation once.

use super::{hierarchy, prepare_relation, PreparedTypeChange, TableAlterContext};
use crate::schema::columns::{
    addition::deferred::AddedColumnValue,
    alteration, generated,
    rows::{self, RewriteRows},
};
use std::collections::BTreeMap;
use uqa_core::{memory::MemoryBudget, Value};
use uqa_sql::{
    ast::{AlterTableAction, ColumnType, GeneratedColumnKind},
    schema::columns::{type_target, type_transform::assign_type_transform_value},
    SQLError,
};

struct ColumnTransform {
    name: String,
    target: ColumnType,
    prepared: PreparedTypeChange,
    generated: Option<GeneratedColumnKind>,
}

struct RelationRewrite {
    table: String,
    qualifier: String,
    prepared: BTreeMap<usize, PreparedTypeChange>,
    transforms: Vec<ColumnTransform>,
    original: super::source::OriginalRows,
}

pub(in crate::schema::table_alteration) struct TypeRewrites {
    relations: Vec<RelationRewrite>,
    memory: MemoryBudget,
}

impl TypeRewrites {
    pub(in crate::schema::table_alteration) fn capture<S: Clone + 'static>(
        context: &TableAlterContext<'_, S>,
        table: &str,
        recurse: bool,
        mode: crate::row_locks::RelationLockMode,
        actions: &[AlterTableAction],
        prepared: BTreeMap<usize, PreparedTypeChange>,
    ) -> Result<Option<Self>, SQLError> {
        if prepared.is_empty() {
            return Ok(None);
        }
        let names = hierarchy::lock_relations(context, table, recurse, mode, actions)?;
        let allowance = context
            .columns
            .generated
            .keys
            .constraints
            .memory
            .work_mem_bytes()?;
        let memory = MemoryBudget::new(allowance);
        // Capture partitions the one allowance; callback reconciliation later uses the parent after the original buffers have spilled.
        let row_memory = memory.child(allowance / 2);
        let read_memory = memory.child(allowance - allowance / 2);
        let control = uqa_storage::read_control::StorageReadControl::new(
            &read_memory,
            context.columns.rewrite.cancellation,
        );
        let mut root_prepared = Some(prepared);
        let mut relations = Vec::new();
        for name in &names {
            let qualifier = uqa_core::RelationIdentity::from_legacy_name(name)
                .map_err(SQLError::Internal)?
                .name;
            let prepared = if let Some(prepared) = root_prepared.take() {
                prepared
            } else {
                hierarchy::validate_parents(context, name, &names, actions)?;
                prepare_relation(context, name, &qualifier, &mut actions.to_vec(), true)?
            };
            relations.push(RelationRewrite {
                table: name.clone(),
                qualifier,
                prepared,
                transforms: Vec::new(),
                original: super::source::OriginalRows {
                    columns: context
                        .columns
                        .analysis
                        .state
                        .stored_columns(name)
                        .map_err(|error| {
                            uqa_sql::catalog::errors::storage_error(
                                "rewrite original columns",
                                error.as_ref(),
                            )
                        })?,
                    generation: context.row_changes.storage_generation(name)?,
                    rows: RewriteRows::new(&row_memory),
                },
            });
        }
        context.binding.locks.prepare_definition_write()?;
        for relation in &mut relations {
            relation.original.rows = rows::capture(
                context.columns.rewrite.reads,
                &relation.table,
                &row_memory,
                &control,
            )?;
        }
        Ok(Some(Self { relations, memory }))
    }

    pub(in crate::schema::table_alteration) fn apply<S: Clone + 'static>(
        &mut self,
        context: &TableAlterContext<'_, S>,
        position: usize,
        name: &str,
        target: &ColumnType,
    ) -> Result<(), SQLError> {
        for relation in &mut self.relations {
            context
                .constraints
                .access
                .ensure_no_pending_events(&relation.table, "ALTER TABLE")?;
            let prepared = relation
                .prepared
                .remove(&position)
                .ok_or_else(|| SQLError::Internal("column transform was not prepared".into()))?;
            let current = context
                .columns
                .analysis
                .state
                .column_type(&relation.table, name)
                .map_err(|error| {
                    uqa_sql::catalog::errors::storage_error("ALTER COLUMN TYPE", error.as_ref())
                })?
                .ok_or_else(|| {
                    uqa_sql::schema::columns::undefined_relation_column(&relation.table, name)
                })?;
            type_target::validate_repeated_type_change(name, &prepared.original_type, &current)?;
            super::identity::retype_identity_sequence(context, &relation.table, name, target)?;
            let generated = alteration::begin_type_change(
                &context.columns,
                &relation.table,
                &relation.qualifier,
                name,
                target,
            )?;
            relation.transforms.push(ColumnTransform {
                name: name.to_string(),
                target: target.clone(),
                prepared,
                generated,
            });
        }
        Ok(())
    }

    pub(in crate::schema::table_alteration) fn finish<S: Clone + 'static>(
        mut self,
        context: &TableAlterContext<'_, S>,
    ) -> Result<(), SQLError> {
        let defaults = context.addition.pending_rows.take();
        for relation in &mut self.relations {
            uqa_sql::schema::columns::alteration::validate_column_type_constraints(
                &context.columns.analysis,
                &relation.table,
                &relation.qualifier,
            )?;
            // The key sorter receives the statement allowance after all retained input buffers have released it.
            relation.original.rows.spill()?;
        }
        let marker = rows::changes::marker(context.row_changes)?;
        for relation in &mut self.relations {
            relation
                .original
                .reconcile(context, &relation.table, marker, &self.memory)?;
            relation.rewrite(context, &defaults, &self.memory)?;
        }
        for relation in &self.relations {
            let changed = relation
                .transforms
                .iter()
                .map(|transform| transform.name.clone())
                .chain(
                    context
                        .columns
                        .deferred_rows
                        .physical_columns(&relation.table),
                )
                .chain(
                    context
                        .addition
                        .pending_rows
                        .deferral
                        .physical_columns(&relation.table),
                )
                .collect::<Vec<_>>();
            crate::schema::validation::validate_rewritten_foreign_keys(
                context.columns.generated.keys.constraints,
                &relation.table,
                &changed,
            )?;
        }
        context
            .constraints
            .pending_foreign_keys
            .validate(context.constraints.rows)?;
        Ok(())
    }
}

impl RelationRewrite {
    fn generated_rewrite_columns<S: Clone + 'static>(
        &self,
        context: &TableAlterContext<'_, S>,
    ) -> Vec<String> {
        self.transforms
            .iter()
            .filter(|transform| transform.generated == Some(GeneratedColumnKind::Stored))
            .map(|transform| transform.name.clone())
            .chain(context.columns.deferred_rows.physical_columns(&self.table))
            .chain(
                context
                    .addition
                    .pending_rows
                    .deferral
                    .physical_columns(&self.table),
            )
            .collect()
    }

    fn rewrite<S: Clone + 'static>(
        &mut self,
        context: &TableAlterContext<'_, S>,
        defaults: &[AddedColumnValue],
        memory: &MemoryBudget,
    ) -> Result<(), SQLError> {
        let columns = context
            .columns
            .analysis
            .state
            .stored_columns(&self.table)
            .map_err(|error| {
                uqa_sql::catalog::errors::storage_error("ALTER COLUMN TYPE", error.as_ref())
            })?;
        let mut output = RewriteRows::new(memory);
        let write = self
            .transforms
            .iter()
            .any(|transform| transform.generated != Some(GeneratedColumnKind::Virtual))
            || defaults.iter().any(|default| default.table == self.table)
            || context
                .columns
                .deferred_rows
                .needs_physical_rewrite(&self.table)
            || context
                .addition
                .pending_rows
                .deferral
                .needs_physical_rewrite(&self.table);
        let generated_columns = self.generated_rewrite_columns(context);
        let assignment = context.columns.generated.assignment;
        let scope = assignment.scopes.current_routine_scope();
        for position in 0..self.original.rows.len() {
            context.columns.rewrite.cancellation.check()?;
            let original = self.original.rows.get(position)?;
            let mut document = original.document.clone();
            document.retain(|name, _| columns.iter().any(|column| column.name == *name));
            for transform in &self.transforms {
                let Some(analyzed) = &transform.prepared.transform else {
                    continue;
                };
                let value = crate::query::catalog_expression::eval_expression_plan_with_schema(
                    assignment.expressions.expressions,
                    scope.clone(),
                    analyzed.plan.clone(),
                    &original.document,
                    analyzed.row_schema(),
                    &[],
                )?;
                document.insert(
                    transform.name.clone(),
                    assign_type_transform_value(
                        context.columns.rewrite.types,
                        value,
                        &transform.target,
                        analyzed.source_type.as_ref(),
                    )?,
                );
            }
            for default in defaults
                .iter()
                .filter(|default| default.table == self.table)
            {
                document.insert(
                    default.column.clone(),
                    default.evaluate(&context.addition.backfill)?,
                );
            }
            for column in &columns {
                if column.generated.is_none() && !document.contains_key(&column.name) {
                    document.insert(column.name.clone(), Value::Null);
                }
            }
            crate::mutation::assignment::refresh_selected_stored_generated_columns(
                assignment,
                &self.table,
                &mut document,
                Some(&generated_columns),
            )?;
            crate::mutation::constraints::validate_rewritten_row(
                context.columns.generated.keys.constraints,
                &self.table,
                &document,
            )?;
            output.push(original.original_id, document)?;
        }
        self.original.rows.spill()?;
        generated::publish_validated_rows(&context.columns.generated, &self.table, output, write)?;
        if write {
            context
                .addition
                .backfill
                .state
                .clear_missing_values(&self.table)?;
        }
        for transform in &self.transforms {
            alteration::finish_type_change(
                &context.columns,
                &self.table,
                &transform.name,
                &transform.target,
                transform.generated,
            )?;
        }
        Ok(())
    }
}
