//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Preserve cast function identities and modifiers without running routines.

use super::{values, ColumnType, ExpressionContext, Field, Node, SQLError, TypedNode};
use crate::catalog::type_metadata::{pg_type_collation_oid, pg_type_modifier, pg_type_oid};
use crate::type_resolution::{cast_catalog_entry, explicit_type_compatible, CastMethod};
use uqa_core::Value;

impl ExpressionContext<'_> {
    pub(super) fn coerce(
        value: TypedNode,
        target: &ColumnType,
        format: i64,
    ) -> Result<TypedNode, SQLError> {
        if pg_type_oid(&value.ty) == pg_type_oid(target) {
            return Self::modify_type(value, target, format);
        }
        if let ColumnType::Domain { base, .. } = target {
            let value = Self::coerce(value, base, format)?;
            return Ok(TypedNode {
                node: Node::new(
                    "COERCETODOMAIN",
                    [
                        ("arg", value.node.into()),
                        ("resulttype", pg_type_oid(target).into()),
                        ("resulttypmod", (-1).into()),
                        ("resultcollid", pg_type_collation_oid(target).into()),
                        ("coercionformat", format.into()),
                        ("location", (-1).into()),
                    ],
                ),
                ty: target.clone(),
            });
        }
        let mut source = &value.ty;
        while let ColumnType::Domain { base, .. } = source {
            source = base;
        }
        let base_target = target.without_type_modifiers();
        let method = if pg_type_oid(source) == pg_type_oid(target) {
            CastMethod::Binary
        } else if let Some(entry) = cast_catalog_entry(source, target) {
            if format == 2 && entry.context != b'i' {
                return Err(coercion_error(&value.ty, target));
            }
            entry.method
        } else if format == 1
            && explicit_type_compatible(source, target)
            && (is_string(source) || is_string(target))
        {
            CastMethod::InputOutput
        } else {
            return Err(coercion_error(&value.ty, target));
        };
        let value = match method {
            CastMethod::Binary => TypedNode {
                node: Node::new(
                    "RELABELTYPE",
                    [
                        ("arg", value.node.into()),
                        ("resulttype", pg_type_oid(&base_target).into()),
                        ("resulttypmod", (-1).into()),
                        ("resultcollid", pg_type_collation_oid(&base_target).into()),
                        ("relabelformat", format.into()),
                        ("location", (-1).into()),
                    ],
                ),
                ty: base_target.clone(),
            },
            CastMethod::Function { oid, arguments } => {
                cast_function(value, &base_target, oid, arguments, format)?
            }
            CastMethod::InputOutput => TypedNode {
                node: Node::new(
                    "COERCEVIAIO",
                    [
                        ("arg", value.node.into()),
                        ("resulttype", pg_type_oid(&base_target).into()),
                        ("resultcollid", pg_type_collation_oid(&base_target).into()),
                        ("coerceformat", format.into()),
                        ("location", (-1).into()),
                    ],
                ),
                ty: base_target,
            },
        };
        Self::modify_type(value, target, format)
    }

    fn modify_type(
        value: TypedNode,
        target: &ColumnType,
        format: i64,
    ) -> Result<TypedNode, SQLError> {
        let modifier = pg_type_modifier(target);
        if modifier < 0 || modifier == pg_type_modifier(&value.ty) {
            return Ok(value);
        }
        if let Some(entry) = cast_catalog_entry(target, target) {
            if let CastMethod::Function { oid, arguments } = entry.method {
                return cast_function(value, target, oid, arguments, format);
            }
        }
        Err(coercion_error(&value.ty, target))
    }
}

fn cast_function(
    value: TypedNode,
    target: &ColumnType,
    oid: i64,
    arity: usize,
    format: i64,
) -> Result<TypedNode, SQLError> {
    if !(1..=3).contains(&arity) {
        return Err(SQLError::Internal("invalid catalog cast arity".into()));
    }
    let input_collation = pg_type_collation_oid(&value.ty);
    let mut arguments = vec![value.node.into()];
    if arity > 1 {
        arguments.push(
            values::constant(&Value::Int(pg_type_modifier(target)), &ColumnType::Integer)?.into(),
        );
    }
    if arity > 2 {
        arguments.push(values::constant(&Value::Bool(format == 1), &ColumnType::Boolean)?.into());
    }
    Ok(TypedNode {
        node: Node::new(
            "FUNCEXPR",
            [
                ("funcid", oid.into()),
                ("funcresulttype", pg_type_oid(target).into()),
                ("funcretset", false.into()),
                ("funcvariadic", false.into()),
                ("funcformat", format.into()),
                ("funccollid", pg_type_collation_oid(target).into()),
                ("inputcollid", input_collation.into()),
                ("args", Field::List(arguments)),
                ("location", (-1).into()),
            ],
        ),
        ty: target.clone(),
    })
}

fn is_string(ty: &ColumnType) -> bool {
    matches!(
        ty,
        ColumnType::Text
            | ColumnType::Varchar(_)
            | ColumnType::Name
            | ColumnType::Bpchar
            | ColumnType::Character(_)
    )
}

fn coercion_error(source: &ColumnType, target: &ColumnType) -> SQLError {
    SQLError::Routine {
        sqlstate: "42846".into(),
        message: format!(
            "cannot cast type {} to {}",
            source.sql_name(),
            target.sql_name()
        ),
    }
}
