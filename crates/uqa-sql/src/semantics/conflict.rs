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
use uqa_core::{memory::ProductionControl, Value};

mod production;
pub use production::prepare_inference_predicate_with_control;

pub trait ConflictCatalog {
    /// Namespace and routines used to recover analysis metadata from an older index expression.
    fn inference_context(&self) -> InferenceContext<'_>;
    fn try_describe_table(&self, table: &str) -> Result<Option<Vec<ColumnDef>>, String>;
    /// The selected relation's named row type; catalogs without a type identity retain an anonymous row.
    fn relation_row_type(&self, _table: &str) -> Result<Option<crate::ColumnType>, SQLError> {
        Ok(None)
    }
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
    if let crate::ast::ConflictTargetConstraint::Named(name) = &conflict.constraint {
        return constraint_target_index(catalog, table, keys, name);
    }
    if conflict.conflict_columns.is_empty() && conflict.expressions.is_empty() {
        return Ok((0..keys.len()).collect());
    }
    let columns = validate_conflict_columns(catalog, table, &conflict.conflict_columns)?;
    #[derive(Clone, Copy, PartialEq)]
    enum Operand<'a> {
        Column(&'a str),
        Expression(&'a Expr),
    }
    fn operand(expr: &Expr) -> Operand<'_> {
        match expr {
            Expr::Column(name) => Operand::Column(name.as_str()),
            _ => Operand::Expression(expr),
        }
    }
    let mut target = conflict
        .conflict_columns
        .iter()
        .map(|column| (Operand::Column(column), None))
        .collect::<Vec<_>>();
    for (position, expr) in conflict.expressions.iter().enumerate() {
        let collation = conflict
            .constraint
            .collation(position)
            .map(|name| {
                crate::catalog::collations::expression::select(
                    name,
                    None,
                    None,
                    Some(catalog.inference_context().routines),
                )
            })
            .transpose()?;
        target.push((operand(expr), collation));
    }
    let needs_key_collation = target.iter().any(|(_, oid)| oid.is_some());
    let mut legacy_scope = None;
    let mut stored_identity = |syntax: &crate::ast::Expr, read_collation: bool| {
        let mut plan = ExpressionPlan::lower(syntax.clone());
        let mut collation = None;
        if read_collation || needs_index_collations(syntax, &columns) {
            let context = catalog.inference_context();
            let (snapshot, schema) = match &legacy_scope {
                Some(scope) => scope,
                None => legacy_scope.insert((
                    context.binding.binding_scope()?,
                    crate::RowSchema::with_column_definitions(Some(table), &columns),
                )),
            };
            collation = Some(crate::binding::assign_stored_input_collations(
                context.routines,
                &mut plan,
                schema,
                &snapshot.context(),
            )?);
        }
        Ok::<_, SQLError>((inference_identity(plan.scalar), collation))
    };
    let mut indexes = Vec::new();
    for (index, key) in keys.iter().enumerate() {
        let mut satisfied = vec![false; target.len()];
        let mut all_attributes_named = true;
        for (position, attribute) in key.keys.iter().enumerate() {
            let recorded = key
                .key_order
                .get(position)
                .and_then(|order| order.collation_oid);
            let expression;
            let (value, collation) = match attribute {
                crate::ast::IndexKey::Column(name) => {
                    let oid = recorded.or_else(|| {
                        if !needs_key_collation {
                            return None;
                        }
                        let column = columns.iter().find(|column| column.name == *name)?;
                        u32::try_from(crate::catalog::collations::declarations::column_oid(column))
                            .ok()
                    });
                    (Operand::Column(name), oid)
                }
                crate::ast::IndexKey::Expression(syntax) => {
                    let (identity, natural) =
                        stored_identity(syntax, needs_key_collation && recorded.is_none())?;
                    expression = identity;
                    (operand(&expression), recorded.or(natural))
                }
            };
            // A duplicated expression can occur at several positions with different collations. Each inference element must match one attribute of this index.
            let mut named = false;
            for ((wanted, qualifier), matched) in target.iter().zip(&mut satisfied) {
                if *wanted == value {
                    named = true;
                    *matched |= qualifier.is_none() || *qualifier == collation;
                }
            }
            all_attributes_named &= named;
        }
        if !all_attributes_named || satisfied.contains(&false) {
            continue;
        }
        if let Some(required) = key.predicate.as_deref() {
            let Some(given) = conflict.predicate.as_deref() else {
                continue;
            };
            if !implies(given, &stored_identity(required, false)?.0) {
                continue;
            }
        }
        indexes.push(index);
    }
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

/// Older catalogs stored bare comparisons and omitted some calls' selected input collations. The SQL binder reconstructs that metadata against the indexed relation before comparing identities.
fn needs_index_collations(syntax: &crate::ast::Expr, columns: &[ColumnDef]) -> bool {
    use crate::ast::{Expr, FunctionDispatch};
    use crate::catalog::collations::DEFAULT_COLLATION_OID;
    if crate::binding::legacy_collations::expression_lacks_input_collation(syntax) {
        return true;
    }
    let bare_operation = syntax.any_node(&|node| match node {
        Expr::Binary { op, .. } => op.comparison().is_some() && op.input_collation().is_none(),
        Expr::Func {
            binding: Some(binding),
            ..
        } => {
            binding.input_collation.is_none()
                && matches!(binding.dispatch, Some(FunctionDispatch::Pattern { .. }))
        }
        _ => false,
    });
    bare_operation
        && syntax.any_node(&|node| match node {
            Expr::Column(name) | Expr::QualifiedColumn { column: name, .. } => {
                columns.iter().any(|column| {
                    column.name.as_str() == name.as_str()
                        && column
                            .collation_oid
                            .is_some_and(|oid| i64::from(oid) != DEFAULT_COLLATION_OID)
                })
            }
            Expr::Func {
                binding: Some(binding),
                ..
            } => matches!(binding.dispatch, Some(FunctionDispatch::Collate { .. })),
            _ => false,
        })
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
    let operator = op.comparison()?;
    if let Expr::Literal(value) | Expr::TypedLiteral { value, .. } = rhs.as_ref() {
        return Some((lhs, *op, value));
    }
    if let Expr::Literal(value) | Expr::TypedLiteral { value, .. } = lhs.as_ref() {
        let reversed = match operator.binary() {
            BinaryOp::Less => BinaryOp::Greater,
            BinaryOp::LessEqual => BinaryOp::GreaterEqual,
            BinaryOp::Greater => BinaryOp::Less,
            BinaryOp::GreaterEqual => BinaryOp::LessEqual,
            other => other,
        };
        return Some((
            rhs,
            reversed.with_input_collation(op.input_collation()),
            value,
        ));
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
    if given.input_collation() != required.input_collation() {
        return false;
    }
    let collation = given.input_collation().map_or(
        crate::catalog::collations::DEFAULT_COLLATION_OID as u32,
        crate::catalog::collations::CollationSelection::oid,
    );
    let Ok(order) = crate::expr::compare_typed_values_with_catalog_and_collation(
        left,
        right,
        collation,
        &ProductionControl::uncontrolled(),
        None,
    ) else {
        return false;
    };
    match (given.uncollated(), required.uncollated()) {
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
) -> Result<Vec<ColumnDef>, SQLError> {
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
    Ok(columns)
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
