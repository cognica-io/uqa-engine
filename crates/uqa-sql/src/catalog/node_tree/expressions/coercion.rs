//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Preserve cast function identities and modifiers without running routines.

use super::{values, ColumnType, Expr, ExpressionContext, Field, Node, SQLError, TypedNode};
use crate::catalog::type_metadata::{pg_type_collation_oid, pg_type_modifier, pg_type_oid};
use crate::type_resolution::{cast_catalog_entry, explicit_type_compatible, CastMethod};
use uqa_core::Value;

#[derive(Clone, Copy)]
struct Coercion {
    format: i64,
    explicit: bool,
    assignment: bool,
}

impl ExpressionContext<'_> {
    pub(super) fn cast_expression(
        &self,
        expression: &Expr,
        ty: &str,
        implicit: bool,
    ) -> Result<TypedNode, SQLError> {
        let ty = self.resolve_type(ty)?;
        let format = if implicit { 2 } else { 1 };
        if let Some(row) = self.row_cast(expression, &ty, format)? {
            return Ok(row);
        }
        let unknown = self.expression_type(expression)?.is_none();
        let mut input_type = &ty;
        while let ColumnType::Domain { base, .. } = input_type {
            input_type = base;
        }
        let input_type = input_type.without_type_modifiers();
        let inner = self.encode(
            expression,
            (unknown && needs_input_context(expression)?).then_some(&input_type),
        )?;
        Self::coerce(inner, &ty, format)
    }

    pub(super) fn coerce(
        value: TypedNode,
        target: &ColumnType,
        format: i64,
    ) -> Result<TypedNode, SQLError> {
        Self::coerce_selected(
            value,
            target,
            Coercion {
                format,
                explicit: format == 1,
                assignment: false,
            },
        )
    }

    pub(super) fn coerce_row_field(
        value: TypedNode,
        target: &ColumnType,
    ) -> Result<TypedNode, SQLError> {
        Self::coerce_selected(
            value,
            target,
            Coercion {
                format: 2,
                explicit: true,
                assignment: false,
            },
        )
    }

    pub(super) fn coerce_assignment(
        value: TypedNode,
        target: &ColumnType,
    ) -> Result<TypedNode, SQLError> {
        Self::coerce_selected(
            value,
            target,
            Coercion {
                format: 2,
                explicit: false,
                assignment: true,
            },
        )
    }

    fn coerce_selected(
        value: TypedNode,
        target: &ColumnType,
        coercion: Coercion,
    ) -> Result<TypedNode, SQLError> {
        let format = coercion.format;
        let same_type = value
            .ty
            .as_ref()
            .is_some_and(|ty| pg_type_oid(ty) == pg_type_oid(target));
        if same_type && !coercion.explicit && pg_type_modifier(target) < 0 {
            return Ok(value);
        }
        if value.node.kind == "COLLATEEXPR" {
            let (node, collation) = super::collations::peel(value.node)?;
            let mut value =
                Self::coerce_selected(TypedNode { node, ty: value.ty }, target, coercion)?;
            if let Some(oid) =
                collation.filter(|_| crate::schema::composites::type_is_collatable(target))
            {
                value.node = super::collations::collate(value.node, oid);
            }
            return Ok(value);
        }
        if same_type {
            return Self::modify_type(value, target, coercion, false);
        }
        if let ColumnType::Domain { base, .. } = target {
            return Self::domain_coercion(value, target, base, coercion);
        }
        if value.ty.is_none() {
            if coercion.explicit && is_string(target) {
                let value = input_output_cast(value, target.without_type_modifiers(), format);
                return Self::modify_type(value, target, coercion, true);
            }
            return Err(crate::type_resolution::field_selection::unknown_conversion_error(target));
        }
        let mut source = value.type_ref()?;
        while let ColumnType::Domain { base, .. } = source {
            source = base;
        }
        if *target == ColumnType::Record && matches!(source, ColumnType::Composite(_)) {
            return Ok(value);
        }
        if matches!(
            (source, target),
            (ColumnType::Array(_), ColumnType::Array(_))
        ) && pg_type_oid(source) != pg_type_oid(target)
        {
            return Self::array_coercion(value, target, coercion);
        }
        let base_target = target.without_type_modifiers();
        let method = if pg_type_oid(source) == pg_type_oid(target) {
            CastMethod::Binary
        } else if let Some(entry) = cast_catalog_entry(source, target) {
            if !coercion.explicit
                && entry.context != b'i'
                && !(coercion.assignment && entry.context == b'a')
            {
                return Err(coercion_error(value.type_ref()?, target));
            }
            entry.method
        } else if (coercion.explicit
            && explicit_type_compatible(source, target)
            && (is_string(source) || is_string(target)))
            || (coercion.assignment
                && is_string(target)
                && crate::type_resolution::assignment_type_compatible(source, target))
        {
            CastMethod::InputOutput
        } else {
            return Err(coercion_error(value.type_ref()?, target));
        };
        let value = match method {
            CastMethod::Binary => relabel(value, &base_target, format),
            CastMethod::Function { oid, arguments } => {
                cast_function(value, &base_target, oid, arguments, coercion)?
            }
            CastMethod::InputOutput => input_output_cast(value, base_target, format),
        };
        Self::modify_type(value, target, coercion, true)
    }

    fn domain_coercion(
        value: TypedNode,
        target: &ColumnType,
        base: &ColumnType,
        coercion: Coercion,
    ) -> Result<TypedNode, SQLError> {
        let format = coercion.format;
        // The domain node owns the written cast; its base conversion is hidden, while explicit input/typmod semantics remain unchanged.
        let base_coercion = Coercion {
            format: 2,
            ..coercion
        };
        let binary = value.ty.as_ref().is_some_and(|mut source| {
            while let ColumnType::Domain { base, .. } = source {
                source = base;
            }
            pg_type_oid(source) == pg_type_oid(base)
                || cast_catalog_entry(source, base).is_some_and(|entry| {
                    entry.method == CastMethod::Binary
                        && (coercion.explicit
                            || entry.context == b'i'
                            || (coercion.assignment && entry.context == b'a'))
                })
        });
        let value = if binary {
            // The enclosing domain already supplies the type identity for a binary-compatible input.
            Self::modify_type(value, base, base_coercion, false)?
        } else {
            Self::coerce_selected(value, base, base_coercion)?
        };
        Ok(TypedNode {
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
            ty: Some(target.clone()),
        })
    }

    fn array_coercion(
        value: TypedNode,
        target: &ColumnType,
        coercion: Coercion,
    ) -> Result<TypedNode, SQLError> {
        let format = coercion.format;
        let mut source = value.type_ref()?;
        while let ColumnType::Domain { base, .. } = source {
            source = base;
        }
        let (ColumnType::Array(source_element), ColumnType::Array(target_element)) =
            (source, target)
        else {
            return Err(coercion_error(value.type_ref()?, target));
        };
        let source_element = scalar_element(source_element);
        let target_element = scalar_element(target_element);
        let element = TypedNode {
            node: Node::new(
                "CASETESTEXPR",
                [
                    ("typeId", pg_type_oid(source_element).into()),
                    ("typeMod", pg_type_modifier(source_element).into()),
                    ("collation", 0.into()),
                ],
            ),
            ty: Some(source_element.clone()),
        };
        let element = Self::coerce_selected(element, target_element, coercion)?;
        let element_type = element
            .ty
            .ok_or_else(|| super::super::invalid("unknown coerced array element"))?;
        Ok(TypedNode {
            node: Node::new(
                "ARRAYCOERCEEXPR",
                [
                    ("arg", value.node.into()),
                    ("elemexpr", element.node.into()),
                    ("resulttype", pg_type_oid(target).into()),
                    ("resulttypmod", pg_type_modifier(&element_type).into()),
                    ("resultcollid", pg_type_collation_oid(target).into()),
                    ("coerceformat", format.into()),
                    ("location", (-1).into()),
                ],
            ),
            ty: Some(ColumnType::Array(Box::new(element_type))),
        })
    }

    fn modify_type(
        mut value: TypedNode,
        target: &ColumnType,
        coercion: Coercion,
        hide_input: bool,
    ) -> Result<TypedNode, SQLError> {
        let format = coercion.format;
        let modifier = pg_type_modifier(target);
        if modifier == pg_type_modifier(value.type_ref()?) {
            return Ok(value);
        }
        if hide_input {
            let field = match value.node.kind.as_str() {
                "FUNCEXPR" => "funcformat",
                "RELABELTYPE" => "relabelformat",
                "COERCEVIAIO" => "coerceformat",
                _ => return Err(SQLError::Internal("invalid inner type coercion".into())),
            };
            let (_, format) = value
                .node
                .fields
                .iter_mut()
                .find(|(name, _)| name == field)
                .ok_or_else(|| SQLError::Internal("missing coercion format".into()))?;
            *format = 2.into();
        }
        if modifier < 0 {
            return Ok(relabel(value, target, format));
        }
        if matches!(
            (value.type_ref()?, target),
            (ColumnType::Array(_), ColumnType::Array(_))
        ) {
            return Self::array_coercion(value, target, coercion);
        }
        if let Some(entry) = cast_catalog_entry(target, target) {
            if let CastMethod::Function { oid, arguments } = entry.method {
                return cast_function(value, target, oid, arguments, coercion);
            }
        }
        Err(coercion_error(value.type_ref()?, target))
    }
}

fn scalar_element(mut ty: &ColumnType) -> &ColumnType {
    while let ColumnType::Array(element) = ty {
        ty = element;
    }
    ty
}

fn relabel(value: TypedNode, target: &ColumnType, format: i64) -> TypedNode {
    TypedNode {
        node: Node::new(
            "RELABELTYPE",
            [
                ("arg", value.node.into()),
                ("resulttype", pg_type_oid(target).into()),
                ("resulttypmod", pg_type_modifier(target).into()),
                ("resultcollid", pg_type_collation_oid(target).into()),
                ("relabelformat", format.into()),
                ("location", (-1).into()),
            ],
        ),
        ty: Some(target.clone()),
    }
}

fn cast_function(
    value: TypedNode,
    target: &ColumnType,
    oid: i64,
    arity: usize,
    coercion: Coercion,
) -> Result<TypedNode, SQLError> {
    let format = coercion.format;
    if !(1..=3).contains(&arity) {
        return Err(SQLError::Internal("invalid catalog cast arity".into()));
    }
    let mut arguments = vec![value.node.into()];
    if arity > 1 {
        arguments.push(
            values::constant(&Value::Int(pg_type_modifier(target)), &ColumnType::Integer)?.into(),
        );
    }
    if arity > 2 {
        arguments
            .push(values::constant(&Value::Bool(coercion.explicit), &ColumnType::Boolean)?.into());
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
                ("inputcollid", 0.into()),
                ("args", Field::List(arguments)),
                ("location", (-1).into()),
            ],
        ),
        ty: Some(target.clone()),
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

fn needs_input_context(expression: &Expr) -> Result<bool, SQLError> {
    if let Some(clause) = crate::catalog::collations::expression::ast_clause(expression)? {
        return needs_input_context(clause.argument);
    }
    Ok(matches!(
        expression,
        Expr::Literal(Value::Null | Value::Str(_)) | Expr::Array(_)
    ))
}

fn input_output_cast(value: TypedNode, target: ColumnType, format: i64) -> TypedNode {
    TypedNode {
        node: Node::new(
            "COERCEVIAIO",
            [
                ("arg", value.node.into()),
                ("resulttype", pg_type_oid(&target).into()),
                ("resultcollid", pg_type_collation_oid(&target).into()),
                ("coerceformat", format.into()),
                ("location", (-1).into()),
            ],
        ),
        ty: Some(target),
    }
}
