//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Observe every ROW child before selecting the enclosing constructor's field coercions.

use super::{
    node_list, pg_type_oid, values, ColumnType, Expr, ExpressionContext, Field, Node, SQLError,
    TypedNode, Value,
};

pub(super) enum RowInput<'a> {
    Unknown(&'a Expr),
    Row(Vec<Self>),
    Value(TypedNode),
}

impl RowInput<'_> {
    pub(super) fn ty(&self) -> Option<&ColumnType> {
        match self {
            Self::Unknown(_) => None,
            Self::Row(_) => Some(&ColumnType::Record),
            Self::Value(value) => Some(&value.ty),
        }
    }

    fn into_node(self) -> Result<Node, SQLError> {
        match self {
            Self::Unknown(Expr::Literal(value)) => values::unknown_constant(value),
            Self::Unknown(_) => unreachable!("unknown ROW literal"),
            Self::Row(fields) => anonymous_row(fields).map(|value| value.node),
            Self::Value(value) => Ok(value.node),
        }
    }
}

impl ExpressionContext<'_> {
    pub(super) fn row_inputs<'a>(
        &self,
        elements: &'a [Expr],
    ) -> Result<Vec<RowInput<'a>>, SQLError> {
        elements
            .iter()
            .map(|element| self.row_input(element))
            .collect()
    }

    pub(super) fn row_input<'a>(&self, expression: &'a Expr) -> Result<RowInput<'a>, SQLError> {
        if let Some(elements) = self.anonymous_row_elements(expression)? {
            return self.row_inputs(elements).map(RowInput::Row);
        }
        if matches!(expression, Expr::Literal(Value::Null | Value::Str(_))) {
            return Ok(RowInput::Unknown(expression));
        }
        self.encode(expression, None).map(RowInput::Value)
    }
}

pub(super) fn anonymous_row(fields: Vec<RowInput<'_>>) -> Result<TypedNode, SQLError> {
    let names = (1..=fields.len())
        .map(|position| Field::StringValue(format!("f{position}")))
        .collect();
    let arguments = fields
        .into_iter()
        .map(|field| field.into_node().map(Field::Node))
        .collect::<Result<Vec<_>, _>>()?;
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
