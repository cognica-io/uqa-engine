//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Analyze column transforms against the original relation before any ALTER action publishes a new definition.

use super::{ddl_storage_error, identity, TableAlterContext};
use std::collections::BTreeMap;
use uqa_sql::{
    ast::{AlterTableAction, ColumnDef, ColumnType, Expr, GeneratedColumnKind},
    schema::{
        columns::{
            type_target::validate_type_target,
            type_transform::{
                analyze_type_transform, coerce_type_transform, fold_type_transform_assignment,
                AnalyzedTypeTransform,
            },
        },
        SchemaBindingContext,
    },
    SQLError,
};

/// A prepared transform is retained even for an empty table; expression errors belong to preparation, independently of row count.
pub(super) struct PreparedTypeChange {
    pub transform: Option<AnalyzedTypeTransform>,
    pub original_type: ColumnType,
}

mod hierarchy;
mod rewrite;
mod source;
pub(super) use rewrite::TypeRewrites;

pub(super) fn prepare<S: Clone + 'static>(
    context: &TableAlterContext<'_, S>,
    table: &str,
    qualifier: &str,
    actions: &mut [AlterTableAction],
) -> Result<BTreeMap<usize, PreparedTypeChange>, SQLError> {
    prepare_relation(context, table, qualifier, actions, false)
}

fn prepare_relation<S: Clone + 'static>(
    context: &TableAlterContext<'_, S>,
    table: &str,
    qualifier: &str,
    actions: &mut [AlterTableAction],
    recursing: bool,
) -> Result<BTreeMap<usize, PreparedTypeChange>, SQLError> {
    let mut prepared = BTreeMap::new();
    if !actions
        .iter()
        .any(|action| matches!(action, AlterTableAction::AlterColumnType { .. }))
    {
        return Ok(prepared);
    }
    let columns = context
        .hierarchy
        .catalog
        .try_describe_table(table)
        .map_err(|error| ddl_storage_error("ALTER COLUMN TYPE", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let hierarchy = context
        .hierarchy
        .partitions
        .catalog
        .try_table_hierarchy(table)
        .map_err(SQLError::Internal)?;
    let binding = context.columns.analysis.bindings.bindings.binding_scope()?;
    let binding = SchemaBindingContext {
        catalog: context.columns.analysis.bindings.schema,
        binding: &binding.context(),
    };
    for (position, action) in actions.iter_mut().enumerate() {
        let AlterTableAction::AlterColumnType { name, ty, using } = action else {
            continue;
        };
        let mut transform = using
            .as_ref()
            .map(|expression| {
                analyze_type_transform(&binding, table, qualifier, &columns, expression)
            })
            .transpose()?;
        // transformAlterTableStmt resolves an identity column's sequence type before ATPrepAlterColumnType checks inheritance and partition keys.
        if let Some(column) = columns
            .iter()
            .find(|column| column.name == *name)
            .filter(|column| {
                !hierarchy.is_partition()
                    && column
                        .auto_increment
                        .as_ref()
                        .is_some_and(uqa_sql::ast::AutoIncrement::is_identity)
            })
        {
            *ty = uqa_sql::type_resolution::resolve_alter_column_type(
                context.hierarchy.publication.types,
                ty,
            )?;
            identity::validate_identity_type(&context.identities, column, ty)?;
        }
        let inherited = !recursing && inherited_column(context, &hierarchy.parents, name)?;
        let column = validate_type_target(
            table,
            &columns,
            name,
            using.is_some(),
            inherited,
            hierarchy.partition_spec.as_ref(),
        )?;
        *ty = uqa_sql::type_resolution::resolve_alter_column_type(
            context.hierarchy.publication.types,
            ty,
        )?;
        binding.catalog.require_type_usage(ty)?;
        uqa_sql::schema::columns::validate_postgres_relation_column_type(name, ty)?;
        if !is_virtual(column) {
            if transform.is_none() {
                transform = Some(analyze_type_transform(
                    &binding,
                    table,
                    qualifier,
                    &columns,
                    &Expr::Column(name.clone()),
                )?);
            }
            if let Some(transform) = &mut transform {
                coerce_type_transform(&binding, name, ty, transform, using.is_some())?;
                (context.plan_type_transform)(&mut transform.plan.scalar)?;
                fold_type_transform_assignment(context.columns.rewrite.types, ty, transform)?;
            }
        }
        prepared.insert(
            position,
            PreparedTypeChange {
                transform,
                original_type: column.ty.clone(),
            },
        );
    }
    Ok(prepared)
}

fn is_virtual(column: &ColumnDef) -> bool {
    column
        .generated
        .as_ref()
        .is_some_and(|generated| generated.kind == GeneratedColumnKind::Virtual)
}

fn inherited_column<S: Clone + 'static>(
    context: &TableAlterContext<'_, S>,
    parents: &[String],
    name: &str,
) -> Result<bool, SQLError> {
    for parent in parents {
        if context
            .addition
            .state
            .has_column(parent, name)
            .map_err(|error| ddl_storage_error("ALTER COLUMN TYPE", error))?
        {
            return Ok(true);
        }
    }
    Ok(false)
}
