//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Column constraint mutation, key and foreign-key metadata, and identifier allocation.

use super::{
    column_not_found, table_not_found, DocId, Engine, RelationIdentity, SQLError,
    StorageBackendError, StorageBackendResult, TableState,
};
use std::collections::BTreeSet;

const TABLE_NEXT_ID_METADATA_PREFIX: &str = "uqa.table_next_id.v1:";

pub(crate) fn table_next_id_metadata_key(table: &str) -> String {
    format!("{TABLE_NEXT_ID_METADATA_PREFIX}{table}")
}

pub(crate) fn materialize_constraint_metadata(
    relation: &RelationIdentity,
    columns: &mut [uqa_sql::ast::ColumnDef],
    constraints: &mut uqa_sql::ast::TableConstraintSet,
) -> StorageBackendResult<bool> {
    let mut used = BTreeSet::new();
    for column in columns.iter() {
        record_constraint_name(&mut used, column.not_null_name.as_deref())?;
        record_constraint_name(&mut used, column.check_name.as_deref())?;
        record_constraint_name(
            &mut used,
            column
                .references
                .as_ref()
                .and_then(|reference| reference.name.as_deref()),
        )?;
    }
    for constraint in &constraints.key_constraints {
        record_constraint_name(&mut used, constraint.name.as_deref())?;
    }
    for constraint in &constraints.checks {
        record_constraint_name(&mut used, constraint.name.as_deref())?;
    }
    for constraint in &constraints.foreign_keys {
        record_constraint_name(&mut used, constraint.name.as_deref())?;
    }

    let mut changed = false;
    let mut column_object_ids = BTreeSet::new();
    for column in columns.iter_mut() {
        if column
            .object_id
            .is_some_and(|object_id| !column_object_ids.insert(object_id))
        {
            column.object_id = None;
        }
        changed |= assign_catalog_object_id(&mut column.object_id, "column")?;
        if let Some(object_id) = column.object_id {
            column_object_ids.insert(object_id);
        }
        if column.not_null {
            changed |= assign_constraint_name(
                &mut column.not_null_name,
                format!("{}_{}_not_null", relation.name, column.name),
                &mut used,
            )?;
        }
        if column.check.is_some() {
            changed |= assign_constraint_name(
                &mut column.check_name,
                format!("{}_{}_check", relation.name, column.name),
                &mut used,
            )?;
        }
        if let Some(reference) = &mut column.references {
            changed |= assign_constraint_name(
                &mut reference.name,
                format!("{}_{}_fkey", relation.name, column.name),
                &mut used,
            )?;
            changed |= assign_constraint_object_id(&mut reference.object_id)?;
        }
    }
    for constraint in &mut constraints.key_constraints {
        let base = match constraint.kind {
            uqa_sql::ast::TableKeyConstraintKind::PrimaryKey => {
                format!("{}_pkey", relation.name)
            }
            uqa_sql::ast::TableKeyConstraintKind::Unique => format!(
                "{}_{}_key",
                relation.name,
                constraint_column_component(&constraint.columns, relation)?
            ),
        };
        changed |= assign_constraint_name(&mut constraint.name, base, &mut used)?;
    }
    for constraint in &mut constraints.checks {
        let mut referenced_columns = Vec::new();
        collect_constraint_columns(&constraint.expr, &mut referenced_columns);
        let base = if referenced_columns.len() == 1 {
            format!("{}_{}_check", relation.name, referenced_columns[0])
        } else {
            format!("{}_check", relation.name)
        };
        changed |= assign_constraint_name(&mut constraint.name, base, &mut used)?;
    }
    changed |= synchronize_partition_inherited_foreign_key_ids(constraints);
    for constraint in &mut constraints.foreign_keys {
        let component = constraint_column_component(&constraint.local_columns, relation)?;
        changed |= assign_constraint_name(
            &mut constraint.name,
            format!("{}_{}_fkey", relation.name, component),
            &mut used,
        )?;
        changed |= assign_constraint_object_id(&mut constraint.object_id)?;
    }
    changed |= synchronize_partition_inherited_foreign_key_ids(constraints);
    Ok(changed)
}

pub(crate) fn foreign_keys_match_without_object_id(
    left: &uqa_sql::ast::ForeignKey,
    right: &uqa_sql::ast::ForeignKey,
) -> bool {
    let mut left = left.clone();
    let mut right = right.clone();
    left.object_id = None;
    right.object_id = None;
    left == right
}

pub(crate) fn synchronize_partition_inherited_foreign_key_ids(
    constraints: &mut uqa_sql::ast::TableConstraintSet,
) -> bool {
    let mut changed = false;
    for inherited_index in 0..constraints.hierarchy.partition_inherited_foreign_keys.len() {
        let inherited = &constraints.hierarchy.partition_inherited_foreign_keys[inherited_index];
        let Some(foreign_key_index) = constraints
            .foreign_keys
            .iter()
            .position(|foreign_key| foreign_keys_match_without_object_id(foreign_key, inherited))
        else {
            continue;
        };
        let object_id = constraints.foreign_keys[foreign_key_index]
            .object_id
            .or(inherited.object_id);
        if constraints.foreign_keys[foreign_key_index].object_id != object_id {
            constraints.foreign_keys[foreign_key_index].object_id = object_id;
            changed = true;
        }
        if constraints.hierarchy.partition_inherited_foreign_keys[inherited_index].object_id
            != object_id
        {
            constraints.hierarchy.partition_inherited_foreign_keys[inherited_index].object_id =
                object_id;
            changed = true;
        }
    }
    changed
}

fn assign_constraint_object_id(target: &mut Option<[u8; 16]>) -> StorageBackendResult<bool> {
    assign_catalog_object_id(target, "foreign-key constraint")
}

fn assign_catalog_object_id(
    target: &mut Option<[u8; 16]>,
    object_kind: &str,
) -> StorageBackendResult<bool> {
    if target.is_some() {
        return Ok(false);
    }
    let mut object_id = [0_u8; 16];
    getrandom::fill(&mut object_id).map_err(|error| {
        StorageBackendError::Other(format!("allocate {object_kind} object identity: {error}"))
    })?;
    *target = Some(object_id);
    Ok(true)
}

fn record_constraint_name(
    used: &mut BTreeSet<String>,
    name: Option<&str>,
) -> StorageBackendResult<()> {
    let Some(name) = name else {
        return Ok(());
    };
    if name.is_empty() {
        return Err(StorageBackendError::Other(
            "constraint name must not be empty".into(),
        ));
    }
    if !used.insert(name.to_string()) {
        return Err(StorageBackendError::Other(format!(
            "constraint `{name}` is declared more than once"
        )));
    }
    Ok(())
}

fn assign_constraint_name(
    target: &mut Option<String>,
    base: String,
    used: &mut BTreeSet<String>,
) -> StorageBackendResult<bool> {
    if target.is_some() {
        return Ok(false);
    }
    if used.insert(base.clone()) {
        *target = Some(base);
        return Ok(true);
    }
    for suffix in 1_u64.. {
        let candidate = format!("{base}{suffix}");
        if used.insert(candidate.clone()) {
            *target = Some(candidate);
            return Ok(true);
        }
    }
    Err(StorageBackendError::Other(format!(
        "constraint name suffix space exhausted for `{base}`"
    )))
}

fn constraint_column_component(
    columns: &[String],
    relation: &RelationIdentity,
) -> StorageBackendResult<String> {
    if columns.is_empty() {
        return Err(StorageBackendError::Other(format!(
            "constraint on table `{}` has no columns",
            relation.qualified_name()
        )));
    }
    Ok(columns.join("_"))
}

fn collect_constraint_columns(expression: &uqa_sql::ast::Expr, output: &mut Vec<String>) {
    use uqa_sql::ast::{Expr, FrameBound};
    match expression {
        Expr::Column(name) | Expr::QualifiedColumn { column: name, .. } => {
            if !output.contains(name) {
                output.push(name.clone());
            }
        }
        Expr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            for argument in args {
                collect_constraint_columns(argument, output);
            }
            for order in order_by {
                collect_constraint_columns(&order.expr, output);
            }
            if let Some(filter) = filter {
                collect_constraint_columns(filter, output);
            }
        }
        Expr::Array(items) | Expr::Row(items) | Expr::And(items) | Expr::Or(items) => {
            for item in items {
                collect_constraint_columns(item, output);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_constraint_columns(lhs, output);
            collect_constraint_columns(rhs, output);
        }
        Expr::Not(inner)
        | Expr::UnaryMinus(inner)
        | Expr::IsNull { expr: inner, .. }
        | Expr::Cast { expr: inner, .. } => {
            collect_constraint_columns(inner, output);
        }
        Expr::Between { expr, low, high } => {
            collect_constraint_columns(expr, output);
            collect_constraint_columns(low, output);
            collect_constraint_columns(high, output);
        }
        Expr::InList { expr, list, .. } => {
            collect_constraint_columns(expr, output);
            for item in list {
                collect_constraint_columns(item, output);
            }
        }
        Expr::WindowCall { args, spec, .. } => {
            for argument in args {
                collect_constraint_columns(argument, output);
            }
            for expression in &spec.partition_by {
                collect_constraint_columns(expression, output);
            }
            for order in &spec.order_by {
                collect_constraint_columns(&order.expr, output);
            }
            if let Some(frame) = &spec.frame {
                for bound in [&frame.start, &frame.end] {
                    if let FrameBound::Preceding(expression) | FrameBound::Following(expression) =
                        bound
                    {
                        collect_constraint_columns(expression, output);
                    }
                }
            }
        }
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base {
                collect_constraint_columns(base, output);
            }
            for (condition, result) in when {
                collect_constraint_columns(condition, output);
                collect_constraint_columns(result, output);
            }
            if let Some(else_branch) = else_branch {
                collect_constraint_columns(else_branch, output);
            }
        }
        Expr::InSubquery { expr, .. } => collect_constraint_columns(expr, output),
        Expr::Default
        | Expr::Star
        | Expr::QualifiedStar(_)
        | Expr::InternalColumn(_)
        | Expr::Literal(_)
        | Expr::Param(_)
        | Expr::ScalarSubquery(_)
        | Expr::Exists { .. } => {}
    }
}

mod engine;
