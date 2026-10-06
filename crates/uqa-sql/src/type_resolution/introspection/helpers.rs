//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    Binder, BindingCall, ColumnType, FunctionBinding, FunctionTypeResolver, Produced, SQLError,
    SQLParam, ScalarExpr, ScalarTypeSchema, Value,
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
            if *source == *target {
                return Ok(());
            }
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
        let ty = self.control.copy_text(&target.catalog_name())?;
        self.install_cast(expression, ty)
    }

    /// `coerce_to_common_type` reads an `unknown` string constant with the selected type's input function, which reports what the type rejects, and stores the typed constant. A type whose input function consults the catalog keeps the cast form that the binding of a stored expression resolves.
    fn read_unknown_literal(
        &mut self,
        expression: &ScalarExpr,
        target: &ColumnType,
    ) -> Result<Option<ScalarExpr>, SQLError> {
        let ScalarExpr::Literal(value @ Value::Str(_)) = expression else {
            return Ok(None);
        };
        if crate::type_resolution::catalog_input_type(target) {
            return Ok(None);
        }
        // Binding reports no semantic errors: a literal the type rejects keeps its cast, which analysis and evaluation report.
        let value = match crate::assignment::conversion::convert_value_to_column_type(
            value.clone(),
            target,
        ) {
            Ok(value) => value,
            Err(error) if self.strict_literals => return Err(error),
            Err(_) => return Ok(None),
        };
        let memory = self.control.reserve(size_of::<ScalarExpr>())?;
        *self.memory = self.control.combine(self.memory.take(), memory);
        let ty = self.control.copy_text(&target.catalog_name())?;
        let ty = self.retain(ty);
        Ok(Some(ScalarExpr::TypedLiteral {
            value,
            ty,
            bound_type: Some(target.clone()),
            parameter_index: None,
        }))
    }

    pub(super) fn wrap_declared(
        &mut self,
        expression: &mut ScalarExpr,
        ty: &ColumnType,
    ) -> Result<(), SQLError> {
        let name = self.control.copy_text(&ty.catalog_name())?;
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
        let mut target = target.trim();
        while let Some(element) = target.strip_suffix("[]") {
            target = element.trim_end();
        }
        if self.is_character_target(target)? {
            return Ok(true);
        }
        Ok([
            "bytea",
            "pg_catalog.bytea",
            "oid",
            "pg_catalog.oid",
            "xid",
            "pg_catalog.xid",
            "text",
            "pg_catalog.text",
            "int2vector",
            "pg_catalog.int2vector",
            "oidvector",
            "pg_catalog.oidvector",
        ]
        .iter()
        .any(|name| target.eq_ignore_ascii_case(name)))
    }

    pub(super) fn declared_source(
        &self,
        target: &str,
        source: Produced<ColumnType>,
    ) -> Result<Option<Produced<ColumnType>>, SQLError> {
        if self.is_character_target(target)? {
            return match base_type(&source) {
                source @ (ColumnType::Int2Vector | ColumnType::OidVector | ColumnType::Real) => {
                    Ok(Some(source.clone_with_control(&self.control)?))
                }
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
