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
        if let Some(value) = self.array_construct(binding, args)? {
            return Ok(Some(value));
        }
        match binding.and_then(|binding| binding.dispatch) {
            Some(FunctionDispatch::FieldSelect) => {
                return self
                    .field_select(binding.expect("field selection dispatch"), args)
                    .map(Some)
            }
            Some(FunctionDispatch::BooleanTest(test)) => {
                return self.boolean_test(args, test).map(Some);
            }
            Some(FunctionDispatch::ValueNullTest { negated }) => {
                return self.value_null_test(args, negated).map(Some);
            }
            Some(FunctionDispatch::NumericOperator(operator)) => {
                return self.numeric_operator(operator, args).map(Some);
            }
            Some(dispatch @ (FunctionDispatch::AnyOperator | FunctionDispatch::AllOperator)) => {
                return self
                    .quantified_operator(args, dispatch == FunctionDispatch::AnyOperator)
                    .map(Some);
            }
            Some(FunctionDispatch::IsDistinct | FunctionDispatch::ValueIsDistinct) => {
                let [lhs, rhs] = args else {
                    return Err(SQLError::Internal("invalid distinct operands".into()));
                };
                if binding
                    .is_some_and(|binding| binding.dispatch == Some(FunctionDispatch::IsDistinct))
                {
                    if let Some(value) = self.row_distinct(lhs, rhs)? {
                        return Ok(Some(value));
                    }
                }
                let mut value = self.scalar_binary(BinaryOp::Equal, lhs, rhs)?;
                value.node.kind = "DISTINCTEXPR".into();
                return Ok(Some(value));
            }
            Some(_) => return Ok(None),
            None => {}
        }
        if binding.is_some_and(|binding| binding.object_id.is_some()) {
            return Ok(None);
        }
        if name == "nullif" && binding.is_some_and(|binding| binding.builtin) {
            return self.null_if(args).map(Some);
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
                    ("inputcollid", 0.into()),
                    ("op", i64::from(name == "least").into()),
                    ("args", Field::List(arguments)),
                    ("location", (-1).into()),
                ],
            )
        };
        Ok(Some(TypedNode { node, ty: Some(ty) }))
    }

    fn quantified_operator(&self, args: &[Expr], any: bool) -> Result<TypedNode, SQLError> {
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
        self.scalar_array(op, lhs, rhs, any)
    }

    fn boolean_test(
        &self,
        args: &[Expr],
        test: crate::ast::BooleanTest,
    ) -> Result<TypedNode, SQLError> {
        let [argument] = args else {
            return Err(SQLError::Internal("Boolean test takes one input".into()));
        };
        crate::type_resolution::boolean::require_argument_type(
            self.expression_type(argument)?.as_ref(),
            test.label(),
            self.types,
        )?;
        let argument = self.encode(argument, Some(&ColumnType::Boolean))?;
        Ok(TypedNode {
            node: Node::new(
                "BOOLEANTEST",
                [
                    ("arg", argument.node.into()),
                    ("booltesttype", test.catalog_code().into()),
                    ("location", (-1).into()),
                ],
            ),
            ty: Some(ColumnType::Boolean),
        })
    }

    fn value_null_test(&self, args: &[Expr], negated: bool) -> Result<TypedNode, SQLError> {
        let [argument] = args else {
            return Err(SQLError::Internal("invalid outer NULL test".into()));
        };
        let (argument, _) = self.uncoerced_input(argument)?;
        Ok(TypedNode {
            node: Node::new(
                "NULLTEST",
                [
                    ("arg", argument.into()),
                    ("nulltesttype", i64::from(negated).into()),
                    ("argisrow", false.into()),
                    ("location", (-1).into()),
                ],
            ),
            ty: Some(ColumnType::Boolean),
        })
    }

    fn null_if(&self, args: &[Expr]) -> Result<TypedNode, SQLError> {
        let [lhs, rhs] = args else {
            return Err(SQLError::Internal("invalid NULLIF operands".into()));
        };
        let types = [self.expression_type(lhs)?, self.expression_type(rhs)?];
        let [left, right, _] = crate::type_resolution::binary_operator_types(
            BinaryOp::Equal,
            types[0].as_ref(),
            types[1].as_ref(),
        )?;
        let identity = crate::type_resolution::binary_operator_catalog_entry(
            BinaryOp::Equal,
            [&left, &right],
        )?;
        let left = self.encode(lhs, Some(&left))?;
        let right = self.encode(rhs, Some(&right))?;
        let result = left.type_ref()?.clone();
        let mut value =
            super::operator_node(identity.oid, identity.function_oid, [left, right], result);
        value.node.kind = "NULLIFEXPR".into();
        Ok(value)
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
                            ("typeId", pg_type_oid(base.type_ref()?).into()),
                            ("typeMod", pg_type_modifier(base.type_ref()?).into()),
                            ("collation", pg_type_collation_oid(base.type_ref()?).into()),
                        ],
                    ),
                    ty: base.ty.clone(),
                };
                let condition = self.encode(condition, base.ty.as_ref())?;
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
            ty: Some(ty),
        })
    }
}
