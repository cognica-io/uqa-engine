//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar operator, array, and membership expression nodes.

use super::{
    operator_node, values, BinaryOp, ColumnType, Expr, ExpressionContext, Field, Node, SQLError,
    TypedNode,
};
use crate::catalog::type_metadata::{pg_type_collation_oid, pg_type_oid};
use crate::type_resolution::{
    binary_operator_catalog_entry, binary_operator_types, common_type, unary_minus_catalog_entry,
};

impl ExpressionContext<'_> {
    pub(super) fn numeric_operator(
        &self,
        operator: crate::ast::NumericOperator,
        arguments: &[Expr],
    ) -> Result<TypedNode, SQLError> {
        let types = arguments
            .iter()
            .map(|arg| self.expression_type(arg))
            .collect::<Result<Vec<_>, _>>()?;
        let selected = crate::type_resolution::numeric_operator_types(operator, &types)?;
        let arguments = arguments
            .iter()
            .zip(&selected.arguments)
            .map(|(arg, ty)| self.encode(arg, Some(ty)))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(operator_node(
            selected.oid,
            selected.function_oid,
            arguments,
            selected.result,
        ))
    }

    pub(super) fn unary_minus(&self, argument: &Expr) -> Result<TypedNode, SQLError> {
        let value = self.encode(argument, None)?;
        if let Expr::Literal(literal) = argument {
            let negated = crate::expr::negate_value(literal, Some(&value.ty.sql_name()))?;
            return Ok(TypedNode {
                node: values::constant(&negated, &value.ty)?,
                ty: value.ty,
            });
        }
        let operator = unary_minus_catalog_entry(&value.ty)?;
        let value = Self::coerce(value, &operator.operand_type, 2)?;
        Ok(operator_node(
            operator.oid,
            operator.function_oid,
            [value],
            operator.operand_type,
        ))
    }

    pub(super) fn array(
        &self,
        elements: &[Expr],
        expected: Option<&ColumnType>,
    ) -> Result<TypedNode, SQLError> {
        let ty = self
            .expression_type(&Expr::Array(elements.to_vec()))?
            .or_else(|| expected.cloned());
        let Some(ColumnType::Array(element)) = ty else {
            return Err(SQLError::TypeMismatch(
                "cannot determine type of empty array".into(),
            ));
        };
        self.array_typed(elements, &element)
    }

    fn array_typed(&self, elements: &[Expr], element: &ColumnType) -> Result<TypedNode, SQLError> {
        let ty = ColumnType::Array(Box::new(element.clone()));
        let mut leaf = element;
        while let ColumnType::Array(nested) = leaf {
            leaf = nested;
        }
        let elements = elements
            .iter()
            .map(|expression| {
                self.encode(expression, Some(element))
                    .map(|value| value.node.into())
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(TypedNode {
            node: Node::new(
                "ARRAYEXPR",
                [
                    ("array_typeid", pg_type_oid(&ty).into()),
                    ("array_collid", pg_type_collation_oid(&ty).into()),
                    ("element_typeid", pg_type_oid(leaf).into()),
                    ("elements", Field::List(elements)),
                    ("multidims", matches!(element, ColumnType::Array(_)).into()),
                    ("list_start", (-1).into()),
                    ("list_end", (-1).into()),
                    ("location", (-1).into()),
                ],
            ),
            ty,
        })
    }

    pub(super) fn scalar_array(
        &self,
        op: BinaryOp,
        lhs: &Expr,
        rhs: &Expr,
        use_or: bool,
    ) -> Result<TypedNode, SQLError> {
        let left = self.expression_type(lhs)?;
        let right = self.expression_type(rhs)?;
        let Some(ColumnType::Array(element)) = right else {
            return Err(SQLError::TypeMismatch(
                "op ANY/ALL (array) requires array on right side".into(),
            ));
        };
        let [left, element, _] = binary_operator_types(op, left.as_ref(), Some(&element))?;
        let lhs = self.encode(lhs, Some(&left))?;
        let rhs = self.encode(rhs, Some(&ColumnType::Array(Box::new(element.clone()))))?;
        scalar_array_node(op, lhs, rhs, &element, use_or)
    }

    pub(super) fn in_list(
        &self,
        lhs: &Expr,
        list: &[Expr],
        negated: bool,
    ) -> Result<TypedNode, SQLError> {
        let op = if negated {
            BinaryOp::NotEqual
        } else {
            BinaryOp::Equal
        };
        let (variables, plain): (Vec<_>, Vec<_>) = list.iter().partition(|expression| {
            self.domain_value.is_none()
                && expression.any_node(&|node| {
                    matches!(node, Expr::Column(_) | Expr::QualifiedColumn { .. })
                })
        });
        let mut conditions = Vec::new();
        let mut remaining = variables;
        if plain.len() > 1 {
            let mut element = self.expression_type(lhs)?;
            for expression in &plain {
                if let Some(ty) = self.expression_type(expression)? {
                    element = Some(match element {
                        Some(previous) => common_type(&previous, &ty)?,
                        None => ty,
                    });
                }
            }
            let element = element.unwrap_or(ColumnType::Text);
            let values = plain.into_iter().cloned().collect::<Vec<_>>();
            let array = self.array_typed(&values, &element)?;
            let [left, right, _] =
                binary_operator_types(op, self.expression_type(lhs)?.as_ref(), Some(&element))?;
            let lhs = self.encode(lhs, Some(&left))?;
            conditions.push(scalar_array_node(op, lhs, array, &right, !negated)?);
        } else {
            remaining.splice(0..0, plain);
        }
        for rhs in remaining {
            conditions.push(self.binary(op, lhs, rhs)?);
        }
        if conditions.len() == 1 {
            return Ok(conditions.remove(0));
        }
        if conditions.is_empty() {
            return Err(SQLError::Internal("empty IN expression".into()));
        }
        Ok(TypedNode {
            node: Node::new(
                "BOOLEXPR",
                [
                    (
                        "boolop",
                        Field::Atom(if negated { "and" } else { "or" }.into()),
                    ),
                    (
                        "args",
                        Field::List(
                            conditions
                                .into_iter()
                                .map(|value| value.node.into())
                                .collect(),
                        ),
                    ),
                    ("location", (-1).into()),
                ],
            ),
            ty: ColumnType::Boolean,
        })
    }

    pub(super) fn typed_binary(
        op: BinaryOp,
        lhs: TypedNode,
        rhs: TypedNode,
    ) -> Result<TypedNode, SQLError> {
        let [left, right, result] = binary_operator_types(op, Some(&lhs.ty), Some(&rhs.ty))?;
        let identity = binary_operator_catalog_entry(op, [&left, &right])?;
        let arguments = [Self::coerce(lhs, &left, 2)?, Self::coerce(rhs, &right, 2)?];
        Ok(operator_node(
            identity.oid,
            identity.function_oid,
            arguments,
            result,
        ))
    }
}

fn scalar_array_node(
    op: BinaryOp,
    lhs: TypedNode,
    rhs: TypedNode,
    element: &ColumnType,
    use_or: bool,
) -> Result<TypedNode, SQLError> {
    let operator = binary_operator_catalog_entry(op, [&lhs.ty, element])?;
    let collation = pg_type_collation_oid(&lhs.ty).max(pg_type_collation_oid(element));
    Ok(TypedNode {
        node: Node::new(
            "SCALARARRAYOPEXPR",
            [
                ("opno", operator.oid.into()),
                ("opfuncid", operator.function_oid.into()),
                ("hashfuncid", 0.into()),
                ("negfuncid", 0.into()),
                ("useOr", use_or.into()),
                ("inputcollid", collation.into()),
                ("args", Field::List(vec![lhs.node.into(), rhs.node.into()])),
                ("location", (-1).into()),
            ],
        ),
        ty: ColumnType::Boolean,
    })
}
