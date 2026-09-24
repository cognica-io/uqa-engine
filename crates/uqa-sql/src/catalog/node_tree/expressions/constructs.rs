//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Parser-owned conditional and quantified expressions retain their node kinds.

use super::{
    values, BinaryOp, ColumnType, Expr, ExpressionContext, Field, FunctionBinding, Node, SQLError,
    TypedNode,
};
use crate::ast::FunctionDispatch;
use crate::catalog::type_metadata::{pg_type_collation_oid, pg_type_modifier, pg_type_oid};
use uqa_core::Value;

impl ExpressionContext<'_> {
    pub(super) fn construct(
        &self,
        expression: &Expr,
        name: &str,
        binding: Option<&FunctionBinding>,
        args: &[Expr],
    ) -> Result<Option<TypedNode>, SQLError> {
        match binding.and_then(|binding| binding.dispatch) {
            Some(FunctionDispatch::NumericOperator(operator)) => {
                return self.numeric_operator(operator, args).map(Some);
            }
            Some(FunctionDispatch::AnyOperator | FunctionDispatch::AllOperator) => {
                let [lhs, rhs, Expr::Literal(Value::Str(operator))] = args else {
                    return Err(SQLError::Internal(
                        "invalid quantified operator arguments".into(),
                    ));
                };
                let op = match operator.as_str() {
                    "=" => BinaryOp::Equal,
                    "<>" | "!=" => BinaryOp::NotEqual,
                    "<" => BinaryOp::Less,
                    "<=" => BinaryOp::LessEqual,
                    ">" => BinaryOp::Greater,
                    ">=" => BinaryOp::GreaterEqual,
                    _ => {
                        return Err(SQLError::Unsupported(format!(
                            "quantified operator {operator}"
                        )))
                    }
                };
                return self
                    .scalar_array(
                        op,
                        lhs,
                        rhs,
                        binding.is_some_and(|binding| {
                            binding.dispatch == Some(FunctionDispatch::AnyOperator)
                        }),
                    )
                    .map(Some);
            }
            Some(FunctionDispatch::IsDistinct) => {
                let [lhs, rhs] = args else {
                    return Err(SQLError::Internal("invalid distinct operands".into()));
                };
                let mut value = self.binary(BinaryOp::Equal, lhs, rhs)?;
                value.node.kind = "DISTINCTEXPR".into();
                return Ok(Some(value));
            }
            Some(_) => return Ok(None),
            None => {}
        }
        if binding.is_some_and(|binding| binding.object_id.is_some()) {
            return Ok(None);
        }
        if !matches!(name, "coalesce" | "greatest" | "least") {
            return Ok(None);
        }
        let ty = self
            .expression_type(expression)?
            .unwrap_or(ColumnType::Text);
        let arguments = args
            .iter()
            .map(|arg| self.encode(arg, Some(&ty)).map(|value| value.node.into()))
            .collect::<Result<Vec<_>, _>>()?;
        let node = if name == "coalesce" {
            Node::new(
                "COALESCEEXPR",
                [
                    ("coalescetype", pg_type_oid(&ty).into()),
                    ("coalescecollid", pg_type_collation_oid(&ty).into()),
                    ("args", Field::List(arguments)),
                    ("location", (-1).into()),
                ],
            )
        } else {
            Node::new(
                "MINMAXEXPR",
                [
                    ("minmaxtype", pg_type_oid(&ty).into()),
                    ("minmaxcollid", pg_type_collation_oid(&ty).into()),
                    ("inputcollid", pg_type_collation_oid(&ty).into()),
                    ("op", i64::from(name == "least").into()),
                    ("args", Field::List(arguments)),
                    ("location", (-1).into()),
                ],
            )
        };
        Ok(Some(TypedNode { node, ty }))
    }

    pub(super) fn case(
        &self,
        expression: &Expr,
        base: Option<&Expr>,
        when: &[(Expr, Expr)],
        otherwise: Option<&Expr>,
    ) -> Result<TypedNode, SQLError> {
        let ty = self
            .expression_type(expression)?
            .unwrap_or(ColumnType::Text);
        let base = base.map(|base| self.encode(base, None)).transpose()?;
        let mut arguments = Vec::new();
        for (condition, result) in when {
            let condition = if let Some(base) = &base {
                let test = TypedNode {
                    node: Node::new(
                        "CASETESTEXPR",
                        [
                            ("typeId", pg_type_oid(&base.ty).into()),
                            ("typeMod", pg_type_modifier(&base.ty).into()),
                            ("collation", pg_type_collation_oid(&base.ty).into()),
                        ],
                    ),
                    ty: base.ty.clone(),
                };
                let condition = self.encode(condition, Some(&base.ty))?;
                Self::typed_binary(BinaryOp::Equal, test, condition)?
            } else {
                self.encode(condition, Some(&ColumnType::Boolean))?
            };
            let result = self.encode(result, Some(&ty))?;
            arguments.push(
                Node::new(
                    "CASEWHEN",
                    [
                        ("expr", condition.node.into()),
                        ("result", result.node.into()),
                        ("location", (-1).into()),
                    ],
                )
                .into(),
            );
        }
        let otherwise = match otherwise {
            Some(expression) => self.encode(expression, Some(&ty))?.node,
            None => values::constant(&Value::Null, &ty)?,
        };
        Ok(TypedNode {
            node: Node::new(
                "CASEEXPR",
                [
                    ("casetype", pg_type_oid(&ty).into()),
                    ("casecollid", pg_type_collation_oid(&ty).into()),
                    ("arg", base.map_or(Field::Null, |value| value.node.into())),
                    ("args", Field::List(arguments)),
                    ("defresult", otherwise.into()),
                    ("location", (-1).into()),
                ],
            ),
            ty,
        })
    }
}
