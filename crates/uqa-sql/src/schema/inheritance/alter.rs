//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! ALTER inheritance and partition declaration rules over immutable column and constraint definitions.
use crate::ast::{
    AutoIncrement, BinaryOp, ColumnDef, DetachedPartitionConstraint, Expr, PartitionBound,
    PartitionIdentityOverride, PartitionRangeDatum, PartitionSpec, RelationPersistence, TableCheck,
    TableHierarchy, TableKeyConstraint,
};
use crate::SQLError;
use uqa_core::Value;
pub fn validate_row_type(
    parent_columns: &[ColumnDef],
    child_columns: &[ColumnDef],
    parent: &str,
    child: &str,
    exact_columns: bool,
    reject_child_identity: bool,
) -> Result<(), SQLError> {
    validate_attached_columns(
        parent_columns,
        child_columns,
        parent,
        child,
        exact_columns,
        reject_child_identity,
    )?;
    for parent_column in parent_columns {
        let Some(child_column) = child_columns
            .iter()
            .find(|column| column.name == parent_column.name)
        else {
            return Err(routine(
                "42804",
                format!("child table is missing column \"{}\"", parent_column.name),
            ));
        };
        validate_inherited_column(parent_column, child_column, child)?;
    }
    Ok(())
}

/// Check the columns of a table being attached as a partition in their order, as `ATExecAttachPartition` does before it merges the parent's columns: the partition may contain no identity column (`reject_child_identity`) and no column the parent lacks (`exact_columns`).
fn validate_attached_columns(
    parent_columns: &[ColumnDef],
    child_columns: &[ColumnDef],
    parent: &str,
    child: &str,
    exact_columns: bool,
    reject_child_identity: bool,
) -> Result<(), SQLError> {
    for column in child_columns {
        if reject_child_identity
            && column
                .auto_increment
                .as_ref()
                .is_some_and(AutoIncrement::is_identity)
        {
            return Err(SQLError::Diagnostic {
                sqlstate: "55000".into(),
                message: format!(
                    "table \"{}\" being attached contains an identity column \"{}\"",
                    local_relation_name(child),
                    column.name
                ),
                detail: Some("The new partition may not contain an identity column.".into()),
                hint: None,
            });
        }
        if exact_columns
            && !parent_columns
                .iter()
                .any(|parent_column| parent_column.name == column.name)
        {
            return Err(SQLError::Diagnostic {
                sqlstate: "42804".into(),
                message: format!(
                    "table \"{}\" contains column \"{}\" not found in parent \"{}\"",
                    local_relation_name(child),
                    column.name,
                    local_relation_name(parent)
                ),
                detail: Some(
                    "The new partition may contain only the columns present in parent.".into(),
                ),
                hint: None,
            });
        }
    }
    Ok(())
}

/// Check that a child's column matches the parent column it inherits, as `MergeAttributesIntoExisting` does.
fn validate_inherited_column(
    parent_column: &ColumnDef,
    child_column: &ColumnDef,
    child: &str,
) -> Result<(), SQLError> {
    if parent_column.ty != child_column.ty {
        return Err(routine(
            "42804",
            format!(
                "child table \"{}\" has different type for column \"{}\"",
                local_relation_name(child),
                parent_column.name
            ),
        ));
    }
    if parent_column.not_null && !child_column.not_null {
        return Err(routine(
            "42804",
            format!(
                "column \"{}\" in child table \"{}\" must be marked NOT NULL",
                parent_column.name,
                local_relation_name(child)
            ),
        ));
    }
    match (&parent_column.generated, &child_column.generated) {
        (None, None) => Ok(()),
        (Some(_), None) => Err(routine(
            "42804",
            format!(
                "column \"{}\" in child table must be a generated column",
                parent_column.name
            ),
        )),
        (None, Some(_)) => Err(routine(
            "42804",
            format!(
                "column \"{}\" in child table must not be a generated column",
                parent_column.name
            ),
        )),
        (Some(parent_generated), Some(child_generated)) => {
            if parent_generated.kind == child_generated.kind {
                Ok(())
            } else {
                Err(routine(
                    "42804",
                    format!(
                        "column \"{}\" inherits from generated column of different kind",
                        parent_column.name
                    ),
                ))
            }
        }
    }
}

pub fn validate_inherited_checks(
    child: &str,
    child_columns: &[ColumnDef],
    parent_checks: &[TableCheck],
    child_checks: &[TableCheck],
) -> Result<(), SQLError> {
    for parent_check in parent_checks
        .iter()
        .filter(|constraint| !constraint.no_inherit)
    {
        let Some(name) = parent_check.name.as_deref() else {
            return Err(SQLError::Internal(
                "persisted parent CHECK constraint has no name".into(),
            ));
        };
        let Some(child_check) = child_checks
            .iter()
            .find(|constraint| constraint.name.as_deref() == Some(name))
        else {
            return Err(routine(
                "42804",
                format!("child table is missing constraint \"{name}\""),
            ));
        };
        if !crate::schema::check_inheritance::same_check_expression(
            &child_check.expr,
            &parent_check.expr,
            child_columns,
        )? {
            return Err(routine(
                "42804",
                format!(
                    "child table \"{}\" has different definition for check constraint \"{name}\"",
                    local_relation_name(child)
                ),
            ));
        }
        let conflict = if child_check.no_inherit {
            Some("non-inherited")
        } else if parent_check.validated && child_check.enforced && !child_check.validated {
            Some("NOT VALID")
        } else if parent_check.enforced && !child_check.enforced {
            Some("NOT ENFORCED")
        } else {
            None
        };
        if let Some(conflict) = conflict {
            return Err(routine("42P17", format!("constraint \"{name}\" conflicts with {conflict} constraint on child table \"{}\"", local_relation_name(child))));
        }
    }
    Ok(())
}

pub fn install_inherited_identity(
    columns: &mut [ColumnDef],
    inherited: &[(String, AutoIncrement)],
) -> Result<Vec<PartitionIdentityOverride>, SQLError> {
    let mut overrides = Vec::with_capacity(inherited.len());
    for (name, increment) in inherited {
        let column = columns
            .iter_mut()
            .find(|column| column.name == *name)
            .ok_or_else(|| SQLError::Internal(format!("partition lost column `{name}`")))?;
        overrides.push(PartitionIdentityOverride {
            column: name.clone(),
            original: column.auto_increment.clone(),
        });
        column.auto_increment = Some(increment.clone());
    }
    Ok(overrides)
}

pub fn restore_identity_overrides(
    columns: &mut [ColumnDef],
    inherited: &[(String, AutoIncrement)],
    overrides: &[PartitionIdentityOverride],
) {
    for (name, _) in inherited {
        let Some(column) = columns.iter_mut().find(|column| column.name == *name) else {
            continue;
        };
        column.auto_increment = overrides
            .iter()
            .find(|identity_override| identity_override.column == *name)
            .and_then(|identity_override| identity_override.original.clone());
    }
}

pub fn append_inherited_keys(
    target: &mut Vec<TableKeyConstraint>,
    inherited: &[TableKeyConstraint],
) -> Vec<TableKeyConstraint> {
    append_inherited_keys_matching(target, inherited, |_, _| true)
}

/// Each parent index requires a distinct child. The caller supplies attachment eligibility independently of SQL key equivalence.
pub fn append_inherited_keys_matching(
    target: &mut Vec<TableKeyConstraint>,
    inherited: &[TableKeyConstraint],
    can_attach: impl Fn(&TableKeyConstraint, &TableKeyConstraint) -> bool,
) -> Vec<TableKeyConstraint> {
    let mut appended = Vec::new();
    let mut used = std::collections::BTreeSet::new();
    for constraint in inherited {
        if let Some((position, _)) = target.iter().enumerate().find(|(position, candidate)| {
            !used.contains(position)
                && key_equivalent(candidate, constraint)
                && can_attach(candidate, constraint)
        }) {
            used.insert(position);
            continue;
        }
        let mut constraint = constraint.clone();
        constraint.name = None;
        constraint.catalog_identity = None;
        used.insert(target.len());
        target.push(constraint.clone());
        appended.push(constraint);
    }
    appended
}

pub fn key_equivalent(left: &TableKeyConstraint, right: &TableKeyConstraint) -> bool {
    left.kind == right.kind
        && left.columns == right.columns
        && left.included_columns == right.included_columns
        && left.nulls_not_distinct == right.nulls_not_distinct
        && left.without_overlaps == right.without_overlaps
}

pub fn clear_partition_constraint_provenance(constraints: &mut crate::ast::TableConstraintSet) {
    constraints
        .hierarchy
        .partition_inherited_key_constraints
        .clear();
    constraints
        .hierarchy
        .partition_inherited_foreign_keys
        .clear();
}

pub fn detached_bound_check(
    table: &str,
    spec: &PartitionSpec,
    bound: &PartitionBound,
    existing: &[TableCheck],
) -> TableCheck {
    let expr = renderable_bound_expression(spec, bound);
    let relation = local_relation_name(table);
    let key = spec.keys.first().and_then(|key| match key {
        Expr::Column(column) => Some(column.as_str()),
        _ => None,
    });
    let base = key.map_or_else(
        || format!("{relation}_check"),
        |column| format!("{relation}_{column}_check"),
    );
    let name = unique_constraint_name(&base, existing);
    TableCheck {
        catalog_oid: None,
        name: Some(name),
        expr,
        enforced: true,
        validated: true,
        no_inherit: false,
        object_id: None,
        is_local: true,
        partition_constraint: Some(DetachedPartitionConstraint {
            spec: spec.clone(),
            bound: bound.clone(),
        }),
    }
}

fn unique_constraint_name(base: &str, existing: &[TableCheck]) -> String {
    if !existing
        .iter()
        .any(|constraint| constraint.name.as_deref() == Some(base))
    {
        return base.to_string();
    }
    for suffix in 1_u64.. {
        let candidate = format!("{base}{suffix}");
        if !existing
            .iter()
            .any(|constraint| constraint.name.as_deref() == Some(candidate.as_str()))
        {
            return candidate;
        }
    }
    unreachable!("u64 constraint suffix space is exhaustive")
}

fn renderable_bound_expression(spec: &PartitionSpec, bound: &PartitionBound) -> Expr {
    let Some(key) = spec.keys.first().cloned().filter(|_| spec.keys.len() == 1) else {
        return Expr::Literal(Value::Bool(true));
    };
    match bound {
        PartitionBound::List(values) => {
            let mut terms = Vec::new();
            let mut non_null = Vec::new();
            for value in values {
                if matches!(value, Expr::Literal(Value::Null)) {
                    terms.push(Expr::IsNull {
                        expr: Box::new(key.clone()),
                        negated: false,
                    });
                } else {
                    non_null.push(value.clone());
                }
            }
            if !non_null.is_empty() {
                terms.push(Expr::InList {
                    expr: Box::new(key),
                    list: non_null,
                    negated: false,
                });
            }
            if terms.len() == 1 {
                terms.pop().unwrap_or(Expr::Literal(Value::Bool(true)))
            } else {
                Expr::Or(terms)
            }
        }
        PartitionBound::Range { lower, upper } if lower.len() == 1 && upper.len() == 1 => {
            let mut terms = vec![Expr::IsNull {
                expr: Box::new(key.clone()),
                negated: true,
            }];
            if let PartitionRangeDatum::Value(lower) = &lower[0] {
                terms.push(Expr::Binary {
                    op: BinaryOp::GreaterEqual,
                    lhs: Box::new(key.clone()),
                    rhs: Box::new(lower.clone()),
                });
            }
            if let PartitionRangeDatum::Value(upper) = &upper[0] {
                terms.push(Expr::Binary {
                    op: BinaryOp::Less,
                    lhs: Box::new(key),
                    rhs: Box::new(upper.clone()),
                });
            }
            Expr::And(terms)
        }
        PartitionBound::Hash { .. } | PartitionBound::Range { .. } | PartitionBound::Default => {
            Expr::Literal(Value::Bool(true))
        }
    }
}

pub fn validate_matching_persistence(
    child: &str,
    parent: &str,
    operation: &str,
    child_persistence: RelationPersistence,
    parent_persistence: RelationPersistence,
) -> Result<(), SQLError> {
    if (child_persistence == RelationPersistence::Temporary)
        != (parent_persistence == RelationPersistence::Temporary)
    {
        return Err(wrong_object(format!(
            "cannot {operation} {} relation \"{}\" from {} relation \"{}\"",
            persistence_label(child_persistence),
            local_relation_name(child),
            persistence_label(parent_persistence),
            local_relation_name(parent)
        )));
    }
    Ok(())
}

fn persistence_label(persistence: RelationPersistence) -> &'static str {
    match persistence {
        RelationPersistence::Temporary => "temporary",
        RelationPersistence::Unlogged => "unlogged",
        RelationPersistence::Permanent => "permanent",
    }
}

pub fn normalize_parent_sequence_numbers(hierarchy: &mut TableHierarchy) {
    if hierarchy.parent_sequence_numbers.len() == hierarchy.parents.len() {
        return;
    }
    hierarchy.parent_sequence_numbers = hierarchy
        .parents
        .iter()
        .enumerate()
        .map(|(index, _)| i32::try_from(index + 1).unwrap_or(i32::MAX))
        .collect();
}

fn local_relation_name(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}
fn wrong_object(message: impl Into<String>) -> SQLError {
    routine("42809", message)
}
fn routine(sqlstate: &str, message: impl Into<String>) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message: message.into(),
    }
}
