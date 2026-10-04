//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Static type and routine binding for generated-column expressions.

use crate::ast::{
    BinaryOp, ColumnDef, Expr, FunctionBinding, FunctionDispatch, FunctionReturns,
    GeneratedFunctionDependency, RangeSubtype,
};
use crate::schema::SchemaExpressionCatalog;
use crate::{routines::routine_signature_types, type_resolution::canonical_routine_type_name};
use crate::{semantics::builtin_function_dispatch_name, ColumnType, SQLError};
use uqa_core::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::schema) enum GenerationType {
    Null,
    UnknownLiteral(String),
    Boolean,
    Void,
    SmallInteger,
    Integer,
    BigInteger,
    Oid,
    Xid,
    Real,
    Numeric,
    Text,
    Uuid,
    Bytea,
    Json,
    JsonB,
    Array(Box<GenerationType>),
    Date,
    Time,
    TimeTz,
    Timestamp,
    TimestampTz,
    Interval,
    Range(RangeSubtype),
    Multirange(RangeSubtype),
    Vector,
    Tensor,
    Record,
    Enum(crate::ast::EnumTypeReference),
    Composite(crate::ast::CompositeTypeReference),
}

#[derive(Debug, Clone, Copy)]
enum TypeClass {
    Boolean,
    Integer,
    Numeric,
    Text,
    Bytea,
    Array,
    Json,
    JsonB,
    Temporal,
}

pub(in crate::schema) fn infer_generation_expression(
    engine: &dyn SchemaExpressionCatalog,
    columns: &[ColumnDef],
    expression: &mut Expr,
) -> Result<(GenerationType, Vec<GeneratedFunctionDependency>), SQLError> {
    let mut dependencies = Vec::new();
    bind_function_calls(engine, columns, expression, &mut dependencies)?;
    let ty = infer_expression(engine, columns, expression, &mut dependencies)?;
    dependencies.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.argument_types.cmp(&right.argument_types))
    });
    dependencies.dedup();
    Ok((ty, dependencies))
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves generated coercion diagnostics"
)]
fn bind_function_calls(
    engine: &dyn SchemaExpressionCatalog,
    columns: &[ColumnDef],
    expression: &mut Expr,
    dependencies: &mut Vec<GeneratedFunctionDependency>,
) -> Result<(), SQLError> {
    match expression {
        Expr::Func {
            name,
            binding,
            args,
            filter,
            order_by,
            ..
        } => {
            for argument in args.iter_mut() {
                bind_function_calls(engine, columns, argument, dependencies)?;
            }
            for order in order_by {
                bind_function_calls(engine, columns, &mut order.expr, dependencies)?;
            }
            if let Some(filter) = filter {
                bind_function_calls(engine, columns, filter, dependencies)?;
            }
            if let Some(binding) = binding.as_mut() {
                if let Some(FunctionDispatch::NumericOperator(operator)) = binding.dispatch {
                    let types = args
                        .iter()
                        .map(|arg| {
                            let inferred = infer_expression(engine, columns, arg, dependencies)?;
                            Ok(generation_expression_column_type(columns, arg, &inferred))
                        })
                        .collect::<Result<Vec<_>, SQLError>>()?;
                    let selected =
                        crate::type_resolution::numeric_operator_types(operator, &types)?;
                    binding.argument_types = selected
                        .arguments
                        .iter()
                        .map(ColumnType::sql_name)
                        .collect();
                    return Ok(());
                }
            }
            if binding
                .as_ref()
                .and_then(|binding| binding.dispatch)
                .is_some()
            {
                return Ok(());
            }
            let call_arguments = generated_call_arguments(args)?;
            let explicit_variadic = call_arguments
                .iter()
                .any(|argument| argument.explicit_variadic);
            let mut argument_names = Vec::with_capacity(call_arguments.len());
            let mut argument_types = Vec::with_capacity(call_arguments.len());
            for argument in &call_arguments {
                argument_names.push(argument.name.clone());
                argument_types.push(infer_expression(
                    engine,
                    columns,
                    argument.value,
                    dependencies,
                )?);
            }
            if binding.is_none()
                && engine
                    .registered_runtime_function_volatility(name)
                    .is_some()
            {
                return Ok(());
            }
            if binding.is_none() {
                let declared_types = call_arguments
                    .iter()
                    .zip(&argument_types)
                    .map(|(argument, inferred)| {
                        enums::declared_type(columns, argument.value, inferred)
                    })
                    .collect::<Vec<_>>();
                if enums::enum_support_call(
                    engine,
                    name,
                    &argument_names,
                    &argument_types,
                    &declared_types,
                )?
                .is_some()
                {
                    return Ok(());
                }
            }
            if builtin::bind_fixed_builtin_call(
                builtin::FixedBuiltinCall {
                    engine,
                    columns,
                    name,
                    args,
                    argument_names: &argument_names,
                    argument_types: &argument_types,
                    explicit_variadic,
                },
                binding,
                dependencies,
            )? {
                return Ok(());
            }
            if binding
                .as_ref()
                .is_some_and(FunctionBinding::is_polymorphic_builtin_syntax)
            {
                return Ok(());
            }
            if binding.as_ref().is_some_and(|binding| !binding.builtin)
                || engine.lookup_visible_sql_functions(name)?.is_some()
            {
                let declared_argument_types = call_arguments
                    .iter()
                    .zip(&argument_types)
                    .map(|(argument, inferred)| {
                        Ok(generation_expression_column_type(
                            columns,
                            argument.value,
                            inferred,
                        ))
                    })
                    .collect::<Result<Vec<_>, SQLError>>()?;
                let selected = engine
                    .resolve_function_overload(
                        name,
                        binding.as_ref(),
                        &argument_names,
                        &declared_argument_types,
                        explicit_variadic,
                    )?
                    .ok_or_else(|| {
                        crate::type_resolution::function_resolution_error(
                            "42883",
                            name,
                            &argument_names,
                            &declared_argument_types,
                            "does not exist",
                        )
                    })?;
                let selected = validate_bound_function(
                    engine,
                    &selected.binding,
                    &argument_names,
                    &argument_types,
                )?;
                dependencies.push(selected.clone());
                *binding = Some(selected);
            }
            Ok(())
        }
        Expr::Array(items) | Expr::Row(items) | Expr::And(items) | Expr::Or(items) => {
            for item in items {
                bind_function_calls(engine, columns, item, dependencies)?;
            }
            Ok(())
        }
        Expr::Binary { lhs, rhs, .. } => {
            bind_function_calls(engine, columns, lhs, dependencies)?;
            bind_function_calls(engine, columns, rhs, dependencies)
        }
        Expr::Not(inner)
        | Expr::UnaryMinus(inner)
        | Expr::IsNull { expr: inner, .. }
        | Expr::Cast { expr: inner, .. } => {
            bind_function_calls(engine, columns, inner, dependencies)
        }
        Expr::Between { expr, low, high } => {
            bind_function_calls(engine, columns, expr, dependencies)?;
            bind_function_calls(engine, columns, low, dependencies)?;
            bind_function_calls(engine, columns, high, dependencies)
        }
        Expr::InList { expr, list, .. } => {
            bind_function_calls(engine, columns, expr, dependencies)?;
            for item in list {
                bind_function_calls(engine, columns, item, dependencies)?;
            }
            Ok(())
        }
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base {
                bind_function_calls(engine, columns, base, dependencies)?;
            }
            for (condition, result) in when {
                bind_function_calls(engine, columns, condition, dependencies)?;
                bind_function_calls(engine, columns, result, dependencies)?;
            }
            if let Some(else_branch) = else_branch {
                bind_function_calls(engine, columns, else_branch, dependencies)?;
            }
            Ok(())
        }
        Expr::Default
        | Expr::Param(_)
        | Expr::Star
        | Expr::QualifiedStar(_)
        | Expr::Column(_)
        | Expr::QualifiedColumn { .. }
        | Expr::InternalColumn(_)
        | Expr::Literal(_)
        | Expr::TypedLiteral { .. }
        | Expr::WindowCall { .. }
        | Expr::ScalarSubquery(_)
        | Expr::Exists { .. }
        | Expr::InSubquery { .. } => Ok(()),
    }
}

pub(in crate::schema) fn column_generation_type(ty: &ColumnType) -> GenerationType {
    match ty {
        ColumnType::Named(name) => {
            unreachable!("unresolved declaration type {name} reached catalog projection")
        }
        ColumnType::Enum(reference) => GenerationType::Enum(reference.clone()),
        ColumnType::Composite(reference) => GenerationType::Composite(reference.clone()),
        ColumnType::SmallInteger => GenerationType::SmallInteger,
        ColumnType::Integer => GenerationType::Integer,
        ColumnType::BigInteger => GenerationType::BigInteger,
        ColumnType::Oid => GenerationType::Oid,
        ColumnType::Xid => GenerationType::Xid,
        ColumnType::Boolean => GenerationType::Boolean,
        ColumnType::Void => GenerationType::Void,
        ColumnType::Text
        | ColumnType::RefCursor
        | ColumnType::Name
        | ColumnType::Varchar(_)
        | ColumnType::Bpchar
        | ColumnType::Character(_)
        | ColumnType::InternalChar
        | ColumnType::Regproc
        | ColumnType::Regprocedure
        | ColumnType::Regclass
        | ColumnType::Regnamespace
        | ColumnType::Regrole
        | ColumnType::Regtype
        | ColumnType::PgNodeTree
        | ColumnType::AclItem => GenerationType::Text,
        ColumnType::Uuid => GenerationType::Uuid,
        ColumnType::Real | ColumnType::DoublePrecision => GenerationType::Real,
        ColumnType::Numeric { .. } => GenerationType::Numeric,
        ColumnType::Json => GenerationType::Json,
        ColumnType::JsonB => GenerationType::JsonB,
        ColumnType::Bytea => GenerationType::Bytea,
        ColumnType::Int2Vector => GenerationType::Array(Box::new(GenerationType::Integer)),
        ColumnType::OidVector => GenerationType::Array(Box::new(GenerationType::Integer)),
        ColumnType::AnyArray => {
            GenerationType::Array(Box::new(GenerationType::UnknownLiteral("unknown".into())))
        }
        ColumnType::Record => GenerationType::Record,
        ColumnType::Array(element) => {
            GenerationType::Array(Box::new(column_generation_type(element)))
        }
        ColumnType::Date => GenerationType::Date,
        ColumnType::Time | ColumnType::TimePrecision(_) => GenerationType::Time,
        ColumnType::TimeTz | ColumnType::TimeTzPrecision(_) => GenerationType::TimeTz,
        ColumnType::Timestamp | ColumnType::TimestampPrecision(_) => GenerationType::Timestamp,
        ColumnType::TimestampTz | ColumnType::TimestampTzPrecision(_) => {
            GenerationType::TimestampTz
        }
        ColumnType::Interval | ColumnType::IntervalWithFields { .. } => GenerationType::Interval,
        ColumnType::Range(subtype) => GenerationType::Range(*subtype),
        ColumnType::Multirange(subtype) => GenerationType::Multirange(*subtype),
        ColumnType::Vector(_) => GenerationType::Vector,
        ColumnType::Tensor(_) => GenerationType::Tensor,
        ColumnType::Domain { base, .. } => column_generation_type(base),
    }
}

pub(in crate::schema) fn generation_type_assignable_to(
    source: &GenerationType,
    target: &ColumnType,
) -> bool {
    let target = column_generation_type(target);
    assignment_compatible(source, &target)
}

pub(in crate::schema) fn generation_type_name(ty: &GenerationType) -> String {
    match ty {
        GenerationType::Null | GenerationType::UnknownLiteral(_) => "unknown".into(),
        GenerationType::Boolean => "boolean".into(),
        GenerationType::Void => "void".into(),
        GenerationType::SmallInteger => "smallint".into(),
        GenerationType::Integer => "integer".into(),
        GenerationType::BigInteger => "bigint".into(),
        GenerationType::Oid => "oid".into(),
        GenerationType::Xid => "xid".into(),
        GenerationType::Real => "double precision".into(),
        GenerationType::Numeric => "numeric".into(),
        GenerationType::Text => "text".into(),
        GenerationType::Uuid => "uuid".into(),
        GenerationType::Bytea => "bytea".into(),
        GenerationType::Json => "json".into(),
        GenerationType::JsonB => "jsonb".into(),
        GenerationType::Array(element) => format!("{}[]", generation_type_name(element)),
        GenerationType::Date => "date".into(),
        GenerationType::Time => "time without time zone".into(),
        GenerationType::TimeTz => "time with time zone".into(),
        GenerationType::Timestamp => "timestamp without time zone".into(),
        GenerationType::TimestampTz => "timestamp with time zone".into(),
        GenerationType::Interval => "interval".into(),
        GenerationType::Range(subtype) => subtype.range_name().into(),
        GenerationType::Multirange(subtype) => subtype.multirange_name().into(),
        GenerationType::Vector => "vector".into(),
        GenerationType::Tensor => "tensor".into(),
        GenerationType::Record => "record".into(),
        GenerationType::Enum(reference) => ColumnType::Enum(reference.clone()).sql_name(),
        GenerationType::Composite(reference) => ColumnType::Composite(reference.clone()).sql_name(),
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves generated coercion diagnostics"
)]
fn infer_expression(
    engine: &dyn SchemaExpressionCatalog,
    columns: &[ColumnDef],
    expression: &Expr,
    dependencies: &mut Vec<GeneratedFunctionDependency>,
) -> Result<GenerationType, SQLError> {
    match expression {
        Expr::Column(name) | Expr::QualifiedColumn { column: name, .. } => columns
            .iter()
            .find(|column| column.name == *name)
            .map(|column| column_generation_type(&column.ty))
            .ok_or_else(|| SQLError::UnknownColumn(name.clone())),
        Expr::Literal(value) => value_generation_type(value),
        Expr::TypedLiteral { ty, .. } => crate::expr::EngineHook::resolve_type_name(engine, ty)
            .ok()
            .flatten()
            .map(|ty| column_generation_type(&ty))
            .ok_or_else(|| SQLError::Unsupported(format!("type {ty} does not exist"))),
        Expr::Array(items) => {
            let mut element = GenerationType::Null;
            for item in items {
                let item = infer_expression(engine, columns, item, dependencies)?;
                element = common_type(&element, &item)?;
            }
            Ok(GenerationType::Array(Box::new(finalize_common_type(
                element,
            ))))
        }
        Expr::Row(items) => {
            for item in items {
                infer_expression(engine, columns, item, dependencies)?;
            }
            Ok(GenerationType::Record)
        }
        Expr::Binary { op, lhs, rhs } => {
            let lhs = infer_expression(engine, columns, lhs, dependencies)?;
            let rhs = infer_expression(engine, columns, rhs, dependencies)?;
            let result = infer_binary_type(*op, &lhs, &rhs)?;
            if matches!(
                op,
                BinaryOp::Equal
                    | BinaryOp::NotEqual
                    | BinaryOp::Less
                    | BinaryOp::LessEqual
                    | BinaryOp::Greater
                    | BinaryOp::GreaterEqual
            ) {
                enums::validate_unknown_against(engine, &lhs, &rhs)?;
                enums::validate_unknown_against(engine, &rhs, &lhs)?;
            }
            Ok(result)
        }
        Expr::Not(inner) => {
            let ty = infer_expression(engine, columns, inner, dependencies)?;
            require_class("NOT", std::slice::from_ref(&ty), TypeClass::Boolean)?;
            Ok(GenerationType::Boolean)
        }
        Expr::UnaryMinus(inner) => {
            let ty = infer_expression(engine, columns, inner, dependencies)?;
            match ty {
                GenerationType::SmallInteger
                | GenerationType::Integer
                | GenerationType::BigInteger
                | GenerationType::Real
                | GenerationType::Numeric
                | GenerationType::Interval => Ok(ty),
                GenerationType::Oid | GenerationType::Xid => Ok(GenerationType::Integer),
                _ => Err(crate::type_resolution::undefined_prefix_operator(
                    "-",
                    &generation_type_name(&ty),
                )),
            }
        }
        Expr::And(items) | Expr::Or(items) => {
            let types = items
                .iter()
                .map(|item| infer_expression(engine, columns, item, dependencies))
                .collect::<Result<Vec<_>, _>>()?;
            require_class("boolean expression", &types, TypeClass::Boolean)?;
            Ok(GenerationType::Boolean)
        }
        Expr::IsNull { expr, .. } => {
            infer_expression(engine, columns, expr, dependencies)?;
            Ok(GenerationType::Boolean)
        }
        Expr::Between { expr, low, high } => {
            let value = infer_expression(engine, columns, expr, dependencies)?;
            let low = infer_expression(engine, columns, low, dependencies)?;
            let high = infer_expression(engine, columns, high, dependencies)?;
            for (left, right) in [(&value, &low), (&value, &high)] {
                common_type(left, right)?;
                enums::validate_unknown_against(engine, left, right)?;
                enums::validate_unknown_against(engine, right, left)?;
            }
            Ok(GenerationType::Boolean)
        }
        Expr::InList { expr, list, .. } => {
            let value = infer_expression(engine, columns, expr, dependencies)?;
            let mut common = value.clone();
            let mut items = Vec::with_capacity(list.len());
            for item in list {
                let item = infer_expression(engine, columns, item, dependencies)?;
                common = common_type(&common, &item)?;
                items.push(item);
            }
            for item in std::iter::once(&value).chain(&items) {
                enums::validate_unknown_against(engine, item, &common)?;
            }
            Ok(GenerationType::Boolean)
        }
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            let base_type = base
                .as_deref()
                .map(|base| infer_expression(engine, columns, base, dependencies))
                .transpose()?;
            let mut result_type = GenerationType::Null;
            for (condition, result) in when {
                let condition = infer_expression(engine, columns, condition, dependencies)?;
                if let Some(base_type) = base_type.as_ref() {
                    common_type(base_type, &condition)?;
                } else {
                    require_class(
                        "CASE condition",
                        std::slice::from_ref(&condition),
                        TypeClass::Boolean,
                    )?;
                }
                let result = infer_expression(engine, columns, result, dependencies)?;
                result_type = common_type(&result_type, &result)?;
            }
            if let Some(else_branch) = else_branch {
                let else_type = infer_expression(engine, columns, else_branch, dependencies)?;
                result_type = common_type(&result_type, &else_type)?;
            }
            Ok(finalize_common_type(result_type))
        }
        Expr::Cast { expr, ty } => {
            let source = infer_expression(engine, columns, expr, dependencies)?;
            let target = crate::expr::EngineHook::resolve_type_name(engine, ty)
                .ok()
                .flatten()
                .or_else(|| ColumnType::from_sql_name(ty).ok());
            if let Some(target) = target.as_ref() {
                enums::validate_cast_volatility(
                    engine,
                    &source,
                    enums::declared_type(columns, expr, &source).as_ref(),
                    target,
                )?;
            }
            target
                .as_ref()
                .map(column_generation_type)
                .or_else(|| generation_type_from_name(ty))
                .ok_or_else(|| {
                    SQLError::TypeMismatch(format!(
                        "generation expression cast uses unsupported type `{ty}`"
                    ))
                })
        }
        Expr::Func {
            name,
            binding,
            args,
            ..
        } => infer_function(engine, columns, name, binding.as_ref(), args, dependencies),
        Expr::Default
        | Expr::Param(_)
        | Expr::Star
        | Expr::QualifiedStar(_)
        | Expr::InternalColumn(_)
        | Expr::WindowCall { .. }
        | Expr::ScalarSubquery(_)
        | Expr::Exists { .. }
        | Expr::InSubquery { .. } => Err(SQLError::TypeMismatch(
            "unsupported expression shape in generated-column type analysis".into(),
        )),
    }
}

/// `(expression).field` in a generation expression: a row constructor's `fN` field or a composite value's attribute.
fn field_generation_type(
    engine: &dyn SchemaExpressionCatalog,
    columns: &[ColumnDef],
    args: &[Expr],
    argument_types: &[GenerationType],
    dependencies: &mut Vec<GeneratedFunctionDependency>,
) -> Result<GenerationType, SQLError> {
    use crate::type_resolution::field_selection;
    let ([base, Expr::Literal(Value::Str(field))], [base_type, _]) = (args, argument_types) else {
        return Err(SQLError::Internal(
            "field selection takes an expression and a field name".into(),
        ));
    };
    let declared = |ty: &GenerationType| {
        if type_rules::is_unknown(ty) {
            None
        } else {
            match ty {
                GenerationType::Composite(reference) => {
                    Some(ColumnType::Composite(reference.clone()))
                }
                GenerationType::Record => Some(ColumnType::Record),
                other => ColumnType::from_sql_name(&generation_type_name(other)).ok(),
            }
        }
    };
    let field_type = if let Expr::Row(items) = base {
        let items = items
            .iter()
            .map(|item| {
                infer_expression(engine, columns, item, dependencies).map(|ty| declared(&ty))
            })
            .collect::<Result<Vec<_>, _>>()?;
        field_selection::row_field_type(&items, field)?
    } else {
        let catalog = CompositeFieldResolver(engine);
        field_selection::value_field_type(declared(base_type).as_ref(), field, Some(&catalog))?
    };
    Ok(field_type.map_or(GenerationType::Null, |ty| column_generation_type(&ty)))
}

/// Composite attribute lookup for generation expressions, which bind through the schema catalog rather than a routine resolver.
struct CompositeFieldResolver<'a>(&'a dyn SchemaExpressionCatalog);

impl crate::type_resolution::FunctionTypeResolver for CompositeFieldResolver<'_> {
    fn composite_types(&self) -> Option<&dyn crate::expr::composites::CompositeTypeCatalog> {
        crate::expr::EngineHook::composite_types(self.0)
    }

    fn resolve_function_type(
        &self,
        _name: &str,
        _binding: Option<&crate::ast::FunctionBinding>,
        _argument_names: &[Option<String>],
        _argument_types: &[Option<ColumnType>],
        _explicit_variadic: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        Ok(None)
    }
}

fn infer_function(
    engine: &dyn SchemaExpressionCatalog,
    columns: &[ColumnDef],
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[Expr],
    dependencies: &mut Vec<GeneratedFunctionDependency>,
) -> Result<GenerationType, SQLError> {
    let call_arguments = generated_call_arguments(args)?;
    let mut argument_names = Vec::with_capacity(call_arguments.len());
    let mut argument_types = Vec::with_capacity(call_arguments.len());
    for argument in call_arguments {
        argument_names.push(argument.name);
        argument_types.push(infer_expression(
            engine,
            columns,
            argument.value,
            dependencies,
        )?);
    }

    if let Some(binding) = binding {
        if binding.builtin {
            if binding.dispatch == Some(crate::ast::FunctionDispatch::FieldSelect) {
                return field_generation_type(engine, columns, args, &argument_types, dependencies);
            }
            if let Some(dispatch) = binding.dispatch {
                if let Some(return_type) = infer_dispatched_function(dispatch, &argument_types)? {
                    return Ok(return_type);
                }
            }
            if let Some(return_type) = crate::type_resolution::fixed_builtin_return_type(binding) {
                return Ok(column_generation_type(&return_type));
            }
            let dispatch_name = builtin_function_dispatch_name(&binding.name);
            return infer_builtin_function(&dispatch_name, &argument_names, &argument_types)?
                .ok_or_else(|| SQLError::UnknownFunction(binding.name.clone()));
        }
        return bound_routine_return_type(engine, name, binding);
    }

    if engine
        .registered_runtime_function_volatility(name)
        .is_some()
    {
        return Err(SQLError::TypeMismatch(format!(
            "registered function `{name}` has no declared SQL return type and cannot be used in a column generation expression"
        )));
    }

    let declared_types = call_arguments_declared_types(columns, args, &argument_types)?;
    if let Some((operation, reference)) =
        enums::enum_call_type(name, &argument_names, &declared_types)
    {
        return dispatched::enum_function_type(
            operation.label(),
            operation,
            &[GenerationType::Enum(reference)],
        );
    }

    let dispatch_name = builtin_function_dispatch_name(&name.to_ascii_lowercase());
    infer_builtin_function(&dispatch_name, &argument_names, &argument_types)?
        .ok_or_else(|| SQLError::UnknownFunction(name.to_string()))
}

/// Return type of a call bound to a user SQL routine: its invocation's resolved type, else the routine's declared result.
fn bound_routine_return_type(
    engine: &dyn SchemaExpressionCatalog,
    name: &str,
    binding: &FunctionBinding,
) -> Result<GenerationType, SQLError> {
    let function = engine
        .lookup_bound_sql_functions_by_binding(binding)
        .and_then(|overloads| {
            overloads
                .into_iter()
                .find(|function| routine_signature_types(&function.def) == binding.argument_types)
        })
        .ok_or_else(|| SQLError::UnknownFunction(binding.name.clone()))?;
    let unsupported = |type_name: &str| {
        SQLError::TypeMismatch(format!(
            "generated-column function `{name}` returns unsupported type `{type_name}`"
        ))
    };
    if let Some(type_name) = binding
        .invocation
        .as_deref()
        .and_then(|invocation| invocation.return_type.as_deref())
    {
        return crate::expr::EngineHook::resolve_type_name(engine, type_name)
            .ok()
            .flatten()
            .as_ref()
            .map(column_generation_type)
            .or_else(|| generation_type_from_name(type_name))
            .ok_or_else(|| unsupported(type_name));
    }
    match &function.def.returns {
        FunctionReturns::Scalar { type_name } => {
            generation_type_from_name(type_name).ok_or_else(|| unsupported(type_name))
        }
        FunctionReturns::None => {
            let outputs = function.def.output_params();
            if outputs.len() > 1 {
                return Ok(GenerationType::Record);
            }
            let output = outputs.first().ok_or_else(|| {
                SQLError::TypeMismatch(format!(
                    "generated-column function `{name}` does not return a value"
                ))
            })?;
            generation_type_from_name(&output.type_name)
                .ok_or_else(|| unsupported(&output.type_name))
        }
        FunctionReturns::SetOf { .. } | FunctionReturns::Table => Err(SQLError::Internal(format!(
            "generated-column function `{name}` returns a set"
        ))),
    }
}

fn call_arguments_declared_types(
    columns: &[ColumnDef],
    args: &[Expr],
    argument_types: &[GenerationType],
) -> Result<Vec<Option<ColumnType>>, SQLError> {
    Ok(generated_call_arguments(args)?
        .iter()
        .zip(argument_types)
        .map(|(argument, inferred)| enums::declared_type(columns, argument.value, inferred))
        .collect())
}

pub(in crate::schema) fn generation_expression_column_type(
    columns: &[ColumnDef],
    expression: &Expr,
    inferred: &GenerationType,
) -> Option<ColumnType> {
    match expression {
        Expr::Column(name) | Expr::QualifiedColumn { column: name, .. } => columns
            .iter()
            .find(|column| column.name == *name)
            .map(|column| column.ty.clone()),
        Expr::Cast { ty, .. } | Expr::TypedLiteral { ty, .. } => ColumnType::from_sql_name(ty).ok(),
        Expr::Literal(Value::Str(_) | Value::Null) => None,
        _ => ColumnType::from_sql_name(&generation_type_name(inferred)).ok(),
    }
}

pub(in crate::schema) fn validate_bound_function(
    engine: &dyn SchemaExpressionCatalog,
    binding: &FunctionBinding,
    argument_names: &[Option<String>],
    argument_types: &[GenerationType],
) -> Result<FunctionBinding, SQLError> {
    let function = engine
        .lookup_bound_sql_functions_by_binding(binding)
        .and_then(|overloads| {
            overloads
                .into_iter()
                .find(|function| routine_signature_types(&function.def) == binding.argument_types)
        })
        .ok_or_else(|| SQLError::UnknownFunction(binding.name.clone()))?;
    if function.def.is_procedure || function.def.returns_set() {
        return Err(SQLError::TypeMismatch(format!(
            "generated-column function `{}` must return one scalar value",
            binding.name
        )));
    }
    if function.def.volatility != crate::ast::FunctionVolatility::Immutable {
        return Err(non_immutable_function());
    }
    let signature = function.def.signature_params();
    let mut positional = 0usize;
    for (argument_name, argument_type) in argument_names.iter().zip(argument_types) {
        let position = argument_name.as_ref().map_or_else(
            || {
                let position = positional;
                positional += 1;
                position
            },
            |argument_name| {
                signature
                    .iter()
                    .position(|parameter| parameter.name == *argument_name)
                    .unwrap_or(signature.len())
            },
        );
        let parameter = signature.get(position).ok_or_else(|| {
            SQLError::Internal(format!(
                "resolved generated-column function `{}` lost its argument mapping",
                binding.name
            ))
        })?;
        validate_unknown_literal_cast(argument_type, &parameter.type_name)?;
    }
    Ok(binding.clone())
}

mod arguments;
pub(in crate::schema) use arguments::generated_call_arguments;
mod builtin;
use builtin::infer_builtin_function;
mod dispatched;
mod enums;
use dispatched::infer_dispatched_function;
mod type_rules;
use type_rules::{
    accepts_class, assignment_compatible, common_numeric_type, common_type, common_types,
    concat_result_type, finalize_common_type, function_type_error, generation_type_from_name,
    infer_binary_type, non_immutable_function, numeric_input_type, require_arity, require_class,
    require_one, require_signature, validate_unknown_literal_cast, value_generation_type,
};
