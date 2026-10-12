//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    Binder, BindingCall, ColumnType, FunctionBinding, FunctionTypeResolver, LiteralBinding,
    Produced, SQLError, SQLParam, ScalarExpr, ScalarTypeSchema, Value,
};
use crate::type_resolution::{
    common::{base_type, local_routine_name},
    scalar_type_inner_with_control,
};

impl Binder<'_, '_> {
    pub(super) fn bind_optional_calls(
        &mut self,
        call: &mut BindingCall,
        infer: &mut crate::type_resolution::call::InferType<'_>,
    ) -> Result<(), SQLError> {
        let schema = self.schema;
        let params = self.params;
        let resolver = self.resolver;
        let control = self.control;
        if control.budget().is_none() {
            call.name = crate::type_resolution::array_transform::bind_call(
                std::mem::take(&mut call.name),
                &mut call.binding,
                &mut call.arguments,
                schema,
                params,
                resolver,
            );
            call.name = crate::type_resolution::range::bind_call(
                std::mem::take(&mut call.name),
                &mut call.binding,
                &call.arguments,
                schema,
                params,
                resolver,
            );
            call.name = crate::type_resolution::fixed_builtin::bind_call(
                std::mem::take(&mut call.name),
                &mut call.binding,
                &mut call.arguments,
                schema,
                params,
                resolver,
            );
            bind_catalog_function(
                &call.name,
                &mut call.binding,
                &call.arguments,
                schema,
                params,
                resolver,
            );
        } else {
            crate::type_resolution::array_transform::bind_call_in_place_with_control(
                call,
                self.memory,
                infer,
                &control,
            )?;
            crate::type_resolution::range::bind_call_in_place_with_control(
                call,
                self.memory,
                infer,
                &control,
            )?;
            crate::type_resolution::fixed_builtin::bind_call_in_place_with_control(
                call,
                self.memory,
                params,
                infer,
                &control,
            )?;
        }
        Ok(())
    }

    pub(super) fn semantic<T>(&self, result: Result<T, SQLError>) -> Result<Option<T>, SQLError> {
        match result {
            Ok(value) => Ok(Some(value)),
            Err(error)
                if self.control.budget().is_some()
                    && matches!(error.sqlstate(), Some("53200" | "57014")) =>
            {
                Err(error)
            }
            Err(_) => Ok(None),
        }
    }

    pub(super) fn infer(
        &self,
        expression: &ScalarExpr,
    ) -> Result<Option<Produced<ColumnType>>, SQLError> {
        scalar_type_inner_with_control(
            expression,
            self.schema,
            self.params,
            self.resolver,
            &self.control,
        )
    }

    pub(super) fn retain<T>(&mut self, produced: Produced<T>) -> T {
        let (value, memory) = produced.into_parts();
        *self.memory = self.control.combine(self.memory.take(), memory);
        value
    }

    pub(super) fn in_place(&mut self, expression: &mut ScalarExpr) -> Result<(), SQLError> {
        let owned = std::mem::replace(expression, ScalarExpr::Literal(Value::Null));
        *expression = self.bind(owned)?;
        Ok(())
    }

    pub(super) fn items(&mut self, expressions: &mut [ScalarExpr]) -> Result<(), SQLError> {
        for expression in expressions {
            self.in_place(expression)?;
        }
        Ok(())
    }

    pub(super) fn common_type<'a>(
        &self,
        expressions: impl IntoIterator<Item = &'a ScalarExpr>,
    ) -> Result<Option<Produced<ColumnType>>, SQLError> {
        let mut common = None;
        let mut saw_expression = false;
        let mut saw_unknown = false;
        for expression in expressions {
            saw_expression = true;
            // `select_common_type` passes over `unknown` literals, which the selected type then reads.
            if crate::type_resolution::is_unknown_literal(expression) {
                saw_unknown = true;
                continue;
            }
            let Some(ty) = self.semantic(self.common_context(expression))? else {
                return Ok(None);
            };
            let Some(merged) = self.semantic(crate::type_resolution::common::merge_value_types(
                common,
                ty,
                &self.control,
            ))?
            else {
                return Ok(None);
            };
            common = merged;
        }
        if saw_unknown {
            if let Some(selected) = common.as_ref() {
                common = Some(base_type(selected).clone_with_control(&self.control)?);
            }
        }
        if saw_expression && common.is_none() {
            common = Some(ColumnType::Text.clone_with_control(&self.control)?);
        }
        Ok(common)
    }

    pub(super) fn common_context(
        &self,
        expression: &ScalarExpr,
    ) -> Result<Option<Produced<ColumnType>>, SQLError> {
        crate::type_resolution::common::common_context_expression_type_with_control(
            expression,
            self.schema,
            self.params,
            self.resolver,
            &self.control,
        )
    }

    pub(super) fn common_expressions(
        &mut self,
        expressions: &mut [ScalarExpr],
    ) -> Result<(), SQLError> {
        let Some(target) = self.common_type(expressions.iter())? else {
            return Ok(());
        };
        for expression in expressions {
            self.common_cast(expression, &target)?;
        }
        Ok(())
    }

    pub(super) fn common_cast(
        &mut self,
        expression: &mut ScalarExpr,
        target: &ColumnType,
    ) -> Result<(), SQLError> {
        let target = target.without_type_modifiers_with_control(&self.control)?;
        let source = self.semantic(self.common_context(expression))?.flatten();
        if let Some(source) = source {
            let source = source.without_type_modifiers_with_control(&self.control)?;
            if *source == *target
                || super::super::cast_compatibility::preserves_record_identity(&source, &target)
            {
                return Ok(());
            }
        }
        if self.coerce_collated_operand(expression, &target)? {
            return Ok(());
        }
        if let Some(folded) = self.fold_enum_literal(expression, &target)? {
            *expression = folded;
            return Ok(());
        }
        if let Some(folded) = self.read_unknown_literal(expression, &target)? {
            *expression = folded;
            return Ok(());
        }
        // The cast is resolved again at evaluation, so a user-defined type is named by identity rather than by a search-path-dependent name.
        let ty = target.catalog_name_with_control(&self.control)?;
        self.install_cast(expression, ty)
    }

    /// Validate a stored cast before its parent selects additional coercions. An unknown literal reads its base input now; its written cast keeps the modifier at the original boundary.
    pub(super) fn read_explicit_literal_input(
        &mut self,
        expression: &mut ScalarExpr,
        ty: &str,
    ) -> Result<(), SQLError> {
        let target = match ColumnType::from_sql_name_with_control(ty, &self.control) {
            Ok(target) => target,
            Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => return Err(error),
            Err(_) => {
                let Some(target) = self.user_cast_target(ty)? else {
                    return Ok(());
                };
                target.clone_with_control(&self.control)?
            }
        };
        let source = self.infer(expression)?;
        crate::type_resolution::cast_compatibility::validate_explicit_cast_with_control(
            source.as_deref(),
            &target,
            &self.control,
        )?;
        let mut original = &*expression;
        while let Some(clause) = crate::catalog::collations::expression::scalar_clause(original)? {
            original = clause.argument;
        }
        if !matches!(original, ScalarExpr::Literal(Value::Str(_))) {
            return Ok(());
        }
        let input =
            crate::type_resolution::literal_input_type_with_control(&target, &self.control)?;
        if self.coerce_collated_operand(expression, &input)? {
            return Ok(());
        }
        if let Some(constant) = self.read_unknown_literal(expression, &input)? {
            *expression = constant;
        }
        Ok(())
    }

    /// `coerce_to_common_type` reads an `unknown` string constant with the selected type's input function, which reports what the type rejects, and stores the typed constant. A type whose input function consults the catalog keeps the cast form that the binding of a stored expression resolves.
    fn read_unknown_literal(
        &mut self,
        expression: &ScalarExpr,
        target: &ColumnType,
    ) -> Result<Option<ScalarExpr>, SQLError> {
        let ScalarExpr::Literal(value @ Value::Str(text)) = expression else {
            return Ok(None);
        };
        if crate::type_resolution::catalog_input_type(target)
            && !(self.literal_binding == LiteralBinding::Stored
                && crate::expr::requires_catalog_constant_input(target))
        {
            return Ok(None);
        }
        // Ordinary binding leaves invalid inputs to analysis/evaluation. Stored syntax must report them before a surrounding coercion selects a different error.
        let converted = if self.literal_binding == LiteralBinding::Stored
            && crate::expr::requires_catalog_constant_input(target)
        {
            let Some(catalog) = self
                .resolver
                .and_then(FunctionTypeResolver::catalog_input_functions)
            else {
                return Ok(None);
            };
            Ok(self
                .control
                .retain_external_value(catalog.read_unknown_input(text, target)?)?)
        } else {
            crate::assignment::conversion::convert_value_to_column_type_with_control(
                self.control.copy_value(value)?,
                target,
                &self.control,
            )
        };
        let value = match converted {
            Ok(value) => value,
            Err(error)
                if self.literal_binding != LiteralBinding::Ordinary
                    || matches!(error.sqlstate(), Some("53200" | "57014")) =>
            {
                return Err(error);
            }
            Err(_) => return Ok(None),
        };
        let memory = self.control.reserve(size_of::<ScalarExpr>())?;
        *self.memory = self.control.combine(self.memory.take(), memory);
        let ty = target.catalog_name_with_control(&self.control)?;
        let ty = self.retain(ty);
        let bound_type = target.clone_with_control(&self.control)?;
        let bound_type = self.retain(bound_type);
        let value = self.retain(value);
        Ok(Some(ScalarExpr::TypedLiteral {
            collation_oid: None,
            composite_source: None,
            value,
            ty,
            bound_type: Some(bound_type),
            parameter_index: None,
        }))
    }

    pub(super) fn wrap_declared(
        &mut self,
        expression: &mut ScalarExpr,
        ty: &ColumnType,
    ) -> Result<(), SQLError> {
        let name = ty.catalog_name_with_control(&self.control)?;
        if matches!(expression, ScalarExpr::Cast {ty, ..} if ty.eq_ignore_ascii_case(&name)) {
            return Ok(());
        }
        self.install_cast(expression, name)
    }

    fn install_cast(
        &mut self,
        expression: &mut ScalarExpr,
        ty: Produced<String>,
    ) -> Result<(), SQLError> {
        let memory = self.control.reserve(size_of::<ScalarExpr>())?;
        *self.memory = self.control.combine(self.memory.take(), memory);
        let ty = self.retain(ty);
        let inner = std::mem::replace(expression, ScalarExpr::Literal(Value::Null));
        *expression = ScalarExpr::Cast {
            implicit: true,
            expr: Box::new(inner),
            ty,
        };
        Ok(())
    }

    pub(super) fn cast(
        &mut self,
        expression: ScalarExpr,
        ty: Produced<String>,
    ) -> Result<ScalarExpr, SQLError> {
        let memory = self.control.reserve(size_of::<ScalarExpr>())?;
        *self.memory = self.control.combine(self.memory.take(), memory);
        let ty = self.retain(ty);
        Ok(ScalarExpr::Cast {
            implicit: true,
            expr: Box::new(expression),
            ty,
        })
    }

    pub(super) fn cast_requires_source(&self, target: &str) -> Result<bool, SQLError> {
        let Some(target) = self
            .semantic(super::super::inference::resolved_type_name(
                target,
                self.resolver,
                &self.control,
            ))?
            .flatten()
        else {
            return Ok(false);
        };
        let mut target = &*target;
        while let ColumnType::Array(element) | ColumnType::Domain { base: element, .. } = target {
            self.control.check()?;
            target = element;
        }
        Ok(target.is_character_string()
            || matches!(
                target,
                ColumnType::Bytea
                    | ColumnType::Oid
                    | ColumnType::Xid
                    | ColumnType::Int2Vector
                    | ColumnType::OidVector
                    | ColumnType::InternalChar
            ))
    }

    pub(super) fn declared_source(
        &self,
        target: &str,
        source: Produced<ColumnType>,
    ) -> Result<Option<Produced<ColumnType>>, SQLError> {
        if self.is_character_target(target)? {
            return match base_type(&source) {
                source @ (ColumnType::Int2Vector
                | ColumnType::OidVector
                | ColumnType::Real
                | ColumnType::InternalChar) => Ok(Some(source.clone_with_control(&self.control)?)),
                _ => Ok(None),
            };
        }
        Ok(Some(source))
    }

    fn is_character_target(&self, target: &str) -> Result<bool, SQLError> {
        Ok(self
            .semantic(ColumnType::from_sql_name_with_control(
                target,
                &self.control,
            ))?
            .is_some_and(|ty| ty.is_character_string()))
    }

    pub(super) fn frame_bound(
        &mut self,
        bound: &mut crate::ScalarFrameBound,
    ) -> Result<(), SQLError> {
        match bound {
            crate::ScalarFrameBound::Preceding(expression)
            | crate::ScalarFrameBound::Following(expression) => self.in_place(expression),
            crate::ScalarFrameBound::UnboundedPreceding
            | crate::ScalarFrameBound::UnboundedFollowing
            | crate::ScalarFrameBound::CurrentRow => Ok(()),
        }
    }
}

/// A catalog type created by a user, or an array of one.
pub(super) fn is_user_defined_type(ty: &ColumnType) -> bool {
    match ty {
        ColumnType::Enum(_) | ColumnType::Composite(_) | ColumnType::Domain { .. } => true,
        ColumnType::Array(element) => is_user_defined_type(element),
        _ => false,
    }
}

pub(super) fn is_pg_typeof(name: &str) -> bool {
    name.eq_ignore_ascii_case("pg_typeof") || name.eq_ignore_ascii_case("pg_catalog.pg_typeof")
}

pub(super) fn is_common_type_function(name: &str) -> bool {
    ["coalesce", "greatest", "least"]
        .iter()
        .any(|candidate| name.eq_ignore_ascii_case(candidate))
}

pub(super) fn bind_catalog_function(
    name: &str,
    binding: &mut Option<FunctionBinding>,
    args: &[ScalarExpr],
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) {
    if binding.is_some() {
        return;
    }
    if crate::ast::is_builtin_aggregate_function(&local_routine_name(name)) {
        return;
    }
    if crate::type_resolution::functions::builtin_function_type_inner(
        name,
        None,
        args,
        &[],
        schema,
        params,
        None,
    )
    .ok()
    .flatten()
    .is_some()
    {
        return;
    }
    let Some(resolver) = resolver else {
        return;
    };
    let Ok((argument_names, argument_types, explicit_variadic)) =
        crate::type_resolution::function_call_argument_signature(
            args,
            schema,
            params,
            Some(resolver),
        )
    else {
        return;
    };
    if let Ok(Some(selected)) = resolver.resolve_function_overload(
        name,
        None,
        &argument_names,
        &argument_types,
        explicit_variadic,
    ) {
        if resolver
            .is_scalar_function_binding(&selected.binding)
            .is_ok_and(|is_scalar| is_scalar)
        {
            *binding = Some(selected.binding);
        }
    }
}
