//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{call::BindingCall, FunctionTypeResolver};
use crate::{
    ast::{ColumnType, FunctionBinding},
    schema::ScalarTypeSchema,
    SQLError, SQLParam, ScalarExpr,
};
use uqa_core::{
    memory::{MemoryReservation, Produced, ProductionControl},
    Value,
};
mod arrays;
mod comparison;
mod concat;
mod enums;
mod helpers;
mod membership;

/// Bind polymorphic type-introspection calls and common-type coercions while the input schema still carries declared SQL types.
pub fn bind_type_introspection(
    expression: ScalarExpr,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
) -> ScalarExpr {
    bind_ordinary(expression, schema, params, None)
}

/// Bind type-introspection calls with access to catalog-backed function and aggregate overloads.
pub fn bind_type_introspection_with_resolver(
    expression: ScalarExpr,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
) -> ScalarExpr {
    bind_ordinary(expression, schema, params, Some(resolver))
}

/// Bind an admitted scalar tree while every new type name, cast and selected-call allocation remains under the original tree's allowance and both cancellation scopes.
pub fn bind_type_introspection_with_control(
    expression: Produced<ScalarExpr>,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    control: &ProductionControl<'_>,
) -> Result<Produced<ScalarExpr>, SQLError> {
    bind_owned(
        expression,
        schema,
        params,
        None,
        control,
        LiteralBinding::Ordinary,
    )
}

/// Convert the `unknown` literals that binding coerces to catalog enum types, reporting the first invalid input as `PostgreSQL` parse analysis does. Binding at execution then folds the same literals without failing.
pub fn validate_catalog_literals(
    expression: &ScalarExpr,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
) -> Result<(), SQLError> {
    if !resolver
        .enum_labels()
        .is_some_and(crate::expr::enums::EnumLabelCatalog::has_enum_types)
    {
        return Ok(());
    }
    let control = ProductionControl::uncontrolled();
    let expression = control.finish(expression.clone(), None)?;
    bind_owned(
        expression,
        schema,
        params,
        Some(resolver),
        &control,
        LiteralBinding::Validate,
    )
    .map(drop)
}

/// Bind a copy of a stored expression as parse analysis does, so the caller can keep the enum constants that binding coerces from `unknown` literals. Returns `None` when the statement catalog defines no enum type. Input that no label matches stays unconverted for the literal's own validation to report.
pub fn bind_catalog_constants(
    expression: &ScalarExpr,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
) -> Result<Option<ScalarExpr>, SQLError> {
    if !resolver
        .enum_labels()
        .is_some_and(crate::expr::enums::EnumLabelCatalog::has_enum_types)
    {
        return Ok(None);
    }
    let control = ProductionControl::uncontrolled();
    let expression = control.finish(expression.clone(), None)?;
    bind_owned(
        expression,
        schema,
        params,
        Some(resolver),
        &control,
        LiteralBinding::Ordinary,
    )?
    .into_uncontrolled()
    .map(Some)
    .map_err(|_| SQLError::Internal("uncontrolled binding retained a reservation".into()))
}

/// Preserve input constants only at the stored-expression binding boundary, where catalog input functions may run once and their result survives reopening.
pub(super) fn bind_stored_inputs(
    expression: ScalarExpr,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
) -> Result<ScalarExpr, SQLError> {
    let control = ProductionControl::uncontrolled();
    let expression = control.finish(expression, None)?;
    bind_owned(
        expression,
        schema,
        params,
        Some(resolver),
        &control,
        LiteralBinding::Stored,
    )?
    .into_uncontrolled()
    .map_err(|_| SQLError::Internal("stored binding retained a reservation".into()))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LiteralBinding {
    Ordinary,
    Validate,
    Stored,
}

fn bind_ordinary(
    expression: ScalarExpr,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> ScalarExpr {
    let control = ProductionControl::uncontrolled();
    let expression = control
        .finish(expression, None)
        .expect("ordinary scalar owner");
    bind_owned(
        expression,
        schema,
        params,
        resolver,
        &control,
        LiteralBinding::Ordinary,
    )
    .expect("ordinary binding cannot be cancelled or limited")
    .into_uncontrolled()
    .expect("ordinary binding has no reservation")
}

struct RootOwner {
    expression: ScalarExpr,
    memory: Option<MemoryReservation>,
}

fn bind_owned(
    expression: Produced<ScalarExpr>,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
    control: &ProductionControl<'_>,
    literal_binding: LiteralBinding,
) -> Result<Produced<ScalarExpr>, SQLError> {
    assert!(
        control.budget().is_none() || resolver.is_none(),
        "controlled binding cannot invoke an unowned catalog resolver"
    );
    let expression = super::call::check_owner(expression, control)?;
    let (expression, memory) = expression.into_parts();
    let mut root = RootOwner { expression, memory };
    let expression = std::mem::replace(&mut root.expression, ScalarExpr::Literal(Value::Null));
    root.expression = Binder {
        schema,
        params,
        resolver,
        control: *control,
        memory: &mut root.memory,
        literal_binding,
    }
    .bind(expression)?;
    Ok(control.finish(root.expression, root.memory)?)
}

struct Binder<'a, 'b> {
    schema: &'a dyn ScalarTypeSchema,
    params: &'a [SQLParam],
    resolver: Option<&'a dyn FunctionTypeResolver>,
    control: ProductionControl<'a>,
    memory: &'b mut Option<MemoryReservation>,
    /// Report an `unknown` literal that its coerced type rejects instead of leaving the conversion to evaluation.
    literal_binding: LiteralBinding,
}

impl Binder<'_, '_> {
    #[expect(
        clippy::too_many_lines,
        reason = "one expression transform preserves traversal and coercion order"
    )]
    fn bind(&mut self, expression: ScalarExpr) -> Result<ScalarExpr, SQLError> {
        self.control.check()?;
        Ok(match expression {
            ScalarExpr::Func {
                name,
                binding,
                args,
                distinct,
                order_syntax,
                order_by,
                filter,
            } => self.bind_call(BindingCall {
                name,
                binding,
                arguments: args,
                distinct,
                order_syntax,
                order_by,
                filter,
            })?,
            ScalarExpr::Array(mut items) => {
                self.items(&mut items)?;
                self.common_expressions(&mut items)?;
                ScalarExpr::Array(items)
            }
            ScalarExpr::CompositeRow {
                mut items,
                binding,
                mut bound_type,
            } => {
                self.items(&mut items)?;
                if let Some(resolver) = self.resolver {
                    if let Some(ty) = self
                        .semantic(resolver.resolve_type_name(&binding.ty))?
                        .flatten()
                    {
                        bound_type = Some(ty);
                    }
                }
                ScalarExpr::CompositeRow {
                    items,
                    binding,
                    bound_type,
                }
            }
            ScalarExpr::Row(mut items) => {
                self.items(&mut items)?;
                ScalarExpr::Row(items)
            }
            ScalarExpr::Binary {
                op,
                mut lhs,
                mut rhs,
            } => {
                let operands = self
                    .semantic(self.binary_operand_types(op, &lhs, &rhs))?
                    .flatten();
                let real_inputs = operands.as_deref().map_or([false; 2], |types| {
                    [
                        matches!(types[0], ColumnType::Real),
                        matches!(types[1], ColumnType::Real),
                    ]
                });
                self.in_place(&mut lhs)?;
                self.in_place(&mut rhs)?;
                self.coerce_comparison(operands, &mut lhs, &mut rhs)?;
                if matches!(
                    op,
                    crate::ast::BinaryOp::Add
                        | crate::ast::BinaryOp::Subtract
                        | crate::ast::BinaryOp::Multiply
                        | crate::ast::BinaryOp::Divide
                ) {
                    // Value erases float width. Runtime evaluation retains each selected real input even when SQL analysis needs no conversion.
                    if real_inputs[0] {
                        self.wrap_declared(&mut lhs, &ColumnType::Real)?;
                    }
                    if real_inputs[1] {
                        self.wrap_declared(&mut rhs, &ColumnType::Real)?;
                    }
                }
                ScalarExpr::Binary { op, lhs, rhs }
            }
            ScalarExpr::UnaryMinus(mut inner) => {
                let source = self.semantic(self.infer(&inner))?.flatten();
                let source = source
                    .map(|ty| {
                        self.semantic(super::operators::unary_minus_result_type_with_control(
                            &ty,
                            &self.control,
                        ))
                    })
                    .transpose()?
                    .flatten();
                self.in_place(&mut inner)?;
                if let Some(source) = source {
                    self.wrap_declared(&mut inner, &source)?;
                }
                ScalarExpr::UnaryMinus(inner)
            }
            ScalarExpr::Not(mut inner) => {
                self.in_place(&mut inner)?;
                ScalarExpr::Not(inner)
            }
            ScalarExpr::And(mut items) => {
                self.items(&mut items)?;
                ScalarExpr::And(items)
            }
            ScalarExpr::Or(mut items) => {
                self.items(&mut items)?;
                ScalarExpr::Or(items)
            }
            ScalarExpr::IsNull { mut expr, negated } => {
                self.in_place(&mut expr)?;
                ScalarExpr::IsNull { expr, negated }
            }
            ScalarExpr::Between {
                mut expr,
                mut low,
                mut high,
            } => {
                let types = [
                    self.semantic(self.binary_operand_types(
                        crate::ast::BinaryOp::GreaterEqual,
                        &expr,
                        &low,
                    ))?
                    .flatten(),
                    self.semantic(self.binary_operand_types(
                        crate::ast::BinaryOp::LessEqual,
                        &expr,
                        &high,
                    ))?
                    .flatten(),
                ];
                self.in_place(&mut expr)?;
                self.in_place(&mut low)?;
                self.in_place(&mut high)?;
                self.coerce_between(types, &mut expr, &mut low, &mut high)?;
                ScalarExpr::Between { expr, low, high }
            }
            ScalarExpr::InList {
                mut expr,
                mut list,
                negated,
            } => {
                self.in_place(&mut expr)?;
                self.items(&mut list)?;
                self.coerce_in_list(expr, list, negated)?
            }
            ScalarExpr::WindowCall {
                name,
                mut args,
                mut spec,
                mut filter,
                modifiers,
            } => {
                self.items(&mut args)?;
                if let Some(filter) = filter.as_deref_mut() {
                    self.in_place(filter)?;
                }
                self.items(&mut spec.partition_by)?;
                for order in &mut spec.order_by {
                    self.in_place(&mut order.expr)?;
                }
                if let Some(frame) = spec.frame.as_mut() {
                    self.frame_bound(&mut frame.start)?;
                    self.frame_bound(&mut frame.end)?;
                }
                ScalarExpr::WindowCall {
                    name,
                    args,
                    spec,
                    filter,
                    modifiers,
                }
            }
            ScalarExpr::Case {
                mut base,
                mut when,
                mut else_branch,
            } => {
                if let Some(base) = base.as_deref_mut() {
                    self.in_place(base)?;
                }
                for (condition, result) in &mut when {
                    self.in_place(condition)?;
                    self.in_place(result)?;
                }
                if let Some(otherwise) = else_branch.as_deref_mut() {
                    self.in_place(otherwise)?;
                }
                if base.is_some() {
                    let ty = self.common_type(
                        base.iter()
                            .map(Box::as_ref)
                            .chain(when.iter().map(|(condition, _)| condition)),
                    )?;
                    if let Some(ty) = ty {
                        if let Some(base) = base.as_deref_mut() {
                            self.common_cast(base, &ty)?;
                        }
                        for (condition, _) in &mut when {
                            self.common_cast(condition, &ty)?;
                        }
                    }
                }
                // `transformCaseExpr` selects the result type with the ELSE result first.
                let ty = self.common_type(
                    else_branch
                        .iter()
                        .map(Box::as_ref)
                        .chain(when.iter().map(|(_, result)| result)),
                )?;
                if let Some(ty) = ty {
                    for (_, result) in &mut when {
                        self.common_cast(result, &ty)?;
                    }
                    if let Some(otherwise) = else_branch.as_deref_mut() {
                        self.common_cast(otherwise, &ty)?;
                    }
                }
                ScalarExpr::Case {
                    base,
                    when,
                    else_branch,
                }
            }
            ScalarExpr::Cast {
                mut expr,
                ty,
                implicit,
            } => {
                self.fold_enum_array_constructor(&mut expr, &ty)?;
                let source = if !implicit && self.cast_requires_source(&ty)? {
                    let source = self.semantic(self.infer(&expr))?.flatten();
                    source
                        .map(|source| self.declared_source(&ty, source))
                        .transpose()?
                        .flatten()
                } else {
                    None
                };
                self.in_place(&mut expr)?;
                if let Some(folded) = self.fold_explicit_enum_cast(&expr, &ty)? {
                    return Ok(folded);
                }
                if let Some(source) = source {
                    self.wrap_declared(&mut expr, &source)?;
                }
                if self.literal_binding == LiteralBinding::Stored {
                    self.read_explicit_literal_input(&mut expr, &ty)?;
                }
                ScalarExpr::Cast { implicit, expr, ty }
            }
            ScalarExpr::InSubquery {
                mut expr,
                subquery,
                negated,
            } => {
                self.in_place(&mut expr)?;
                ScalarExpr::InSubquery {
                    expr,
                    subquery,
                    negated,
                }
            }
            other => other,
        })
    }

    fn bind_call(&mut self, mut call: BindingCall) -> Result<ScalarExpr, SQLError> {
        self.items(&mut call.arguments)?;
        for order in &mut call.order_by {
            self.in_place(&mut order.expr)?;
        }
        if let Some(filter) = call.filter.as_deref_mut() {
            self.in_place(filter)?;
        }
        let schema = self.schema;
        let params = self.params;
        let resolver = self.resolver;
        let control = self.control;
        let mut infer = |expression: &ScalarExpr| {
            super::scalar_type_inner_with_control(expression, schema, params, resolver, &control)
        };
        if let Some((binding, crate::ast::FunctionDispatch::NumericOperator(operator))) = call
            .binding
            .as_mut()
            .and_then(|binding| binding.dispatch.map(|dispatch| (binding, dispatch)))
        {
            let mut common = |expression: &ScalarExpr| {
                super::common::common_context_expression_type_with_control(
                    expression, schema, params, resolver, &control,
                )
            };
            if control.budget().is_none() {
                super::operators::numeric::bind_call(
                    operator,
                    binding,
                    &call.arguments,
                    schema,
                    params,
                    resolver,
                );
            } else {
                super::operators::numeric::bind_call_in_place_with_control(
                    operator,
                    binding,
                    &call.arguments,
                    self.memory,
                    &mut common,
                    &control,
                )?;
            }
            return Ok(call.into_expression());
        }
        if super::containment::is_operator(&call.name) {
            if control.budget().is_none() {
                super::containment::bind_unknown_arguments(
                    &mut call.arguments,
                    schema,
                    params,
                    resolver,
                );
            } else {
                super::containment::bind_unknown_arguments_in_place_with_control(
                    &mut call,
                    self.memory,
                    &mut infer,
                    &control,
                )?;
            }
        }
        if helpers::is_common_type_function(&call.name) {
            self.common_expressions(&mut call.arguments)?;
        }
        self.coerce_comparison_call(&mut call)?;
        self.coerce_compatible_array_call(&mut call)?;
        self.coerce_concat_call(&mut call)?;
        self.bind_optional_calls(&mut call, &mut infer)?;
        self.bind_enum_call(&mut call)?;
        if helpers::is_pg_typeof(&call.name) && call.arguments.len() == 1 {
            let ty = if super::is_unknown_literal(&call.arguments[0]) {
                None
            } else {
                self.semantic(self.infer(&call.arguments[0]))?.flatten()
            };
            let cast = control.copy_text("regtype")?;
            // A user-defined type is folded by OID: its name may be shadowed by a built-in type or depend on the search path.
            if let Some(ty) = ty.as_deref().filter(|ty| helpers::is_user_defined_type(ty)) {
                let oid = crate::catalog::type_metadata::pg_type_oid(ty);
                return self.cast(ScalarExpr::Literal(Value::Int(oid)), cast);
            }
            let name = ty.map_or_else(
                || control.copy_text("unknown"),
                |ty| ty.regtype_name_with_control(&control),
            )?;
            let name = self.retain(name);
            return self.cast(ScalarExpr::Literal(Value::Str(name)), cast);
        }
        Ok(call.into_expression())
    }
}

impl BindingCall {
    fn into_expression(self) -> ScalarExpr {
        ScalarExpr::Func {
            name: self.name,
            binding: self.binding,
            args: self.arguments,
            distinct: self.distinct,
            order_syntax: self.order_syntax,
            order_by: self.order_by,
            filter: self.filter,
        }
    }
}

#[cfg(test)]
mod tests;
