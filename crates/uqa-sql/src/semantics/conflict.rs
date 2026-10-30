//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Unique-index inference, target validation, and SQL predicate implication.
use crate::{
    ast::{BinaryOp, ColumnDef, TableConstraintSet},
    binding::snapshot::BindingSnapshot,
    catalog::index::EnforcedKey,
    plan::AggregateClassifier,
    plan::{ConflictPlan, ExpressionPlan, InsertPlan},
    routines::RoutineResolution,
    SQLError, SQLParam, ScalarExpr as Expr,
};
use std::collections::BTreeSet;
use uqa_core::{memory::ProductionControl, Value};

mod production;
pub use production::prepare_inference_predicate_with_control;

pub trait ConflictCatalog {
    fn try_describe_table(&self, table: &str) -> Result<Option<Vec<ColumnDef>>, String>;
    /// Lend declaration metadata after observing the same stored sequence dependencies as a description. Consumers inspect names, types and constraints, never rewritten default expressions.
    fn with_column_metadata_with_control(
        &self,
        table: &str,
        control: &ProductionControl<'_>,
        read: &mut dyn FnMut(&[ColumnDef]) -> Result<(), SQLError>,
    ) -> Result<bool, SQLError> {
        control.check()?;
        let Some(columns) = self.try_describe_table(table).map_err(SQLError::Internal)? else {
            return Ok(false);
        };
        read(&columns)?;
        Ok(true)
    }
    fn enforced_keys(&self, table: &str) -> Result<Vec<EnforcedKey>, String>;
    fn try_declared_table_constraints(&self, table: &str) -> Result<TableConstraintSet, String>;
}
/// Capture the stored-expression catalog scope only when an inference expression requires binding.
pub trait InferenceBindingScope {
    fn binding_scope(&self) -> Result<BindingSnapshot, SQLError>;
    fn binding_scope_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<crate::binding::production::ControlledBindingSnapshot, SQLError> {
        control.check()?;
        self.binding_scope()?.copy_with_control(control)
    }
}
#[derive(Clone, Copy)]
pub struct InferenceContext<'a> {
    pub catalog: &'a dyn ConflictCatalog,
    pub aggregates: &'a dyn AggregateClassifier,
    pub routines: &'a dyn RoutineResolution,
    pub binding: &'a dyn InferenceBindingScope,
}

/// Analyze the inference clause in the INSERT target's scope before uniqueness arbitration, including commands that produce no input rows.
pub fn prepare_inference_predicate<'a>(
    context: InferenceContext<'_>,
    statement: &'a InsertPlan,
    params: &[SQLParam],
) -> Result<std::borrow::Cow<'a, InsertPlan>, SQLError> {
    let control = ProductionControl::uncontrolled();
    let Some(command) =
        prepare_inference_predicate_with_control(context, statement, params, &control)?
    else {
        return Ok(std::borrow::Cow::Borrowed(statement));
    };
    let crate::plan::CommandPlan::Insert(insert) = command
        .into_uncontrolled()
        .expect("ordinary inference command")
    else {
        unreachable!("INSERT inference")
    };
    Ok(std::borrow::Cow::Owned(*insert))
}

fn inference_identity(expression: Expr) -> Expr {
    let control = ProductionControl::uncontrolled();
    production::inference_identity_owned(
        crate::plan::OwnedScalarExpr::uncontrolled(expression),
        &control,
    )
    .expect("ordinary inference identity")
    .finish(&control)
    .expect("ordinary inference scalar")
    .into_uncontrolled()
    .expect("ordinary inference ownership")
}

/// Match the inference expressions analyzed by [`prepare_inference_predicate`] against the stored unique keys.
pub fn conflict_key_indices(
    catalog: &dyn ConflictCatalog,
    table: &str,
    keys: &[EnforcedKey],
    conflict: &ConflictPlan,
) -> Result<Vec<usize>, SQLError> {
    if let Some(name) = &conflict.constraint {
        return constraint_target_index(catalog, table, keys, name);
    }
    if conflict.conflict_columns.is_empty() && conflict.expressions.is_empty() {
        return Ok((0..keys.len()).collect());
    }
    validate_conflict_columns(catalog, table, &conflict.conflict_columns)?;
    let target = conflict
        .conflict_columns
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let indexes = keys
        .iter()
        .enumerate()
        .filter_map(|(index, key)| {
            (key.columns
                .iter()
                .map(String::as_str)
                .collect::<BTreeSet<_>>()
                == target
                && {
                    let expressions = key
                        .keys
                        .iter()
                        .filter_map(|key| match key {
                            crate::ast::IndexKey::Expression(expr) => Some(inference_identity(
                                ExpressionPlan::lower((**expr).clone()).scalar,
                            )),
                            crate::ast::IndexKey::Column(_) => None,
                        })
                        .collect::<Vec<_>>();
                    expressions
                        .iter()
                        .all(|expr| conflict.expressions.contains(expr))
                        && conflict
                            .expressions
                            .iter()
                            .all(|expr| expressions.contains(expr))
                }
                && key.predicate.as_deref().is_none_or(|required| {
                    conflict.predicate.as_deref().is_some_and(|given| {
                        implies(
                            given,
                            &inference_identity(ExpressionPlan::lower(required.clone()).scalar),
                        )
                    })
                }))
            .then_some(index)
        })
        .collect::<Vec<_>>();
    if indexes.is_empty() {
        return Err(SQLError::Routine {
            sqlstate: "42P10".into(),
            message:
                "there is no unique or exclusion constraint matching the ON CONFLICT specification"
                    .into(),
        });
    }
    Ok(indexes)
}

/// Prove that every row for which `given` is true also makes `required` true. SQL NULL is never treated as false in a proof that would admit an invalid arbiter.
fn implies(given: &Expr, required: &Expr) -> bool {
    if given == required || matches!(required, Expr::Literal(Value::Bool(true))) {
        return true;
    }
    if matches!(given, Expr::Literal(Value::Bool(false) | Value::Null)) {
        return true;
    }
    if let Expr::And(parts) = required {
        return parts.iter().all(|part| implies(given, part));
    }
    if let Expr::Or(parts) = given {
        return parts.iter().all(|part| implies(part, required));
    }
    if let Expr::And(parts) = given {
        if parts.iter().any(|part| implies(part, required)) {
            return true;
        }
    }
    if let Expr::Or(parts) = required {
        return parts.iter().any(|part| implies(given, part));
    }
    if let Some((left, given_op, given_value)) = comparison(given) {
        if let Some((right, required_op, required_value)) = comparison(required) {
            return left == right
                && comparison_implies(given_op, given_value, required_op, required_value);
        }
        if let Expr::IsNull {
            expr,
            negated: true,
        } = required
        {
            return left == expr.as_ref() && !matches!(given_value, Value::Null);
        }
    }
    false
}

fn comparison(expr: &Expr) -> Option<(&Expr, BinaryOp, &Value)> {
    let Expr::Binary { op, lhs, rhs } = expr else {
        return None;
    };
    if !matches!(
        op,
        BinaryOp::Equal
            | BinaryOp::NotEqual
            | BinaryOp::Less
            | BinaryOp::LessEqual
            | BinaryOp::Greater
            | BinaryOp::GreaterEqual
    ) {
        return None;
    }
    if let Expr::Literal(value) = rhs.as_ref() {
        return Some((lhs, *op, value));
    }
    if let Expr::Literal(value) = lhs.as_ref() {
        let reversed = match op {
            BinaryOp::Less => BinaryOp::Greater,
            BinaryOp::LessEqual => BinaryOp::GreaterEqual,
            BinaryOp::Greater => BinaryOp::Less,
            BinaryOp::GreaterEqual => BinaryOp::LessEqual,
            other => *other,
        };
        return Some((rhs, reversed, value));
    }
    None
}

fn comparison_implies(given: BinaryOp, left: &Value, required: BinaryOp, right: &Value) -> bool {
    use BinaryOp::{Equal, Greater, GreaterEqual, Less, LessEqual, NotEqual};
    if matches!(left, Value::Null) || matches!(right, Value::Null) {
        return false;
    }
    if std::mem::discriminant(left) != std::mem::discriminant(right) {
        return false;
    }
    let order = left.cmp(right);
    match (given, required) {
        (Equal, Equal) | (NotEqual, NotEqual) => order.is_eq(),
        (Equal, NotEqual) => !order.is_eq(),
        (Equal | GreaterEqual, Greater) | (GreaterEqual, NotEqual) => order.is_gt(),
        (Equal | LessEqual, Less) | (LessEqual, NotEqual) => order.is_lt(),
        (Equal | Greater | GreaterEqual, GreaterEqual) | (Greater, Greater | NotEqual) => {
            order.is_ge()
        }
        (Equal | Less | LessEqual, LessEqual) | (Less, Less | NotEqual) => order.is_le(),
        _ => false,
    }
}

pub fn validate_conflict_target(
    catalog: &dyn ConflictCatalog,
    table: &str,
    conflict: &ConflictPlan,
) -> Result<(), SQLError> {
    let keys = catalog
        .enforced_keys(table)
        .map_err(|error| SQLError::Internal(format!("conflict target keys: {error}")))?;
    conflict_key_indices(catalog, table, &keys, conflict).map(|_| ())
}

fn constraint_target_index(
    catalog: &dyn ConflictCatalog,
    table: &str,
    keys: &[EnforcedKey],
    name: &str,
) -> Result<Vec<usize>, SQLError> {
    if let Some(index) = keys
        .iter()
        .position(|key| key.constraint_owned && key.name.as_deref() == Some(name))
    {
        return Ok(vec![index]);
    }
    let snapshot = catalog
        .try_declared_table_constraints(table)
        .map_err(SQLError::Internal)?;
    let columns = catalog
        .try_describe_table(table)
        .map_err(SQLError::Internal)?
        .ok_or_else(|| SQLError::UnknownTable(table.into()))?;
    let exists = snapshot
        .checks
        .iter()
        .any(|check| check.name.as_deref() == Some(name))
        || snapshot
            .foreign_keys
            .iter()
            .any(|key| key.name.as_deref() == Some(name))
        || columns.iter().any(|column| {
            column.not_null_name.as_deref() == Some(name)
                || column.check_name.as_deref() == Some(name)
                || column
                    .references
                    .as_ref()
                    .is_some_and(|reference| reference.name.as_deref() == Some(name))
        });
    if exists {
        return Err(SQLError::Routine {
            sqlstate: "42809".into(),
            message: "constraint in ON CONFLICT clause has no associated index".into(),
        });
    }
    Err(SQLError::Routine {
        sqlstate: "42704".into(),
        message: format!("constraint \"{name}\" for table \"{table}\" does not exist"),
    })
}

fn validate_conflict_columns(
    catalog: &dyn ConflictCatalog,
    table: &str,
    names: &[String],
) -> Result<(), SQLError> {
    let columns = catalog
        .try_describe_table(table)
        .map_err(SQLError::Internal)?
        .ok_or_else(|| SQLError::UnknownTable(table.into()))?;
    for name in names {
        if !columns.iter().any(|column| column.name == *name)
            && !matches!(
                name.as_str(),
                "ctid" | "tableoid" | "xmin" | "xmax" | "cmin" | "cmax"
            )
        {
            return Err(SQLError::UnknownColumn(name.clone()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod inference;

#[cfg(test)]
mod production_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inference_implication_ignores_cast_origin_but_preserves_cast_types() {
        let predicate = |implicit, ty: &str, bound| {
            inference_identity(Expr::Binary {
                op: BinaryOp::Greater,
                lhs: Box::new(Expr::Cast {
                    implicit,
                    expr: Box::new(Expr::Column("value".into())),
                    ty: ty.into(),
                }),
                rhs: Box::new(Expr::Literal(Value::Int(bound))),
            })
        };
        for implicit in [false, true] {
            let required = predicate(implicit, "bigint", 0);
            let equivalent = predicate(!implicit, "bigint", 0);
            assert_eq!(required, equivalent);
            assert!(implies(&equivalent, &required));
            assert!(implies(&predicate(!implicit, "bigint", 1), &required));
            assert!(!implies(&predicate(!implicit, "bigint", -1), &required));
            assert!(!implies(&predicate(!implicit, "integer", 0), &required));
        }
    }
}
