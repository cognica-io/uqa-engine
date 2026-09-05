//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed index keys retain the legacy string encoding for ordinary columns.

use std::borrow::Cow;

use serde::{Deserialize, Serialize};

use super::Expr;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum IndexKey {
    Column(String),
    Expression(Box<Expr>),
}

impl IndexKey {
    #[must_use]
    pub fn from_expression(expression: Expr) -> Self {
        match expression {
            Expr::Column(column) => Self::Column(column),
            other => Self::Expression(Box::new(other)),
        }
    }

    #[must_use]
    pub fn column(&self) -> Option<&str> {
        match self {
            Self::Column(column) => Some(column),
            Self::Expression(_) => None,
        }
    }

    #[must_use]
    pub fn expression(&self) -> Cow<'_, Expr> {
        match self {
            Self::Column(column) => Cow::Owned(Expr::Column(column.clone())),
            Self::Expression(expression) => Cow::Borrowed(expression),
        }
    }
}

impl From<String> for IndexKey {
    fn from(column: String) -> Self {
        Self::Column(column)
    }
}

impl From<&str> for IndexKey {
    fn from(column: &str) -> Self {
        Self::Column(column.into())
    }
}
