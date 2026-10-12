//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind catalog expression nodes to declared column and domain-value types.

mod coercion;
mod collations;
mod constructs;
mod operators;
mod rows;
mod schema;
mod subscripts;
pub use schema::{ColumnReference, ExpressionSchema};

use super::{values, Field, Node};
use crate::ast::{BinaryOp, Expr, FunctionBinding, FunctionCallSyntax};
use crate::catalog::type_metadata::{pg_type_collation_oid, pg_type_modifier, pg_type_oid};
use crate::type_resolution::{
    binary_operator_catalog_entry, binary_operator_types, common_context_expression_type,
    FunctionTypeResolver,
};
use crate::{ColumnType, SQLError};

pub struct RoutineIdentity {
    pub oid: i64,
    pub argument_types: Vec<ColumnType>,
    pub result_type: ColumnType,
}

pub trait ExpressionRoutines {
    fn resolve(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        argument_types: &[Option<ColumnType>],
    ) -> Result<RoutineIdentity, SQLError>;
}

pub struct ExpressionContext<'a> {
    pub schema: &'a dyn ExpressionSchema,
    pub domain_value: Option<&'a ColumnType>,
    pub types: Option<&'a dyn FunctionTypeResolver>,
    pub routines: &'a dyn ExpressionRoutines,
}

struct TypedNode {
    node: Node,
    ty: ColumnType,
}

impl ExpressionContext<'_> {
    pub fn check(&self, expression: &Expr) -> Result<Node, SQLError> {
        let mut value = self.encode(expression, Some(&ColumnType::Boolean))?;
        collations::assign(&mut value.node)?;
        Ok(value.node)
    }

    fn expression_type(&self, expression: &Expr) -> Result<Option<ColumnType>, SQLError> {
        let plan = crate::plan::ExpressionPlan::lower(expression.clone());
        common_context_expression_type(&plan.scalar, self.schema, &[], self.types)
    }

    fn encode(
        &self,
        expression: &Expr,
        expected: Option<&ColumnType>,
    ) -> Result<TypedNode, SQLError> {
        if let Some(clause) = crate::catalog::collations::expression::ast_clause(expression)? {
            return self.collated_expression(clause, expected);
        }
        let value = match expression {
            Expr::Column(column) => self.column(column, None)?,
            Expr::QualifiedColumn { qualifier, column } => self.column(column, Some(qualifier))?,
            Expr::Literal(value) => self.literal(expression, value, expected)?,
            Expr::TypedLiteral {
                value,
                ty,
                composite_source,
                collation_oid,
            } => self.retained_constant(
                value,
                self.resolve_type(ty)?,
                composite_source.as_deref(),
                *collation_oid,
            )?,
            Expr::Binary { op, lhs, rhs } => self.binary(*op, lhs, rhs)?,
            Expr::UnaryMinus(argument) => self.unary_minus(argument)?,
            Expr::Array(elements) => self.array(elements, expected)?,
            Expr::Row(elements) => self.row(elements)?,
            Expr::CompositeRow { items, binding } => self.stored_row(items, binding)?,
            Expr::InList {
                expr,
                list,
                negated,
            } => self.in_list(expr, list, *negated)?,
            Expr::Case {
                base,
                when,
                else_branch,
            } => self.case(expression, base.as_deref(), when, else_branch.as_deref())?,
            Expr::And(items) => self.boolean("and", items)?,
            Expr::Or(items) => self.boolean("or", items)?,
            Expr::Not(item) => self.boolean("not", std::slice::from_ref(item.as_ref()))?,
            Expr::IsNull { expr, negated } => {
                let (arg, is_row) = self.uncoerced_input(expr)?;
                TypedNode {
                    node: Node::new(
                        "NULLTEST",
                        [
                            ("arg", arg.into()),
                            ("nulltesttype", i64::from(*negated).into()),
                            ("argisrow", is_row.into()),
                            ("location", (-1).into()),
                        ],
                    ),
                    ty: ColumnType::Boolean,
                }
            }
            Expr::Between { expr, low, high } => self.boolean(
                "and",
                &[
                    Expr::Binary {
                        op: BinaryOp::GreaterEqual,
                        lhs: expr.clone(),
                        rhs: low.clone(),
                    },
                    Expr::Binary {
                        op: BinaryOp::LessEqual,
                        lhs: expr.clone(),
                        rhs: high.clone(),
                    },
                ],
            )?,
            Expr::Func {
                order_syntax,
                name,
                binding,
                args,
                distinct: false,
                order_by,
                filter: None,
            } if order_by.is_empty() => {
                if let Some(value) = self.construct(expression, name, binding.as_ref(), args)? {
                    value
                } else {
                    self.function(name, binding.as_ref(), args, *order_syntax)?
                }
            }
            Expr::Cast { expr, ty, implicit } => self.cast_expression(expr, ty, *implicit)?,
            _ => {
                return Err(SQLError::Unsupported(
                    "catalog expression node encoding for this expression".into(),
                ))
            }
        };
        if let Some(ty) = expected {
            Self::coerce(value, ty, 2)
        } else {
            Ok(value)
        }
    }

    fn literal(
        &self,
        expression: &Expr,
        value: &uqa_core::Value,
        expected: Option<&ColumnType>,
    ) -> Result<TypedNode, SQLError> {
        let source = self.expression_type(expression)?;
        let ty = source
            .as_ref()
            .or(expected)
            .cloned()
            .unwrap_or(ColumnType::Text);
        let value = if source.is_none() {
            if let Some(value) = crate::expr::enums::fold_unknown_literal(
                self.types.and_then(FunctionTypeResolver::enum_labels),
                value,
                &ty,
            )? {
                value
            } else if let uqa_core::Value::Str(text) = value {
                if crate::expr::requires_catalog_constant_input(&ty) {
                    self.types
                        .and_then(FunctionTypeResolver::catalog_input_functions)
                        .ok_or_else(|| {
                            SQLError::Internal("catalog input functions are unavailable".into())
                        })?
                        .read_unknown_input(text, &ty)?
                } else {
                    crate::type_resolution::coerce_common_context_value(
                        value.clone(),
                        None,
                        Some(&ty),
                    )?
                }
            } else {
                crate::type_resolution::coerce_common_context_value(value.clone(), None, Some(&ty))?
            }
        } else {
            crate::type_resolution::coerce_common_context_value(
                value.clone(),
                source.as_ref(),
                Some(&ty),
            )?
        };
        self.constant(&value, ty, None)
    }

    fn constant(
        &self,
        value: &uqa_core::Value,
        ty: ColumnType,
        source: Option<&crate::expr::composites::CompositeConstantSource>,
    ) -> Result<TypedNode, SQLError> {
        Ok(TypedNode {
            node: values::constant_with_source(
                value,
                &ty,
                source,
                self.types.and_then(FunctionTypeResolver::composite_types),
            )?,
            ty,
        })
    }

    fn retained_constant(
        &self,
        value: &uqa_core::Value,
        ty: ColumnType,
        source: Option<&crate::expr::composites::CompositeConstantSource>,
        collation: Option<u32>,
    ) -> Result<TypedNode, SQLError> {
        let mut value = self.constant(value, ty, source)?;
        if let Some(oid) = collation {
            crate::catalog::collations::expression::validate_literal_collation(
                Some(&value.ty),
                Some(oid),
            )?;
            value
                .node
                .fields
                .iter_mut()
                .find(|(name, _)| name == "constcollid")
                .expect("constant collation")
                .1 = i64::from(oid).into();
        }
        Ok(value)
    }

    fn resolve_type(&self, name: &str) -> Result<ColumnType, SQLError> {
        if let Some(resolver) = self.types {
            if let Some(ty) = resolver.resolve_type_name(name)? {
                return Ok(ty);
            }
        }
        ColumnType::from_sql_name(name)
    }

    fn column(&self, name: &str, qualifier: Option<&str>) -> Result<TypedNode, SQLError> {
        if let Some(ty) = self.domain_value {
            if name != "value" || qualifier.is_some() {
                return Err(SQLError::UnknownColumn(name.into()));
            }
            return Ok(TypedNode {
                node: Node::new(
                    "COERCETODOMAINVALUE",
                    [
                        ("typeId", pg_type_oid(ty).into()),
                        ("typeMod", pg_type_modifier(ty).into()),
                        ("collation", pg_type_collation_oid(ty).into()),
                        ("location", (-1).into()),
                    ],
                ),
                ty: ty.clone(),
            });
        }
        let column = self.schema.column_reference(name, qualifier)?;
        let ty = column.ty;
        let ordinal = column.attribute_number;
        Ok(TypedNode {
            node: Node::new(
                "VAR",
                [
                    ("varno", 1.into()),
                    ("varattno", ordinal.into()),
                    ("vartype", pg_type_oid(ty).into()),
                    ("vartypmod", pg_type_modifier(ty).into()),
                    ("varcollid", column.collation_oid.into()),
                    ("varnullingrels", Field::List(vec![Field::Atom("b".into())])),
                    ("varlevelsup", 0.into()),
                    ("varreturningtype", 0.into()),
                    ("varnosyn", 1.into()),
                    ("varattnosyn", ordinal.into()),
                    ("location", (-1).into()),
                ],
            ),
            ty: ty.clone(),
        })
    }

    fn binary(&self, op: BinaryOp, lhs: &Expr, rhs: &Expr) -> Result<TypedNode, SQLError> {
        if let (Expr::Row(left), Expr::Row(right)) = (lhs, rhs) {
            return self.row_comparison(op, left, right);
        }
        self.scalar_binary(op, lhs, rhs)
    }

    fn scalar_binary(&self, op: BinaryOp, lhs: &Expr, rhs: &Expr) -> Result<TypedNode, SQLError> {
        let types = [self.expression_type(lhs)?, self.expression_type(rhs)?];
        let [left, right, result] =
            binary_operator_types(op, types[0].as_ref(), types[1].as_ref())?;
        let identity = binary_operator_catalog_entry(op, [&left, &right])?;
        let arguments = [
            self.encode(lhs, Some(&left))?,
            self.encode(rhs, Some(&right))?,
        ];
        Ok(operator_node(
            identity.oid,
            identity.function_oid,
            arguments,
            result,
        ))
    }

    fn boolean(&self, operator: &str, args: &[Expr]) -> Result<TypedNode, SQLError> {
        let args = args
            .iter()
            .map(|arg| {
                self.encode(arg, Some(&ColumnType::Boolean))
                    .map(|value| value.node.into())
            })
            .collect::<Result<_, _>>()?;
        Ok(TypedNode {
            node: Node::new(
                "BOOLEXPR",
                [
                    ("boolop", Field::Atom(operator.into())),
                    ("args", Field::List(args)),
                    ("location", (-1).into()),
                ],
            ),
            ty: ColumnType::Boolean,
        })
    }

    fn function(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        arguments: &[Expr],
        syntax: FunctionCallSyntax,
    ) -> Result<TypedNode, SQLError> {
        let types = arguments
            .iter()
            .map(|arg| self.expression_type(arg))
            .collect::<Result<Vec<_>, _>>()?;
        let routine = self.routines.resolve(name, binding, &types)?;
        if routine.argument_types.len() != arguments.len() {
            return Err(SQLError::Internal(
                "catalog routine arity differs from bound arguments".into(),
            ));
        }
        let arguments = arguments
            .iter()
            .zip(&routine.argument_types)
            .map(|(arg, ty)| self.encode(arg, Some(ty)))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(TypedNode {
            node: Node::new(
                "FUNCEXPR",
                [
                    ("funcid", routine.oid.into()),
                    ("funcresulttype", pg_type_oid(&routine.result_type).into()),
                    ("funcretset", false.into()),
                    ("funcvariadic", false.into()),
                    (
                        "funcformat",
                        if syntax == FunctionCallSyntax::Extract {
                            3.into()
                        } else {
                            0.into()
                        },
                    ),
                    (
                        "funccollid",
                        pg_type_collation_oid(&routine.result_type).into(),
                    ),
                    ("inputcollid", 0.into()),
                    (
                        "args",
                        Field::List(arguments.into_iter().map(|arg| arg.node.into()).collect()),
                    ),
                    ("location", (-1).into()),
                ],
            ),
            ty: routine.result_type,
        })
    }
}

fn operator_node(
    oid: i64,
    function_oid: i64,
    arguments: impl IntoIterator<Item = TypedNode>,
    result: ColumnType,
) -> TypedNode {
    let arguments: Vec<_> = arguments.into_iter().collect();
    TypedNode {
        node: Node::new(
            "OPEXPR",
            [
                ("opno", oid.into()),
                ("opfuncid", function_oid.into()),
                ("opresulttype", pg_type_oid(&result).into()),
                ("opretset", false.into()),
                ("opcollid", pg_type_collation_oid(&result).into()),
                ("inputcollid", 0.into()),
                (
                    "args",
                    Field::List(arguments.into_iter().map(|arg| arg.node.into()).collect()),
                ),
                ("location", (-1).into()),
            ],
        ),
        ty: result,
    }
}
