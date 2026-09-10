//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Common-table-expression syntax and recursive traversal controls.

use serde::{Deserialize, Serialize};

use super::{DeleteStmt, Expr, InsertStmt, MergeStmt, SelectStmt, Statement, UpdateStmt};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CTE {
    pub name: String,
    pub columns: Vec<String>,
    pub recursive: bool,
    #[serde(default)]
    pub materialization: CteMaterialization,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<CteSearchClause>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cycle: Option<CteCycleClause>,
    #[serde(flatten)]
    pub body: CteBody,
}

/// A CTE owns a query or a data-modifying statement. The serialized `query` arm retains the original SELECT-only catalog representation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CteBody {
    #[serde(rename = "query")]
    Query(Box<SelectStmt>),
    #[serde(rename = "insert")]
    Insert(Box<InsertStmt>),
    #[serde(rename = "update")]
    Update(Box<UpdateStmt>),
    #[serde(rename = "delete")]
    Delete(Box<DeleteStmt>),
    #[serde(rename = "merge")]
    Merge(Box<MergeStmt>),
}

impl CteBody {
    pub fn query(&self) -> Option<&SelectStmt> {
        match self {
            Self::Query(query) => Some(query),
            _ => None,
        }
    }

    pub fn query_mut(&mut self) -> Option<&mut SelectStmt> {
        match self {
            Self::Query(query) => Some(query),
            _ => None,
        }
    }

    pub const fn modifies_data(&self) -> bool {
        !matches!(self, Self::Query(_))
    }

    pub fn returning(&self) -> Option<&[super::Projection]> {
        match self {
            Self::Query(_) => None,
            Self::Insert(command) => Some(&command.returning),
            Self::Update(command) => Some(&command.returning),
            Self::Delete(command) => Some(&command.returning),
            Self::Merge(command) => Some(&command.returning),
        }
    }

    pub fn into_statement(self) -> Statement {
        match self {
            Self::Query(query) => Statement::Select(query),
            Self::Insert(command) => Statement::Insert(*command),
            Self::Update(command) => Statement::Update(*command),
            Self::Delete(command) => Statement::Delete(*command),
            Self::Merge(command) => Statement::Merge(*command),
        }
    }
}

impl TryFrom<Statement> for CteBody {
    type Error = crate::SQLError;

    fn try_from(statement: Statement) -> Result<Self, Self::Error> {
        match statement {
            Statement::Select(query) => Ok(Self::Query(query)),
            Statement::Insert(command) => Ok(Self::Insert(Box::new(command))),
            Statement::Update(command) => Ok(Self::Update(Box::new(command))),
            Statement::Delete(command) => Ok(Self::Delete(Box::new(command))),
            Statement::Merge(command) => Ok(Self::Merge(Box::new(command))),
            _ => Err(crate::SQLError::Unsupported(
                "WITH body must be a SELECT, INSERT, UPDATE, DELETE, or MERGE statement".into(),
            )),
        }
    }
}

/// The planning fence requested for one common-table expression.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CteMaterialization {
    #[default]
    Default,
    Materialized,
    NotMaterialized,
}

/// `PostgreSQL` recursive-CTE traversal-order metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CteSearchClause {
    pub columns: Vec<String>,
    pub breadth_first: bool,
    pub sequence_column: String,
}

/// `PostgreSQL` recursive-CTE cycle detection metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CteCycleClause {
    pub columns: Vec<String>,
    pub mark_column: String,
    pub mark_value: Expr,
    pub mark_default: Expr,
    pub path_column: String,
}
