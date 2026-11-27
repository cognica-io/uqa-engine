//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Recognize executables whose mutable dependencies are covered by their retained analysis snapshot.

use super::PreparedStatementPlan;
use crate::prepared::{composites::CompositeInputs, domains::GenericDomainInputs};
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
        let inputs = TrackedInputs {
            composites: &self.composite_inputs,
            domains: &self.generic_domains,
        };
        let tracked_type = |ty: &ColumnType| inputs.ty(ty);
        if self.needs_analysis
            || !self.dependencies.routines.is_empty()
            || !self
                .parameter_types
                .iter()
                .all(|ty| ty.as_ref().is_some_and(tracked_type))
        {
            return false;
        }
        let Some(schema) = &self.result_schema else {
            return false;
        };
        if !schema
            .column_types()
            .iter()
            .all(|ty| ty.as_ref().is_some_and(tracked_type))
            || (0..schema.len()).any(|index| {
                schema.record_fields(index).is_some()
                    && schema.column_types()[index]
                        .as_ref()
                        .is_none_or(|ty| !inputs.composites.tracks_type(ty))
            })
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
            .is_some_and(|plan| independent_plan(plan, true, tracked_relations, &inputs))
            // Composite analysis has replaced written type names and ROW casts with retained identities; its logical tree carries every remaining executable input.
            && (inputs.composites.has_inputs()
                || independent_plan(&self.source_plan, false, tracked_relations, &inputs))
            && independent_plan(&self.logical_plan, false, tracked_relations, &inputs)
    }
}

struct TrackedInputs<'a> {
    composites: &'a CompositeInputs,
    domains: &'a GenericDomainInputs,
}

impl TrackedInputs<'_> {
    fn ty(&self, ty: &ColumnType) -> bool {
        if let ColumnType::Domain { oid, base, .. } = ty {
            let mut base = &**base;
            while let ColumnType::Domain { base: nested, .. } = base {
                base = nested;
            }
            return self.domains.tracks_oid(*oid) && independent_type(base);
        }
        independent_type(ty) || self.composites.tracks_type(ty)
    }

    fn name(&self, name: &str) -> bool {
        independent_type_name(name)
            || self.composites.tracks_name(name)
            || self.domains.tracks_name(name)
    }
}

fn independent_plan(
    plan: &UnifiedPlan,
    executable: bool,
    tracked_relations: bool,
    inputs: &TrackedInputs<'_>,
) -> bool {
    match plan {
        UnifiedPlan::Query(query) => {
            independent_query(query, executable, tracked_relations, inputs)
        }
        UnifiedPlan::Command(_) => false,
    }
}

fn independent_query(
    query: &QueryPlan,
    executable: bool,
    tracked_relations: bool,
    inputs: &TrackedInputs<'_>,
) -> bool {
    if !query.ctes.is_empty() {
        return false;
    }
    let admitted = |expression: &ScalarExpr| independent_expression(expression, executable, inputs);
    match &query.root {
        RelationalPlan::QueryBlock(block) => {
            block.from.as_ref().is_none_or(|source| match source {
                SourcePlan::Table { .. } => tracked_relations,
                SourcePlan::Function {
                    binding,
                    relations,
                    args,
                    column_types,
                    ..
                } => {
                    relations.is_none()
                        && column_types.is_empty()
                        && binding.as_ref().map_or(!executable, |binding| {
                            independent_binding(binding)
                                && crate::fixed_builtin_return_type(binding)
                                    .is_some_and(|ty| independent_type(&ty))
                        })
                        && args.iter().all(admitted)
                }
                _ => false,
            }) && block.subqueries.is_empty()
                && block.windows.is_empty()
                && block.locking.is_empty()
                && block.query_expressions().all(admitted)
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
                && independent_query(left, executable, tracked_relations, inputs)
                && independent_query(right, executable, tracked_relations, inputs)
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

fn independent_expression(
    expression: &ScalarExpr,
    executable: bool,
    inputs: &TrackedInputs<'_>,
) -> bool {
    let mut independent = true;
    expression.visit(&mut |part| {
        independent &= match part {
            ScalarExpr::Literal(value) => independent_value(value),
            ScalarExpr::RetainedLiteral(literal) => {
                // Retained values are already evaluated. Classify their selected identity without decoding resident or spilled payloads.
                literal.ty.as_ref().is_some_and(|ty| inputs.ty(ty))
                    && literal
                        .declaration
                        .as_deref()
                        .is_none_or(|name| inputs.name(name))
            }
            ScalarExpr::Collated { ty, .. } => inputs.ty(ty),
            ScalarExpr::Cast { ty, .. } => inputs.name(ty),
            ScalarExpr::TypedLiteral {
                value,
                ty,
                bound_type,
                parameter_index,
                ..
            } => {
                parameter_index.is_none()
                    && ((bound_type
                        .as_ref()
                        .is_some_and(|ty| inputs.composites.tracks_type(ty))
                        || inputs.composites.tracks_name(ty))
                        || (independent_value(value)
                            && bound_type
                                .as_ref()
                                .map_or_else(|| inputs.name(ty), |ty| inputs.ty(ty))))
            }
            ScalarExpr::CompositeRow {
                bound_type,
                binding,
                ..
            } => {
                bound_type
                    .as_ref()
                    .is_some_and(|ty| inputs.composites.tracks_type(ty))
                    || inputs.composites.tracks_name(&binding.ty)
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
                        tracked_field(binding, inputs.composites)
                            || tracked_binary_operator(binding, args.len(), executable)
                            || (independent_binding(binding)
                                && (!executable
                                    || (args.len() == binding.argument_types.len()
                                        && crate::fixed_builtin_return_type(binding)
                                            .is_some_and(|ty| independent_type(&ty)))))
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

fn tracked_field(binding: &FunctionBinding, composites: &CompositeInputs) -> bool {
    binding.builtin
        && binding.object_id.is_none()
        && binding.dispatch == Some(crate::ast::FunctionDispatch::FieldSelect)
        && binding.invocation.is_none()
        && binding.resolution_error.is_none()
        && binding.composite_field().is_some_and(|field| {
            composites.tracks_oid(field.type_oid)
                && (independent_type(&field.result_type)
                    || composites.tracks_type(&field.result_type))
        })
}

/// A selected builtin operator with primitive operands and result cannot depend on a mutable type or routine. Polymorphic output conversions stay on the conservative path.
fn tracked_binary_operator(binding: &FunctionBinding, arity: usize, executable: bool) -> bool {
    let Some(crate::ast::FunctionDispatch::Concat { oid }) = &binding.dispatch else {
        return false;
    };
    binding.builtin
        && binding.object_id.is_none()
        && binding.invocation.is_none()
        && binding.resolution_error.is_none()
        && arity == 2
        // The parser records the operator before analysis selects its overload. Every surviving executable call must carry its catalog identity.
        && oid.map_or(!executable, |oid| {
            crate::type_resolution::binary_operator_by_oid(i64::from(oid)).is_some_and(|operator| {
                operator.operand_types.iter().all(|name| independent_type_name(name))
                    && independent_type_name(operator.result_type)
            })
        })
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
        | Value::BitString(_)
        | Value::Decimal(_)
        | Value::Json(_)
        | Value::JsonB(_) => true,
        Value::Datum(datum) => crate::catalog::type_metadata::builtin_scalar_type(datum.type_oid())
            .is_some_and(independent_type),
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
