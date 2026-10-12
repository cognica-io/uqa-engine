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
    Collated { argument: Box<Self>, oid: u32 },
    Row(Vec<Self>),
    Value(TypedNode),
}

impl RowInput<'_> {
    pub(super) fn ty(&self) -> Option<&ColumnType> {
        match self {
            Self::Unknown(_) => None,
            Self::Collated { argument, .. } => argument.ty(),
            Self::Row(_) => Some(&ColumnType::Record),
            Self::Value(value) => Some(&value.ty),
        }
    }

    fn into_node(self) -> Result<Node, SQLError> {
        match self {
            Self::Unknown(Expr::Literal(value)) => values::unknown_constant(value),
            Self::Unknown(_) => unreachable!("unknown ROW literal"),
            Self::Collated { argument, oid } => Ok(super::super::collations::collate(
                argument.into_node()?,
                i64::from(oid),
            )),
            Self::Row(fields) => anonymous_row(fields).map(|value| value.node),
            Self::Value(value) => Ok(value.node),
        }
    }
}

impl ExpressionContext<'_> {
    /// NULL tests, like ROW fields, do not select a type for unknown literals.
    pub(in super::super) fn uncoerced_input(
        &self,
        expression: &Expr,
    ) -> Result<(Node, bool), SQLError> {
        let input = self.row_input(expression)?;
        let mut ty = input.ty();
        while let Some(ColumnType::Domain { base, .. }) = ty {
            ty = Some(base);
        }
        let is_row = matches!(ty, Some(ColumnType::Record | ColumnType::Composite(_)));
        Ok((input.into_node()?, is_row))
    }

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
        if let Some(clause) = crate::catalog::collations::expression::ast_clause(expression)? {
            let argument = self.row_input(clause.argument)?;
            let oid = crate::catalog::collations::expression::select(
                clause.name,
                clause.oid,
                argument.ty(),
                self.types,
            )?;
            return Ok(RowInput::Collated {
                argument: Box::new(argument),
                oid,
            });
        }
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
