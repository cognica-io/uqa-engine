//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Recognize executables whose mutable dependencies are covered by their retained analysis snapshot.

use super::PreparedStatementPlan;
use crate::{
    ast::FunctionBinding,
    plan::{QueryPlan, RelationalPlan, SourcePlan, UnifiedPlan},
    ColumnType, ScalarExpr,
};
use uqa_core::Value;

impl PreparedStatementPlan {
    /// Primitive expressions and selected fixed builtins can retain their executable across an unrelated registry refresh. Relations require exact analysis revisions, checked before every plan selection; surviving calls still check current execution privileges.
    #[must_use]
    pub fn has_tracked_executable_dependencies(&self) -> bool {
        if self.needs_analysis
            || !self.dependencies.routines.is_empty()
            || !self
                .parameter_types
                .iter()
                .all(|ty| ty.as_ref().is_some_and(independent_type))
        {
            return false;
        }
        let Some(schema) = &self.result_schema else {
            return false;
        };
        if !schema
            .column_types()
            .iter()
            .all(|ty| ty.as_ref().is_some_and(independent_type))
            || (0..schema.len()).any(|index| schema.record_fields(index).is_some())
        {
            return false;
        }
        let tracked_relations = !self.dependencies.relations.is_empty()
            && self.dependency_snapshot.as_ref().is_some_and(|snapshot| {
                snapshot.global_catalog.is_some()
                    && self
                        .dependencies
                        .relations
                        .iter()
                        .all(|oid| snapshot.relations.get(oid).is_some_and(Option::is_some))
            });
        if !self.dependencies.relations.is_empty() && !tracked_relations {
            return false;
        }
        self.plan
            .as_ref()
            .is_some_and(|plan| independent_plan(plan, true, tracked_relations))
            && independent_plan(&self.source_plan, false, tracked_relations)
            && independent_plan(&self.logical_plan, false, tracked_relations)
    }
}

fn independent_plan(plan: &UnifiedPlan, executable: bool, tracked_relations: bool) -> bool {
    match plan {
        UnifiedPlan::Query(query) => independent_query(query, executable, tracked_relations),
        UnifiedPlan::Command(_) => false,
    }
}

fn independent_query(query: &QueryPlan, executable: bool, tracked_relations: bool) -> bool {
    if !query.ctes.is_empty() {
        return false;
    }
    let admitted = |expression: &ScalarExpr| independent_expression(expression, executable);
    match &query.root {
        RelationalPlan::QueryBlock(block) => {
            block.from.as_ref().is_none_or(|source| {
                tracked_relations && matches!(source, SourcePlan::Table { .. })
            }) && block.subqueries.is_empty()
                && block.windows.is_empty()
                && block.locking.is_empty()
                && block
                    .expressions()
                    .iter()
                    .all(|expression| admitted(expression))
        }
        RelationalPlan::SetOp {
            left,
            right,
            order_by,
            limit,
            offset,
            subqueries,
            ..
        } => {
            subqueries.is_empty()
                && independent_query(left, executable, tracked_relations)
                && independent_query(right, executable, tracked_relations)
                && order_by
                    .iter()
                    .map(|order| &order.expr)
                    .chain(limit.as_deref())
                    .chain(offset.as_deref())
                    .all(admitted)
        }
        RelationalPlan::Values { rows, subqueries } => {
            subqueries.is_empty() && rows.iter().flatten().all(admitted)
        }
    }
}

fn independent_expression(expression: &ScalarExpr, executable: bool) -> bool {
    let mut independent = true;
    expression.visit(&mut |part| {
        independent &= match part {
            ScalarExpr::Literal(value) => independent_value(value),
            ScalarExpr::Cast { ty, .. } => independent_type_name(ty),
            ScalarExpr::TypedLiteral {
                value,
                ty,
                bound_type,
                parameter_index,
            } => {
                parameter_index.is_none()
                    && independent_value(value)
                    && bound_type
                        .as_ref()
                        .map_or_else(|| independent_type_name(ty), independent_type)
            }
            ScalarExpr::Func {
                binding,
                args,
                distinct,
                order_by,
                filter,
                ..
            } => {
                !*distinct
                    && order_by.is_empty()
                    && filter.is_none()
                    && binding.as_ref().map_or(!executable, |binding| {
                        independent_binding(binding)
                            && (!executable
                                || (args.len() == binding.argument_types.len()
                                    && crate::fixed_builtin_return_type(binding)
                                        .is_some_and(|ty| independent_type(&ty))))
                    })
            }
            ScalarExpr::Column(_)
            | ScalarExpr::QualifiedColumn { .. }
            | ScalarExpr::Param(_)
            | ScalarExpr::Array(_)
            | ScalarExpr::Binary { .. }
            | ScalarExpr::UnaryMinus(_)
            | ScalarExpr::Not(_)
            | ScalarExpr::And(_)
            | ScalarExpr::Or(_)
            | ScalarExpr::IsNull { .. }
            | ScalarExpr::Between { .. }
            | ScalarExpr::InList { .. }
            | ScalarExpr::Case { .. } => true,
            _ => false,
        };
    });
    independent
}

fn independent_binding(binding: &FunctionBinding) -> bool {
    binding.builtin
        && binding.object_id.is_none()
        && binding.dispatch.is_none()
        && binding.invocation.is_none()
        && binding.resolution_error.is_none()
        && binding
            .argument_types
            .iter()
            .all(|ty| independent_type_name(ty))
}

fn independent_value(value: &Value) -> bool {
    match value {
        Value::Null
        | Value::Bool(_)
        | Value::Int(_)
        | Value::Float(_)
        | Value::Str(_)
        | Value::FixedChar(_)
        | Value::Bytes(_)
        | Value::Decimal(_)
        | Value::Json(_)
        | Value::JsonB(_) => true,
        Value::Array(array) => array.elements().iter().all(independent_value),
        _ => false,
    }
}

fn independent_type_name(name: &str) -> bool {
    ColumnType::from_sql_name(name).is_ok_and(|ty| independent_type(&ty))
}

// The admitted set has catalog-free input and result identities. Keep pseudo, reg*, temporal, domain, enum and composite types on the existing conservative invalidation path.
fn independent_type(ty: &ColumnType) -> bool {
    match ty {
        ColumnType::Array(element) => independent_type(element),
        ColumnType::SmallInteger
        | ColumnType::Integer
        | ColumnType::BigInteger
        | ColumnType::Boolean
        | ColumnType::Text
        | ColumnType::Name
        | ColumnType::Uuid
        | ColumnType::Varchar(_)
        | ColumnType::Bpchar
        | ColumnType::Character(_)
        | ColumnType::Real
        | ColumnType::DoublePrecision
        | ColumnType::Numeric { .. }
        | ColumnType::Json
        | ColumnType::JsonB
        | ColumnType::Bytea
        | ColumnType::InternalChar => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests;
