//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! openCypher subset: lexer, AST, recursive-descent parser. The
//! compiler that lowers AST onto graph operators lives in a separate
//! slice; this module is the front-end only.

pub mod ast;
pub mod executor;
pub mod lexer;
pub mod parser;
pub mod writer;

pub use ast::{
    BinaryOp, CaseExpr, CreateClause, CypherClause, CypherExpr, CypherQuery, DeleteClause,
    FunctionCall, InList, IsNotNull, IsNull, ListComprehension, ListIndex, ListLiteral, ListSlice,
    Literal, MapLiteral, MatchClause, MergeClause, NodePattern, OrderByItem, Parameter,
    PathElement, PathPattern, PropertyAccess, RelDirection, RelPattern, ReturnClause, ReturnItem,
    SetClause, SetItem, SetOperator, UnaryOp, UnwindClause, Variable, WithClause,
};
pub use executor::{Binding, BindingRow, CypherError, CypherExecutor, ResultRow};
pub use lexer::{tokenize, LexError, Token, TokenKind};
pub use parser::{parse_cypher, ParseError};
pub use writer::CypherWriter;
