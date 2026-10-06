//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored expressions keep the enum constants that parse analysis coerces from `unknown` literals by label identity, as `PostgreSQL` stores `Const` nodes: `ALTER TYPE ... RENAME VALUE` changes their label and never their meaning, and reloading the catalog never converts label text again. Binding decides every coercion; this module carries the constants it produced back into the stored tree. Binding changes a stored tree only by adding casts, binding calls and converting literals, so the two trees correspond node for node once the added casts are skipped.

use super::{is_unknown_literal, FunctionTypeResolver};
use crate::ast::ColumnType;
use crate::expr::enums::{fold_unknown_literal, is_enum_bearing, EnumLabelCatalog};
use crate::schema::ScalarTypeSchema;
use crate::{SQLError, SQLParam, ScalarExpr, ScalarFrameBound};
use uqa_core::Value;

/// Whether an expression has an `unknown` literal outside its subqueries, the only nodes that binding converts to enum constants.
#[must_use]
pub fn contains_unknown_literal(expression: &ScalarExpr) -> bool {
    let mut found = false;
    expression.visit(&mut |node| {
        found |= is_unknown_literal(node);
    });
    found
}

/// Replace every `unknown` literal that binding coerces to an enum, an enum array or a domain over one by the constant binding produced for it. Returns whether any literal changed.
pub fn fold_stored_enum_constants(
    expression: &mut ScalarExpr,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
) -> Result<bool, SQLError> {
    let Some(catalog) = resolver.enum_labels() else {
        return Ok(false);
    };
    let Some(bound) =
        super::introspection::bind_catalog_constants(expression, schema, params, resolver)?
    else {
        return Ok(false);
    };
    transfer(expression, &bound, &mut EnumConstants { catalog })
}

/// Store the coercions binding adds to function arguments and the operands of an operator, as `PostgreSQL` stores the `RelabelType` nodes and coerced constants of an analyzed expression: an `unknown` literal becomes the constant the selected operand type's input function read (`1` in `a + '1'`, `'16384'::oid` against a `regclass` column), an operand of `oid` or one of its alias types gains its cast to `oid`, and an array operand its cast to `oid[]`. Selected function arguments retain their input casts, including runtime text-to-regclass conversion and integer-to-bigint conversion. Constants of the types whose input consults the catalog keep their cast, which the stored expression's binding resolves. Returns whether anything changed.
pub fn store_operand_coercions(
    expression: &mut ScalarExpr,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
) -> Result<bool, SQLError> {
    let bound = super::introspection::bind_type_introspection_with_resolver(
        expression.clone(),
        schema,
        params,
        resolver,
    );
    transfer(expression, &bound, &mut OperatorCoercions)
}

/// What a transfer takes from the bound copy of a stored expression.
trait Folding {
    /// A stored `unknown` literal and what binding made of it.
    fn literal(&mut self, stored: &mut ScalarExpr, bound: &ScalarExpr) -> Result<bool, SQLError>;
    /// The casts binding added around a node, outermost first, which the stored node may take over.
    fn added_casts(&mut self, stored: &mut ScalarExpr, casts: &[&str]) -> bool;
    /// Casts at a selected function's argument boundary retain its declared input type.
    fn argument_casts(&mut self, stored: &mut ScalarExpr, casts: &[&str]) -> bool {
        self.added_casts(stored, casts)
    }
}

struct EnumConstants<'a> {
    catalog: &'a dyn EnumLabelCatalog,
}

impl Folding for EnumConstants<'_> {
    fn literal(&mut self, stored: &mut ScalarExpr, bound: &ScalarExpr) -> Result<bool, SQLError> {
        let ScalarExpr::TypedLiteral {
            bound_type: Some(target),
            ..
        } = bound
        else {
            return Ok(false);
        };
        fold_literal(stored, target, self.catalog)
    }

    fn added_casts(&mut self, _: &mut ScalarExpr, _: &[&str]) -> bool {
        false
    }
}

struct OperatorCoercions;

impl Folding for OperatorCoercions {
    fn literal(&mut self, stored: &mut ScalarExpr, bound: &ScalarExpr) -> Result<bool, SQLError> {
        let ScalarExpr::TypedLiteral {
            value,
            bound_type: Some(target),
            ..
        } = bound
        else {
            return Ok(false);
        };
        // Enum constants are stored by label identity, and a catalog input type keeps its cast.
        if is_enum_bearing(target) || super::catalog_input_type(target) {
            return Ok(false);
        }
        *stored = ScalarExpr::TypedLiteral {
            value: value.clone(),
            ty: target.catalog_name(),
            bound_type: None,
            parameter_index: None,
        };
        Ok(true)
    }

    fn added_casts(&mut self, stored: &mut ScalarExpr, casts: &[&str]) -> bool {
        if casts.is_empty() || !casts.iter().all(|ty| *ty == "oid" || *ty == "oid[]") {
            return false;
        }
        store_casts(stored, casts)
    }

    fn argument_casts(&mut self, stored: &mut ScalarExpr, casts: &[&str]) -> bool {
        store_casts(stored, casts)
    }
}

fn store_casts(stored: &mut ScalarExpr, casts: &[&str]) -> bool {
    for ty in casts.iter().rev() {
        let inner = std::mem::replace(stored, ScalarExpr::Literal(Value::Null));
        *stored = ScalarExpr::Cast {
            expr: Box::new(inner),
            ty: (*ty).to_string(),
        };
    }
    !casts.is_empty()
}

/// Casts that binding added around a node are not part of the stored tree. A cast the stored tree already has keeps its written type name, which binding never changes. Returns the node under the added casts and their type names, outermost first.
fn split_added_casts<'a>(
    stored: &ScalarExpr,
    mut bound: &'a ScalarExpr,
) -> (&'a ScalarExpr, Vec<&'a str>) {
    let mut added = Vec::new();
    while let ScalarExpr::Cast { expr, ty } = bound {
        if matches!(stored, ScalarExpr::Cast { ty: written, .. } if written == ty) {
            break;
        }
        added.push(ty.as_str());
        bound = expr;
    }
    (bound, added)
}

fn transfer(
    stored: &mut ScalarExpr,
    bound: &ScalarExpr,
    folding: &mut dyn Folding,
) -> Result<bool, SQLError> {
    let (bound, added) = split_added_casts(stored, bound);
    let mut changed = if is_unknown_literal(stored) {
        folding.literal(stored, bound)?
    } else {
        transfer_children(stored, bound, folding)?
    };
    changed |= folding.added_casts(stored, &added);
    Ok(changed)
}

#[expect(
    clippy::too_many_lines,
    reason = "one structural walk pairs every scalar variant with its bound counterpart"
)]
fn transfer_children(
    stored: &mut ScalarExpr,
    bound: &ScalarExpr,
    folding: &mut dyn Folding,
) -> Result<bool, SQLError> {
    Ok(match (stored, bound) {
        // An explicit cast of a literal whose conversion binding folded: the constant replaces the literal and the cast remains, converting a value of its own type.
        (ScalarExpr::Cast { expr, .. }, ScalarExpr::TypedLiteral { .. }) => {
            transfer(expr, bound, folding)?
        }
        (ScalarExpr::Cast { expr, .. }, ScalarExpr::Cast { expr: bound, .. })
        | (ScalarExpr::UnaryMinus(expr), ScalarExpr::UnaryMinus(bound))
        | (ScalarExpr::Not(expr), ScalarExpr::Not(bound))
        | (ScalarExpr::IsNull { expr, .. }, ScalarExpr::IsNull { expr: bound, .. })
        | (ScalarExpr::InSubquery { expr, .. }, ScalarExpr::InSubquery { expr: bound, .. }) => {
            transfer(expr, bound, folding)?
        }
        (
            ScalarExpr::Func {
                name,
                args,
                order_by,
                filter,
                ..
            },
            ScalarExpr::Func {
                name: bound_name,
                binding: bound_binding,
                args: bound_args,
                order_by: bound_order,
                filter: bound_filter,
                ..
            },
        ) => {
            // Binding may replace a call by another with rearranged arguments; such a call keeps its literals.
            if name != bound_name
                || order_by.len() != bound_order.len()
                || filter.is_some() != bound_filter.is_some()
            {
                return Ok(false);
            }
            let mut changed =
                transfer_call_arguments(args, bound_args, bound_binding.as_ref(), folding)?;
            for (order, bound) in order_by.iter_mut().zip(bound_order) {
                changed |= transfer(&mut order.expr, &bound.expr, folding)?;
            }
            if let (Some(filter), Some(bound)) = (filter, bound_filter) {
                changed |= transfer(filter, bound, folding)?;
            }
            changed
        }
        (ScalarExpr::Array(items), ScalarExpr::Array(bound))
        | (ScalarExpr::Row(items), ScalarExpr::Row(bound))
        | (ScalarExpr::And(items), ScalarExpr::And(bound))
        | (ScalarExpr::Or(items), ScalarExpr::Or(bound)) => transfer_all(items, bound, folding)?,
        (
            ScalarExpr::Binary { lhs, rhs, .. },
            ScalarExpr::Binary {
                lhs: bound_lhs,
                rhs: bound_rhs,
                ..
            },
        ) => transfer(lhs, bound_lhs, folding)? | transfer(rhs, bound_rhs, folding)?,
        (
            ScalarExpr::Between { expr, low, high },
            ScalarExpr::Between {
                expr: bound_expr,
                low: bound_low,
                high: bound_high,
            },
        ) => {
            transfer(expr, bound_expr, folding)?
                | transfer(low, bound_low, folding)?
                | transfer(high, bound_high, folding)?
        }
        (
            ScalarExpr::InList { expr, list, .. },
            ScalarExpr::InList {
                expr: bound_expr,
                list: bound_list,
                ..
            },
        ) => transfer(expr, bound_expr, folding)? | transfer_all(list, bound_list, folding)?,
        (
            ScalarExpr::WindowCall {
                name,
                args,
                spec,
                filter,
                ..
            },
            ScalarExpr::WindowCall {
                name: bound_name,
                args: bound_args,
                spec: bound_spec,
                filter: bound_filter,
                ..
            },
        ) => {
            if name != bound_name
                || filter.is_some() != bound_filter.is_some()
                || args.len() != bound_args.len()
                || spec.partition_by.len() != bound_spec.partition_by.len()
                || spec.order_by.len() != bound_spec.order_by.len()
            {
                return Ok(false);
            }
            let mut changed = transfer_all(args, bound_args, folding)?;
            if let (Some(filter), Some(bound)) = (filter, bound_filter) {
                changed |= transfer(filter, bound, folding)?;
            }
            changed |= transfer_all(&mut spec.partition_by, &bound_spec.partition_by, folding)?;
            for (order, bound) in spec.order_by.iter_mut().zip(&bound_spec.order_by) {
                changed |= transfer(&mut order.expr, &bound.expr, folding)?;
            }
            if let (Some(frame), Some(bound)) = (spec.frame.as_mut(), bound_spec.frame.as_ref()) {
                changed |= transfer_frame_bound(&mut frame.start, &bound.start, folding)?;
                changed |= transfer_frame_bound(&mut frame.end, &bound.end, folding)?;
            }
            changed
        }
        (
            ScalarExpr::Case {
                base,
                when,
                else_branch,
            },
            ScalarExpr::Case {
                base: bound_base,
                when: bound_when,
                else_branch: bound_else,
            },
        ) => {
            if base.is_some() != bound_base.is_some()
                || when.len() != bound_when.len()
                || else_branch.is_some() != bound_else.is_some()
            {
                return Ok(false);
            }
            let mut changed = false;
            if let (Some(base), Some(bound)) = (base, bound_base) {
                changed |= transfer(base, bound, folding)?;
            }
            for ((condition, result), (bound_condition, bound_result)) in
                when.iter_mut().zip(bound_when)
            {
                changed |= transfer(condition, bound_condition, folding)?;
                changed |= transfer(result, bound_result, folding)?;
            }
            if let (Some(branch), Some(bound)) = (else_branch, bound_else) {
                changed |= transfer(branch, bound, folding)?;
            }
            changed
        }
        _ => false,
    })
}

/// Fixed built-ins may reorder named arguments and insert defaults in the bound
/// copy. Map supplied values back through the selected signature, retaining the
/// original argument markers and written order in stored syntax.
fn transfer_call_arguments(
    stored: &mut [ScalarExpr],
    bound: &[ScalarExpr],
    binding: Option<&crate::ast::FunctionBinding>,
    folding: &mut dyn Folding,
) -> Result<bool, SQLError> {
    let selected = binding.filter(|binding| binding.builtin);
    let positions = if let Some(binding) = selected {
        let arguments = crate::scalar_call_arguments(stored)?;
        let names = arguments
            .iter()
            .map(|arg| arg.name.map(str::to_string))
            .collect::<Vec<_>>();
        super::fixed_builtin::resolve_fixed_builtin_call(
            &binding.name,
            Some(binding),
            &names,
            &vec![None; stored.len()],
            arguments.iter().any(|arg| arg.explicit_variadic),
            None,
        )?
        .and_then(|selected| selected.builtin_argument_positions)
    } else {
        None
    };
    if positions.is_none() && stored.len() != bound.len() {
        return Ok(false);
    }
    let mut changed = false;
    for (index, argument) in stored.iter_mut().enumerate() {
        let (argument, position) = if let Some(positions) = &positions {
            (call_argument_value_mut(argument), positions[index])
        } else {
            (argument, index)
        };
        let (bound, casts) = split_added_casts(argument, &bound[position]);
        changed |= transfer(argument, bound, folding)?;
        changed |= folding.argument_casts(argument, &casts);
    }
    Ok(changed)
}

fn call_argument_value_mut(expression: &mut ScalarExpr) -> &mut ScalarExpr {
    if !matches!(expression, ScalarExpr::Func { binding: Some(binding), .. }
        if matches!(binding.dispatch, Some(crate::ast::FunctionDispatch::NamedArgument | crate::ast::FunctionDispatch::VariadicArgument)))
    {
        return expression;
    }
    let ScalarExpr::Func { args, .. } = expression else {
        unreachable!("argument marker is a structural call");
    };
    call_argument_value_mut(args.last_mut().expect("validated argument marker"))
}

fn transfer_all(
    stored: &mut [ScalarExpr],
    bound: &[ScalarExpr],
    folding: &mut dyn Folding,
) -> Result<bool, SQLError> {
    if stored.len() != bound.len() {
        return Ok(false);
    }
    let mut changed = false;
    for (stored, bound) in stored.iter_mut().zip(bound) {
        changed |= transfer(stored, bound, folding)?;
    }
    Ok(changed)
}

fn transfer_frame_bound(
    stored: &mut ScalarFrameBound,
    bound: &ScalarFrameBound,
    folding: &mut dyn Folding,
) -> Result<bool, SQLError> {
    match (stored, bound) {
        (ScalarFrameBound::Preceding(stored), ScalarFrameBound::Preceding(bound))
        | (ScalarFrameBound::Following(stored), ScalarFrameBound::Following(bound)) => {
            transfer(stored, bound, folding)
        }
        _ => Ok(false),
    }
}

/// Convert the stored literal itself with the type binding chose for it, so every constant carries the label its own text names.
fn fold_literal(
    stored: &mut ScalarExpr,
    target: &ColumnType,
    catalog: &dyn EnumLabelCatalog,
) -> Result<bool, SQLError> {
    if !is_enum_bearing(target) {
        return Ok(false);
    }
    let ScalarExpr::Literal(value) = stored else {
        return Ok(false);
    };
    let Some(value) = fold_unknown_literal(Some(catalog), value, target)? else {
        return Ok(false);
    };
    *stored = stored_enum_constant(value, target);
    Ok(true)
}

/// The stored form of an enum constant: its label-identity value under the type's OID identity. The type is resolved from the identity when the expression is bound, so no cached name can go stale.
#[must_use]
pub fn stored_enum_constant(value: Value, target: &ColumnType) -> ScalarExpr {
    ScalarExpr::TypedLiteral {
        value,
        ty: target.catalog_name(),
        bound_type: None,
        parameter_index: None,
    }
}
