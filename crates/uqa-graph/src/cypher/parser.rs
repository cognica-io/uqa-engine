//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Recursive-descent parser for the supported openCypher grammar.

use std::collections::BTreeMap;

use uqa_core::Value;

use crate::cypher::ast::{
    BinaryOp, CaseExpr, CreateClause, CypherClause, CypherExpr, CypherQuery, DeleteClause,
    FunctionCall, InList, IsNotNull, IsNull, ListComprehension, ListIndex, ListLiteral, ListSlice,
    Literal, MapLiteral, MatchClause, MergeClause, NodePattern, OrderByItem, Parameter,
    PathElement, PathPattern, PropertyAccess, RelDirection, RelPattern, ReturnClause, ReturnItem,
    SetClause, SetItem, SetOperator, UnaryOp, UnwindClause, Variable, WithClause,
};
use crate::cypher::lexer::{is_keyword, tokenize, LexError, Token, TokenKind};

mod atoms;
mod clauses;
mod expressions;
mod patterns;
mod stream;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ParseError {
    #[error(transparent)]
    Lex(#[from] LexError),
    #[error("expected {expected}, got {got:?} ({value:?}) at position {position}")]
    Expected {
        expected: &'static str,
        got: TokenKind,
        value: String,
        position: usize,
    },
    #[error("expected keyword {keyword:?}, got {got:?} at position {position}")]
    ExpectedKeyword {
        keyword: &'static str,
        got: String,
        position: usize,
    },
    #[error("unexpected token {got:?} at position {position}")]
    Unexpected { got: String, position: usize },
}

/// Parse a Cypher query string into a Cypher query AST.
pub fn parse_cypher(source: &str) -> Result<CypherQuery, ParseError> {
    let tokens = tokenize(source)?;
    let mut parser = Parser { tokens, pos: 0 };
    parser.parse()
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

const RESERVED_KEYWORDS: &[&str] = &[
    "AND",
    "AS",
    "ASC",
    "BY",
    "CASE",
    "CONTAINS",
    "CREATE",
    "DELETE",
    "DESC",
    "DETACH",
    "DISTINCT",
    "ELSE",
    "END",
    "ENDS",
    "EXISTS",
    "FALSE",
    "IN",
    "IS",
    "LIMIT",
    "MATCH",
    "MERGE",
    "NODE",
    "NOT",
    "NULL",
    "ON",
    "OPTIONAL",
    "OR",
    "ORDER",
    "RELATIONSHIP",
    "REMOVE",
    "RETURN",
    "SET",
    "SKIP",
    "STARTS",
    "THEN",
    "TRUE",
    "UNWIND",
    "WHEN",
    "WHERE",
    "WITH",
    "XOR",
];

#[cfg(test)]
mod tests;
