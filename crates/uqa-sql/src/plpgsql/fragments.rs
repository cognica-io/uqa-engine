//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Original embedded SQL and its optional definition-time validation tree.

use super::PLpgSQLCursorArgument;
use crate::{
    ast::{Expr, Statement},
    SQLError,
};
use std::sync::Arc;

/// `PostgreSQL`'s raw grammar selected for an embedded PL/pgSQL expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PLpgSQLParseMode {
    Statement,
    Expression,
    Assignment1,
    Assignment2,
    Assignment3,
}
impl TryFrom<i64> for PLpgSQLParseMode {
    type Error = SQLError;
    fn try_from(mode: i64) -> Result<Self, SQLError> {
        match mode {
            0 => Ok(Self::Statement),
            2 => Ok(Self::Expression),
            3 => Ok(Self::Assignment1),
            4 => Ok(Self::Assignment2),
            5 => Ok(Self::Assignment3),
            _ => Err(SQLError::Internal(format!(
                "invalid PL/pgSQL parse mode {mode}"
            ))),
        }
    }
}
impl PLpgSQLParseMode {
    pub(super) fn parser_mode(self) -> pg_query::ParseMode {
        match self {
            Self::Statement => pg_query::ParseMode::Default,
            Self::Expression => pg_query::ParseMode::PlPgSqlExpr,
            Self::Assignment1 => pg_query::ParseMode::PlPgSqlAssign1,
            Self::Assignment2 => pg_query::ParseMode::PlPgSqlAssign2,
            Self::Assignment3 => pg_query::ParseMode::PlPgSqlAssign3,
        }
    }
}

/// One syntactic occurrence, shared by clones but never interned by its text.
/// This carries no execution state; the execution owner supplies the cache.
#[derive(Debug)]
pub struct PLpgSQLSource {
    pub query: Arc<str>,
    pub mode: PLpgSQLParseMode,
}

/// The validation tree is absent when `PostgreSQL` compiles a runtime structure
/// without checking SQL that execution might never reach.
#[derive(Debug, Clone)]
pub struct PLpgSQLFragment<T> {
    source: Arc<PLpgSQLSource>,
    validation: Option<Arc<T>>,
}
impl<T> PLpgSQLFragment<T> {
    pub(super) fn new(source: Arc<PLpgSQLSource>, validation: Option<T>) -> Self {
        Self {
            source,
            validation: validation.map(Arc::new),
        }
    }
    pub fn source(&self) -> &Arc<PLpgSQLSource> {
        &self.source
    }
    pub fn validation(&self) -> Option<&T> {
        self.validation.as_deref()
    }
    /// Identity is valid for the lifetime of the source-owning compiled body.
    pub fn site(&self) -> usize {
        Arc::as_ptr(&self.source) as usize
    }
}
pub type PLpgSQLExpression = PLpgSQLFragment<Expr>;
pub type PLpgSQLStatement = PLpgSQLFragment<Statement>;
pub type PLpgSQLCursorArguments = PLpgSQLFragment<Vec<PLpgSQLCursorArgument>>;

impl PLpgSQLExpression {
    pub fn parse_statement(&self) -> Result<Statement, SQLError> {
        self.parse()
            .map(super::variable_conflicts::expression_query)
    }
    pub fn parse(&self) -> Result<Expr, SQLError> {
        super::lowering_expression::compile_expression_source(&self.source)
    }
}
impl PLpgSQLStatement {
    pub fn parse(&self) -> Result<Statement, SQLError> {
        super::lowering_expression::compile_statement_source(&self.source)
    }
}
impl PLpgSQLCursorArguments {
    pub fn parse_statement(&self) -> Result<Statement, SQLError> {
        let arguments = self.parse()?;
        let Statement::Select(mut query) =
            super::variable_conflicts::expression_query(Expr::Literal(uqa_core::Value::Null))
        else {
            unreachable!()
        };
        query.projections = arguments
            .into_iter()
            .map(|argument| crate::ast::Projection {
                expr: argument.expr,
                alias: argument.name,
            })
            .collect();
        Ok(Statement::Select(query))
    }
    pub fn parse(&self) -> Result<Vec<PLpgSQLCursorArgument>, SQLError> {
        super::lowering_expression::compile_cursor_arguments_source(&self.source)
    }
    pub fn empty() -> Self {
        Self::new(
            Arc::new(PLpgSQLSource {
                query: Arc::from(""),
                mode: PLpgSQLParseMode::Expression,
            }),
            Some(Vec::new()),
        )
    }
}

/// Only a clone of the same compilation shares first-use sites. A new parse,
/// including replacement or a specialization, receives a fresh identity.
#[derive(Debug, Clone, Default)]
pub struct PLpgSQLCompilationIdentity(Arc<()>);
impl PLpgSQLCompilationIdentity {
    pub fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PLpgSQLCompileMode {
    #[default]
    Validate,
    Runtime,
}
impl PLpgSQLCompileMode {
    pub(super) const fn validates(self) -> bool {
        matches!(self, Self::Validate)
    }
}

#[cfg(test)]
mod tests;
