//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed array reads retain container, element, result and endpoint identities.

use super::{
    ColumnType, Expr, ExpressionContext, Field, FunctionBinding, Node, SQLError, TypedNode,
};
use crate::ast::{ArrayAccess, MAX_ARRAY_SUBSCRIPT_DIMENSIONS};
use crate::catalog::type_metadata::{pg_type_collation_oid, pg_type_modifier, pg_type_oid};
use crate::type_resolution::{array_element_type, base_type};

impl ExpressionContext<'_> {
    pub(super) fn array_construct(
        &self,
        binding: Option<&FunctionBinding>,
        args: &[Expr],
    ) -> Result<Option<TypedNode>, SQLError> {
        binding
            .and_then(|binding| binding.dispatch)
            .and_then(|dispatch| {
                ArrayAccess::from_dispatch(dispatch, args.len(), |index| {
                    matches!(args[index], Expr::Literal(uqa_core::Value::Null))
                })
            })
            .map(|access| self.array_access(access?, args))
            .transpose()
    }

    fn array_access(&self, access: ArrayAccess, args: &[Expr]) -> Result<TypedNode, SQLError> {
        let source_type = self.expression_type(&args[0])?;
        let input = self.encode(&args[0], None)?;
        let base = base_type(&input.ty);
        let Some(element) = array_element_type(base) else {
            let name = match source_type.as_ref().map(base_type) {
                None => "unknown".into(),
                Some(ty) => match self.types {
                    Some(types) => types.format_type_name(ty)?,
                    None => ty.sql_name(),
                },
            };
            return Err(SQLError::Routine {
                sqlstate: "42804".into(),
                message: format!(
                    "cannot subscript type {name} because it does not support subscripting"
                ),
            });
        };
        let container = match base {
            ColumnType::Int2Vector | ColumnType::OidVector => {
                ColumnType::Array(Box::new(element.clone()))
            }
            _ => base.clone(),
        };
        let ty = if access.slice_bounds().is_some() {
            container.clone()
        } else {
            element.clone()
        };
        let element_oid = pg_type_oid(element);
        let mut bounds =
            Vec::with_capacity(access.bound_count().min(MAX_ARRAY_SUBSCRIPT_DIMENSIONS * 2));
        // Analysis visits written lower/upper positions, including beyond the rank limit.
        for (position, argument) in args.iter().enumerate().skip(1) {
            let bound = if access.is_omitted(position) {
                Field::Null
            } else {
                self.array_index(argument)?.node.into()
            };
            if bounds.len() < MAX_ARRAY_SUBSCRIPT_DIMENSIONS * 2 {
                bounds.push(bound);
            }
        }
        access.validate_dimensions()?;
        let (upper, lower) = if access.slice_bounds().is_some() {
            let mut upper = Vec::with_capacity(bounds.len() / 2);
            let mut lower = Vec::with_capacity(bounds.len() / 2);
            for (index, bound) in bounds.into_iter().enumerate() {
                if index % 2 == 0 {
                    lower.push(bound);
                } else {
                    upper.push(bound);
                }
            }
            (upper, Field::List(lower))
        } else {
            (bounds, Field::Null)
        };
        Ok(TypedNode {
            node: Node::new(
                "SUBSCRIPTINGREF",
                [
                    ("refcontainertype", pg_type_oid(&container).into()),
                    ("refelemtype", element_oid.into()),
                    ("refrestype", pg_type_oid(&ty).into()),
                    ("reftypmod", pg_type_modifier(&ty).into()),
                    ("refcollid", pg_type_collation_oid(&ty).into()),
                    ("refupperindexpr", Field::List(upper)),
                    ("reflowerindexpr", lower),
                    ("refexpr", input.node.into()),
                    ("refassgnexpr", Field::Null),
                ],
            ),
            ty,
        })
    }

    fn array_index(&self, mut argument: &Expr) -> Result<TypedNode, SQLError> {
        // Rebuild an analyzed endpoint's assignment cast in the same context.
        if let Expr::Cast {
            expr,
            ty,
            implicit: true,
        } = argument
        {
            if self.resolve_type(ty)? == ColumnType::Integer {
                argument = expr;
            }
        }
        let ty = self.expression_type(argument)?;
        if ty
            .as_ref()
            .is_some_and(|ty| !crate::assignment_type_compatible(ty, &ColumnType::Integer))
        {
            return Err(SQLError::Routine {
                sqlstate: "42804".into(),
                message: "array subscript must have type integer".into(),
            });
        }
        let value = self.encode(argument, ty.is_none().then_some(&ColumnType::Integer))?;
        Self::coerce_assignment(value, &ColumnType::Integer)
    }
}
