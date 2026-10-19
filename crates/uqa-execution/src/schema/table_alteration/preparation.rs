//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepare primary-key requirements and column transforms in written order against the original relation.

use super::{ddl_storage_error, identity, TableAlterContext};
use std::collections::BTreeMap;
use uqa_sql::{
    ast::{
        AlterTableAction, ColumnDef, ColumnType, Expr, GeneratedColumnKind, TableHierarchy,
        TableKeyConstraintKind,
    },
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
    pub collation_oid: u32,
}

pub(super) struct PreparedAlter {
    pub types: BTreeMap<usize, PreparedTypeChange>,
    pub primary_keys: BTreeMap<usize, Vec<String>>,
}

pub(super) fn prepare<S: Clone + 'static>(
    context: &TableAlterContext<'_, S>,
    table: &str,
    qualifier: &str,
    actions: &mut [AlterTableAction],
    recurse: bool,
    mode: crate::row_locks::RelationLockMode,
) -> Result<PreparedAlter, SQLError> {
    let mut primary_keys = BTreeMap::new();
    let types = prepare_actions(
        context,
        table,
        qualifier,
        actions,
        false,
        |columns, position, action| {
            if let AlterTableAction::AddKeyConstraint { constraint } = action {
                if constraint.kind == TableKeyConstraintKind::PrimaryKey {
                    primary_keys.insert(
                        position,
                        super::primary_keys::prepare(
                            context, table, columns, constraint, recurse, mode,
                        )?,
                    );
                }
            }
            Ok(())
        },
    )?;
    Ok(PreparedAlter {
        types,
        primary_keys,
    })
}

pub(super) fn prepare_relation<S: Clone + 'static>(
    context: &TableAlterContext<'_, S>,
    table: &str,
    qualifier: &str,
    actions: &mut [AlterTableAction],
    recursing: bool,
) -> Result<BTreeMap<usize, PreparedTypeChange>, SQLError> {
    prepare_actions(context, table, qualifier, actions, recursing, |_, _, _| {
        Ok(())
    })
}

fn prepare_actions<S: Clone + 'static>(
    context: &TableAlterContext<'_, S>,
    table: &str,
    qualifier: &str,
    actions: &mut [AlterTableAction],
    recursing: bool,
    mut prepare_key: impl FnMut(&[ColumnDef], usize, &AlterTableAction) -> Result<(), SQLError>,
) -> Result<BTreeMap<usize, PreparedTypeChange>, SQLError> {
    let mut prepared = BTreeMap::new();
    if !actions.iter().any(|action| {
        matches!(action, AlterTableAction::AlterColumnType { .. })
            || (!recursing && matches!(action, AlterTableAction::AddKeyConstraint { constraint } if constraint.kind == TableKeyConstraintKind::PrimaryKey))
    }) {
        return Ok(prepared);
    }
    let columns = context
        .hierarchy
        .catalog
        .try_describe_table(table)
        .map_err(|error| ddl_storage_error("ALTER TABLE", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let first_type = actions
        .iter()
        .position(|action| matches!(action, AlterTableAction::AlterColumnType { .. }))
        .unwrap_or(actions.len());
    let (preceding, remaining) = actions.split_at_mut(first_type);
    if !recursing {
        for (position, action) in preceding.iter().enumerate() {
            prepare_key(&columns, position, action)?;
        }
    }
    if remaining.is_empty() {
        return Ok(prepared);
    }
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
    let relation = TypeChangeRelation {
        table,
        qualifier,
        columns: &columns,
        hierarchy: &hierarchy,
        recursing,
    };
    for (offset, action) in remaining.iter_mut().enumerate() {
        if !recursing {
            prepare_key(&columns, first_type + offset, action)?;
        }
        if let Some(change) = prepare_type_change(context, relation, &binding, action)? {
            prepared.insert(first_type + offset, change);
        }
    }
    Ok(prepared)
}

#[derive(Clone, Copy)]
struct TypeChangeRelation<'a> {
    table: &'a str,
    qualifier: &'a str,
    columns: &'a [ColumnDef],
    hierarchy: &'a TableHierarchy,
    recursing: bool,
}

fn prepare_type_change<S: Clone + 'static>(
    context: &TableAlterContext<'_, S>,
    relation: TypeChangeRelation<'_>,
    binding: &SchemaBindingContext<'_, '_>,
    action: &mut AlterTableAction,
) -> Result<Option<PreparedTypeChange>, SQLError> {
    let TypeChangeRelation {
        table,
        qualifier,
        columns,
        hierarchy,
        recursing,
    } = relation;
    let AlterTableAction::AlterColumnType {
        name,
        ty,
        collation,
        using,
    } = action
    else {
        return Ok(None);
    };
    let mut transform = using
        .as_ref()
        .map(|expression| analyze_type_transform(binding, table, qualifier, columns, expression))
        .transpose()?;
    identity::resolve_identity_column_type(context, hierarchy.is_partition(), columns, name, ty)?;
    let inherited = !recursing && inherited_column(context, &hierarchy.parents, name)?;
    let column = validate_type_target(
        table,
        columns,
        name,
        using.is_some(),
        inherited,
        hierarchy.partition_spec.as_ref(),
    )?;
    *ty = uqa_sql::type_resolution::resolve_alter_column_type(
        context.hierarchy.publication.types,
        ty,
    )?;
    let collation_oid = uqa_sql::catalog::collations::declarations::select(
        binding.catalog,
        ty,
        collation.as_deref(),
    )?;
    binding.catalog.require_type_usage(ty)?;
    uqa_sql::schema::columns::validate_postgres_relation_column_type(name, ty)?;
    if !is_virtual(column) {
        if transform.is_none() {
            transform = Some(analyze_type_transform(
                binding,
                table,
                qualifier,
                columns,
                &Expr::Column(name.clone()),
            )?);
        }
        if let Some(transform) = &mut transform {
            coerce_type_transform(binding, name, ty, transform, using.is_some())?;
            (context.plan_type_transform)(&mut transform.plan.scalar)?;
            fold_type_transform_assignment(context.columns.rewrite.types, ty, transform)?;
        }
    }
    Ok(Some(PreparedTypeChange {
        transform,
        original_type: column.ty.clone(),
        collation_oid,
    }))
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
