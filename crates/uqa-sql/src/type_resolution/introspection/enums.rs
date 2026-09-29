//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Enum support calls and `unknown` literals coerced to enum types. With a catalog resolver, a literal becomes a typed enum constant once, as `PostgreSQL` parse analysis calls `enum_in`; without one, a cast to the schema-qualified type defers the conversion to evaluation.

use super::{Binder, BindingCall, ColumnType, FunctionBinding, SQLError, ScalarExpr, Value};
use crate::ast::{EnumFunctionOperation, FunctionDispatch};
use crate::expr::enums::{fold_unknown_literal, is_enum_bearing};
use crate::type_resolution::enums::{enum_argument_type, EnumArgumentType};

impl Binder<'_, '_> {
    /// Convert an `unknown` literal coerced to an enum or enum-array type into a typed constant. Invalid input is reported in strict mode and otherwise left to the evaluated cast, which reports the same error.
    pub(super) fn fold_enum_literal(
        &mut self,
        expression: &ScalarExpr,
        target: &ColumnType,
    ) -> Result<Option<ScalarExpr>, SQLError> {
        if !is_enum_bearing(target) || self.control.budget().is_some() {
            return Ok(None);
        }
        let ScalarExpr::Literal(value @ (Value::Str(_) | Value::Null)) = expression else {
            return Ok(None);
        };
        let Some(catalog) = self.resolver.and_then(|resolver| resolver.enum_labels()) else {
            return Ok(None);
        };
        match fold_unknown_literal(Some(catalog), value, target) {
            Ok(Some(value)) => Ok(Some(ScalarExpr::TypedLiteral {
                value,
                ty: target.catalog_name(),
                bound_type: Some(target.clone()),
                parameter_index: None,
            })),
            Ok(None) => Ok(None),
            Err(error) if self.strict_literals => Err(error),
            Err(_) => Ok(None),
        }
    }

    /// `'label'::enum_type` and `'{...}'::enum_type[]` over an `unknown` literal.
    pub(super) fn fold_explicit_enum_cast(
        &mut self,
        expression: &ScalarExpr,
        ty: &str,
    ) -> Result<Option<ScalarExpr>, SQLError> {
        if !matches!(expression, ScalarExpr::Literal(Value::Str(_) | Value::Null))
            || self.control.budget().is_some()
        {
            return Ok(None);
        }
        let Some(target) = self.user_cast_target(ty)? else {
            return Ok(None);
        };
        self.fold_literal_cast(expression, &target, ty)
    }

    /// The catalog type a cast names, when it is not a built-in type.
    fn user_cast_target(&mut self, ty: &str) -> Result<Option<ColumnType>, SQLError> {
        let Some(resolver) = self.resolver else {
            return Ok(None);
        };
        if ColumnType::from_sql_name(ty).is_ok() {
            return Ok(None);
        }
        Ok(self.semantic(resolver.resolve_type_name(ty))?.flatten())
    }

    /// An explicit cast of an `unknown` literal: an enum or enum-array target takes the constant itself; a domain over one takes the constant of its base type under the domain cast, which `coerce_type` builds as `CoerceToDomain` over the converted literal.
    fn fold_literal_cast(
        &mut self,
        expression: &ScalarExpr,
        target: &ColumnType,
        ty: &str,
    ) -> Result<Option<ScalarExpr>, SQLError> {
        if is_enum_bearing(target) {
            return self.fold_enum_literal(expression, target);
        }
        if !matches!(target, ColumnType::Domain { .. }) {
            return Ok(None);
        }
        let base = crate::type_resolution::common::base_type(target);
        if !is_enum_bearing(base) {
            return Ok(None);
        }
        let base = base.clone();
        let Some(constant) = self.fold_enum_literal(expression, &base)? else {
            return Ok(None);
        };
        let ty = self.control.copy_text(ty)?;
        self.cast(constant, ty).map(Some)
    }

    /// `ARRAY[...]::T[]` converts each `unknown` element with the element type's input function, as `transformTypeCast` passes the target element type to `transformArrayExpr`; nested constructors take the same element type.
    pub(super) fn fold_enum_array_constructor(
        &mut self,
        expression: &mut ScalarExpr,
        ty: &str,
    ) -> Result<(), SQLError> {
        if !matches!(expression, ScalarExpr::Array(_)) || self.control.budget().is_some() {
            return Ok(());
        }
        let Some(target) = self.user_cast_target(ty)? else {
            return Ok(());
        };
        let Some(mut element) = crate::type_resolution::common::array_element_type(&target) else {
            return Ok(());
        };
        while let ColumnType::Array(inner) = element {
            element = inner;
        }
        let element = element.clone();
        if !is_enum_bearing(crate::type_resolution::common::base_type(&element)) {
            return Ok(());
        }
        let ScalarExpr::Array(items) = expression else {
            return Ok(());
        };
        self.fold_enum_array_items(items, &element)
    }

    fn fold_enum_array_items(
        &mut self,
        items: &mut [ScalarExpr],
        element: &ColumnType,
    ) -> Result<(), SQLError> {
        for item in items {
            if let ScalarExpr::Array(inner) = item {
                self.fold_enum_array_items(inner, element)?;
            } else if let Some(folded) =
                self.fold_literal_cast(item, element, &element.catalog_name())?
            {
                *item = folded;
            }
        }
        Ok(())
    }

    /// Bind an `anyenum` support call to its concrete enum type and coerce its `unknown` arguments. A visible user routine that matches the call exactly takes precedence, as an exact candidate outranks a polymorphic one.
    pub(super) fn bind_enum_call(&mut self, call: &mut BindingCall) -> Result<(), SQLError> {
        if call.binding.is_some() {
            return Ok(());
        }
        let Some(operation) = EnumFunctionOperation::from_call(&call.name, call.arguments.len())
        else {
            return Ok(());
        };
        let mut types = Vec::with_capacity(call.arguments.len());
        for argument in &call.arguments {
            if crate::scalar_call_argument(argument).is_ok_and(|argument| argument.name.is_some()) {
                return Ok(());
            }
            types.push(self.semantic(self.common_context(argument))?.flatten());
        }
        let borrowed = types.iter().map(|ty| ty.as_deref()).collect::<Vec<_>>();
        let EnumArgumentType::Enum(reference) = enum_argument_type(operation, &borrowed) else {
            return Ok(());
        };
        let target = ColumnType::Enum(reference.clone());
        if self.user_routine_matches_exactly(call)? {
            return Ok(());
        }
        let binding = FunctionBinding::dispatched_with_control(
            FunctionDispatch::Enum {
                operation,
                type_oid: reference.oid,
            },
            &self.control,
        )?;
        call.binding = Some(self.retain(binding));
        for (index, argument) in call.arguments.iter_mut().enumerate() {
            if types[index].is_some() {
                continue;
            }
            if index < operation.enum_argument_count() {
                self.common_cast(argument, &target)?;
            } else {
                self.common_cast(argument, &ColumnType::BigInteger)?;
            }
        }
        Ok(())
    }

    fn user_routine_matches_exactly(&self, call: &BindingCall) -> Result<bool, SQLError> {
        let Some(resolver) = self.resolver else {
            return Ok(false);
        };
        let Some((names, types, variadic)) =
            self.semantic(crate::type_resolution::function_call_argument_signature(
                &call.arguments,
                self.schema,
                self.params,
                Some(resolver),
            ))?
        else {
            return Ok(false);
        };
        let selected = self
            .semantic(
                resolver.resolve_function_overload(&call.name, None, &names, &types, variadic),
            )?
            .flatten();
        Ok(selected.is_some_and(|selected| {
            !selected.binding.builtin && selected.exact_matches == selected.known_arguments
        }))
    }
}
