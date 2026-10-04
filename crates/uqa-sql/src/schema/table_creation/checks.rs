//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The CHECK constraints of a new table, which `DefineRelation` adds once the table's partitioning is set up. `AddRelationNewConstraints` takes them in written order: it transforms each expression, then rejects a name an earlier CHECK took, merges a named CHECK with the inherited constraint of its name or rejects a name another constraint of the table holds, or else chooses a name, and `StoreRelCheck` rejects a NO INHERIT constraint on a partitioned table.

use super::declaration::{validate_check_expression, CreateTableAnalysisContext};
use crate::ast::{CreateTable, DeclaredCheck, TableCheck};
use crate::schema::check_inheritance::{duplicate_check, validate_check_merge};
use crate::schema::constraint_changes::{restore_column_check, take_column_check};
use crate::schema::constraint_metadata::assign_check_name;
use crate::{SQLError, SQLNotice};
use std::collections::BTreeSet;

/// Add the CHECK constraints `table` declares, in written order. `held` names the constraints the table holds before its CHECKs besides the ones it inherits: the keys and foreign keys a partition clones from its parent.
pub fn define_create_table_checks(
    context: &CreateTableAnalysisContext<'_>,
    table: &mut CreateTable,
    held: &BTreeSet<String>,
    notices: &mut Vec<SQLNotice>,
) -> Result<(), SQLError> {
    let relation =
        uqa_core::RelationIdentity::from_legacy_name(&table.name).map_err(SQLError::Internal)?;
    let columns = table.columns.clone();
    let partition = table.hierarchy.is_partition();
    let (mut inherited, local): (Vec<TableCheck>, Vec<TableCheck>) =
        std::mem::take(&mut table.checks)
            .into_iter()
            .partition(|check| !check.is_local);
    let mut local = local.into_iter().map(Some).collect::<Vec<_>>();
    // The inherited CHECKs, which the parents already transformed, are bound to the new table.
    for check in &mut inherited {
        bind_check(context, table, &columns, check)?;
    }
    // `ChooseConstraintName` avoids every constraint of the schema, which by now include the ones the table inherits and clones.
    let mut used = context
        .index_names
        .automatic_constraint_names(&table.name)?;
    used.extend(held.iter().cloned());
    used.extend(inherited.iter().filter_map(|check| check.name.clone()));
    let mut chosen = BTreeSet::new();
    let mut stored = Vec::new();
    for declared in std::mem::take(&mut table.check_order) {
        let (mut check, column) = match declared {
            DeclaredCheck::Column(name) => {
                let index = table
                    .columns
                    .iter()
                    .position(|column| column.name == name)
                    .ok_or_else(|| lost_check(&name))?;
                let check = take_column_check(&mut table.columns[index])
                    .ok_or_else(|| lost_check(&name))?;
                (check, Some(index))
            }
            DeclaredCheck::Table(position) => (
                local
                    .get_mut(position)
                    .and_then(Option::take)
                    .ok_or_else(|| lost_check(&position.to_string()))?,
                None,
            ),
        };
        bind_check(context, table, &columns, &mut check)?;
        let merged = if let Some(name) = check.name.clone() {
            if !chosen.insert(name.clone()) {
                return Err(SQLError::Routine {
                    sqlstate: "42710".into(),
                    message: format!("check constraint \"{name}\" already exists"),
                });
            }
            used.insert(name);
            merge_with_existing(&relation.name, &inherited, held, &check, &columns)?
        } else {
            assign_check_name(&relation.name, &check.expr, &mut check.name, &mut used)
                .map_err(|error| SQLError::Internal(error.to_string()))?;
            chosen.extend(check.name.clone());
            None
        };
        if let Some(name) = merged.and(check.name.as_deref()) {
            notices.push(SQLNotice::notice(format!(
                "merging constraint \"{name}\" with inherited definition"
            )));
        } else if check.no_inherit && table.hierarchy.partition_spec.is_some() {
            return Err(SQLError::Routine {
                sqlstate: "42P16".into(),
                message: format!(
                    "cannot add NO INHERIT constraint to partitioned table \"{}\"",
                    relation.name
                ),
            });
        }
        // A local CHECK merged with the inherited one stands for it; a partition's merged constraints stay inherited.
        if merged.is_some() {
            check.is_local = !partition;
        }
        match (merged, column) {
            (Some(position), Some(index)) => {
                inherited.remove(position);
                restore_column_check(&mut table.columns[index], check);
            }
            (Some(position), None) => inherited[position] = check,
            (None, Some(index)) => restore_column_check(&mut table.columns[index], check),
            (None, None) => stored.push(check),
        }
    }
    if local.iter().any(Option::is_some) {
        return Err(lost_check("a table CHECK"));
    }
    inherited.append(&mut stored);
    table.checks = inherited;
    Ok(())
}

/// Transform a CHECK expression against the new table's columns, as `cookConstraint` does.
fn bind_check(
    context: &CreateTableAnalysisContext<'_>,
    table: &CreateTable,
    columns: &[crate::ast::ColumnDef],
    check: &mut TableCheck,
) -> Result<(), SQLError> {
    validate_check_expression(
        context,
        &table.name,
        &table.qualifier,
        columns,
        &mut check.expr,
    )?;
    crate::catalog::regrole_dependencies::reject_stored_regrole_constants(
        context.schema,
        &check.expr,
        None,
    )
}

/// `MergeWithExistingConstraint`: the position of the inherited CHECK a named local CHECK merges with, or none when the table holds no constraint of its name; a name that another kind of constraint holds is taken.
fn merge_with_existing(
    relation: &str,
    inherited: &[TableCheck],
    held: &BTreeSet<String>,
    check: &TableCheck,
    columns: &[crate::ast::ColumnDef],
) -> Result<Option<usize>, SQLError> {
    let name = check.name.as_deref().unwrap_or_default();
    if let Some(position) = inherited
        .iter()
        .position(|existing| existing.name.as_deref() == Some(name))
    {
        validate_check_merge(relation, &inherited[position], check, columns)?;
        return Ok(Some(position));
    }
    if held.contains(name) {
        return Err(duplicate_check(relation, name));
    }
    Ok(None)
}

fn lost_check(name: &str) -> SQLError {
    SQLError::Internal(format!(
        "the written order of CREATE TABLE CHECKs lost {name}"
    ))
}
