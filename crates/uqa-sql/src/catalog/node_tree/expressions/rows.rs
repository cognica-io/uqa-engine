//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Preserve row construction and the selected scalar operators in catalog comparisons.

use super::{
    values, BinaryOp, ColumnType, Expr, ExpressionContext, Field, Node, SQLError, TypedNode,
};
use crate::catalog::type_metadata::pg_type_oid;
use crate::type_resolution::binary_operator_by_oid;
use uqa_core::Value;

mod distinct;
mod named;

impl ExpressionContext<'_> {
    pub(super) fn row(&self, elements: &[Expr]) -> Result<TypedNode, SQLError> {
        let arguments = elements
            .iter()
            .map(|element| {
                if let Expr::Literal(value @ (Value::Null | Value::Str(_))) = element {
                    values::unknown_constant(value).map(Field::Node)
                } else {
                    self.encode(element, None).map(|value| value.node.into())
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        let names = (1..=elements.len())
            .map(|position| Field::StringValue(format!("f{position}")))
            .collect();
        Ok(TypedNode {
            node: Node::new(
                "ROWEXPR",
                [
                    ("args", node_list(arguments)),
                    ("row_typeid", pg_type_oid(&ColumnType::Record).into()),
                    ("row_format", 2.into()),
                    ("colnames", node_list(names)),
                    ("location", (-1).into()),
                ],
            ),
            ty: ColumnType::Record,
        })
    }

    pub(super) fn row_comparison(
        &self,
        op: BinaryOp,
        left: &[Expr],
        right: &[Expr],
    ) -> Result<TypedNode, SQLError> {
        require_equal_width(left, right)?;
        if left.is_empty() {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "cannot compare rows of zero length".into(),
            });
        }
        let mut operators = left
            .iter()
            .zip(right)
            .map(|(left, right)| {
                let selected = self.scalar_binary(op, left, right)?;
                if selected.ty != ColumnType::Boolean {
                    return Err(SQLError::Routine {
                        sqlstate: "42804".into(),
                        message: format!(
                            "row comparison operator must yield type boolean, not type {}",
                            selected.ty.sql_name()
                        ),
                    });
                }
                Ok(selected.node)
            })
            .collect::<Result<Vec<_>, _>>()?;
        if operators.len() == 1 {
            return Ok(boolean_node(operators.pop().expect("one comparison")));
        }
        let operator_name = binary_operator_by_oid(operators[0].integer("opno")?)
            .ok_or_else(|| super::super::invalid("unknown row comparison operator"))?
            .name;
        let interpretations = operators
            .iter()
            .map(|node| {
                binary_operator_by_oid(node.integer("opno")?)
                    .ok_or_else(|| super::super::invalid("unknown row comparison operator"))
                    .map(|operator| {
                        operator
                            .row_comparison_interpretations()
                            .collect::<Vec<_>>()
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let comparison = (1..=6)
            .find(|comparison| {
                interpretations
                    .iter()
                    .all(|options| options.iter().any(|(candidate, _)| candidate == comparison))
            })
            .ok_or_else(|| SQLError::Diagnostic {
                sqlstate: "0A000".into(),
                message: format!(
                    "could not determine interpretation of row comparison operator {operator_name}"
                ),
                detail: None,
                hint: Some(
                    "Row comparison operators must be associated with btree operator families."
                        .into(),
                ),
            })?;
        if matches!(comparison, 3 | 6) {
            return Ok(boolean_node(boolean(
                if comparison == 3 { "and" } else { "or" },
                operators,
            )));
        }
        row_ordering_node(comparison, operators, interpretations)
    }
}

fn row_ordering_node(
    comparison: i64,
    operators: Vec<Node>,
    interpretations: Vec<Vec<(i64, i64)>>,
) -> Result<TypedNode, SQLError> {
    let mut opnos = Vec::new();
    let mut families = Vec::new();
    let mut collations = Vec::new();
    let mut left = Vec::new();
    let mut right = Vec::new();
    for (mut operator, options) in operators.into_iter().zip(interpretations) {
        opnos.push(operator.integer("opno")?);
        families.push(
            options
                .into_iter()
                .find_map(|(candidate, family)| (candidate == comparison).then_some(family))
                .expect("common comparison interpretation"),
        );
        collations.push(operator.integer("inputcollid")?);
        let arguments = operator
            .fields
            .iter_mut()
            .find_map(|(name, field)| (name == "args").then_some(field))
            .expect("scalar operator arguments");
        let Field::List(mut arguments) = std::mem::replace(arguments, Field::Null) else {
            return Err(super::super::invalid("invalid scalar operator arguments"));
        };
        if arguments.len() != 2 {
            return Err(super::super::invalid("invalid scalar operator arity"));
        }
        right.push(arguments.pop().expect("right argument"));
        left.push(arguments.pop().expect("left argument"));
    }
    Ok(boolean_node(Node::new(
        "ROWCOMPAREEXPR",
        [
            ("cmptype", comparison.into()),
            ("opnos", oid_list(opnos)),
            ("opfamilies", oid_list(families)),
            ("inputcollids", oid_list(collations)),
            ("largs", Field::List(left)),
            ("rargs", Field::List(right)),
        ],
    )))
}

fn require_equal_width<T, U>(left: &[T], right: &[U]) -> Result<(), SQLError> {
    if left.len() != right.len() {
        return Err(SQLError::Routine {
            sqlstate: "42601".into(),
            message: "unequal number of entries in row expressions".into(),
        });
    }
    Ok(())
}

fn node_list(values: Vec<Field>) -> Field {
    if values.is_empty() {
        Field::Null
    } else {
        Field::List(values)
    }
}

fn oid_list(values: Vec<i64>) -> Field {
    Field::List(
        std::iter::once(Field::Atom("o".into()))
            .chain(values.into_iter().map(Field::from))
            .collect(),
    )
}

fn boolean(operator: &str, values: Vec<Node>) -> Node {
    Node::new(
        "BOOLEXPR",
        [
            ("boolop", Field::Atom(operator.into())),
            (
                "args",
                Field::List(values.into_iter().map(Field::Node).collect()),
            ),
            ("location", (-1).into()),
        ],
    )
}

fn boolean_node(node: Node) -> TypedNode {
    TypedNode {
        node,
        ty: ColumnType::Boolean,
    }
}
