//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Observe the same selected operand as a bound type or its explicit SQL spelling.

use super::{array_source_name, base_type, inferred_type, resolve_operand_type, ArraySource};
use crate::{ast::ColumnType, schema::ScalarTypeSchema, SQLError, SQLParam, ScalarExpr};
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};

enum Operand<'a> {
    Bound(&'a ColumnType),
    Spelling(&'a str),
    OwnedName(Produced<String>),
}

impl Operand<'_> {
    fn name(self, control: &ProductionControl<'_>) -> Result<Produced<String>, SQLError> {
        Ok(match self {
            Self::Bound(ty) => ty.catalog_name_with_control(control)?,
            Self::Spelling(name) => control.copy_text(name)?,
            Self::OwnedName(name) => name,
        })
    }

    fn is_real(self, control: &ProductionControl<'_>) -> Result<bool, SQLError> {
        let name = match &self {
            Self::Bound(ColumnType::Named(name)) => name.as_str(),
            Self::OwnedName(name) => name.as_str(),
            Self::Bound(ty) => return Ok(matches!(ty, ColumnType::Real)),
            Self::Spelling(name) => name,
        };
        match ColumnType::from_sql_name_with_control(name, control) {
            Ok(ty) => Ok(matches!(*ty, ColumnType::Real)),
            Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => Err(error),
            Err(_) => Ok(false),
        }
    }

    fn negation_source(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<ScalarNegationSource, SQLError> {
        if let Self::Bound(
            ty @ (ColumnType::SmallInteger
            | ColumnType::Integer
            | ColumnType::BigInteger
            | ColumnType::Real
            | ColumnType::DoublePrecision
            | ColumnType::Numeric { .. }
            | ColumnType::Interval),
        ) = &self
        {
            return Ok(ScalarNegationSource(NegationType::Builtin(
                crate::catalog::type_metadata::builtin_type_name(ty)
                    .expect("arithmetic builtin type"),
            )));
        }
        self.name(control)
            .map(|name| ScalarNegationSource(NegationType::Spelling(name)))
    }
}

/// The declared operand of prefix minus. Selected numeric and interval types carry the catalog's static name.
#[derive(Debug)]
pub struct ScalarNegationSource(NegationType);

#[derive(Debug)]
enum NegationType {
    Builtin(&'static str),
    Spelling(Produced<String>),
}

impl ScalarNegationSource {
    pub fn negate_with_control(
        source: Option<&Self>,
        value: &Value,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Value>, SQLError> {
        match source.map(|source| &source.0) {
            Some(NegationType::Builtin(name)) => {
                crate::expr::negate_canonical_value(value, name, control)
            }
            Some(NegationType::Spelling(name)) => {
                crate::expr::negate_value_with_control(value, Some(name), control)
            }
            None => crate::expr::negate_value_with_control(value, None, control),
        }
    }
}

pub fn scalar_negation_source_with_control(
    expression: &ScalarExpr,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    control: &ProductionControl<'_>,
) -> Result<Option<ScalarNegationSource>, SQLError> {
    read_operand(
        expression,
        schema,
        params,
        None,
        control,
        false,
        |operand, control| operand.negation_source(control),
    )
}

pub fn scalar_operand_is_real_with_control(
    expression: &ScalarExpr,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    Ok(read_operand(
        expression,
        schema,
        params,
        None,
        control,
        false,
        |operand, control| operand.is_real(control),
    )?
    .unwrap_or(false))
}

pub(super) fn operand_type_name(
    expression: &ScalarExpr,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    engine: Option<&dyn crate::expr::EngineHook>,
    control: &ProductionControl<'_>,
    preserve_domain: bool,
) -> Result<Option<Produced<String>>, SQLError> {
    read_operand(
        expression,
        schema,
        params,
        engine,
        control,
        preserve_domain,
        |operand, control| operand.name(control),
    )
}

fn read_operand<T>(
    expression: &ScalarExpr,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    engine: Option<&dyn crate::expr::EngineHook>,
    control: &ProductionControl<'_>,
    preserve_domain: bool,
    read: fn(Operand<'_>, &ProductionControl<'_>) -> Result<T, SQLError>,
) -> Result<Option<T>, SQLError> {
    control.check()?;
    let bound = |ty: &ColumnType| {
        let ty = if preserve_domain {
            ty
        } else {
            base_type(ty, control)?
        };
        read(Operand::Bound(ty), control)
    };
    let name = match expression {
        ScalarExpr::Cast { expr, ty, .. } => {
            if let Some(target) = resolve_operand_type(ty, engine, control)? {
                if matches!(*target, ColumnType::Record) {
                    let source_name =
                        operand_type_name(expr, schema, params, engine, control, true)?;
                    let source = match inferred_type(expr, schema, params, control)? {
                        Some(source) => Some(source),
                        None => source_name
                            .as_deref()
                            .map(|name| resolve_operand_type(name, engine, control))
                            .transpose()?
                            .flatten(),
                    };
                    if source.as_deref().is_some_and(|source| {
                        crate::type_resolution::cast_compatibility::preserves_record_identity(
                            source, &target,
                        )
                    }) {
                        if preserve_domain {
                            return source_name
                                .map(|name| read(Operand::OwnedName(name), control))
                                .transpose();
                        }
                        return source.as_deref().map(bound).transpose();
                    }
                }
            }
            Some(ty.as_str())
        }
        ScalarExpr::TypedLiteral {
            bound_type: Some(ty),
            ..
        }
        | ScalarExpr::Collated { ty, .. } => {
            return bound(ty).map(Some);
        }
        ScalarExpr::Func {
            binding: Some(binding),
            ..
        } if binding
            .invocation
            .as_ref()
            .is_some_and(|invocation| invocation.return_type.is_some()) =>
        {
            binding
                .invocation
                .as_ref()
                .and_then(|invocation| invocation.return_type.as_deref())
        }
        ScalarExpr::TypedLiteral { ty, .. }
        | ScalarExpr::CompositeRow {
            binding: crate::ast::CompositeRowBinding { ty, .. },
            ..
        } => Some(ty.as_str()),
        ScalarExpr::UnaryMinus(inner) => {
            return read_operand(inner, schema, params, None, control, false, read);
        }
        ScalarExpr::Array(items) => {
            match array_source_name(items, schema, params, engine, control)? {
                ArraySource::Known(name) => {
                    return read(Operand::OwnedName(name), control).map(Some)
                }
                ArraySource::Unknown => return Ok(None),
                ArraySource::Infer => {}
            }
            return inferred_type(expression, schema, params, control)?
                .as_deref()
                .map(bound)
                .transpose();
        }
        ScalarExpr::Literal(Value::Int(value)) if i32::try_from(*value).is_ok() => {
            return bound(&ColumnType::Integer).map(Some);
        }
        ScalarExpr::Literal(Value::Int(_)) => return bound(&ColumnType::BigInteger).map(Some),
        ScalarExpr::Literal(Value::Bytes(_)) => return bound(&ColumnType::Bytea).map(Some),
        ScalarExpr::Literal(Value::BitString(_)) => return bound(&ColumnType::Bit(None)).map(Some),
        ScalarExpr::Literal(Value::Str(_) | Value::FixedChar(_)) => return Ok(None),
        _ => {
            let Some(ty) = inferred_type(expression, schema, params, control)? else {
                return Ok(None);
            };
            return bound(&ty).map(Some);
        }
    };
    name.map(|name| read(Operand::Spelling(name), control))
        .transpose()
}
