//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use serde::{Deserialize, Serialize};

use super::{Expr, InternalRelationId, SelectStmt};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FromClause {
    /// `FROM <table> [AS <alias>]`.
    Table {
        /// Durable catalog identity, including an explicit schema when present.
        name: String,
        /// Relation name visible to SQL column binding before an alias is applied.
        qualifier: String,
        alias: Option<String>,
        /// Ordinary references include inheritance children; `ONLY table`
        /// clears this flag.
        #[serde(default = "default_include_descendants")]
        include_descendants: bool,
    },
    /// `FROM left <kind> right ON predicate`. `lateral` is true when
    /// the right side is a LATERAL subquery / function -- the engine
    /// re-evaluates it for every left row.
    Join {
        left: Box<FromClause>,
        right: Box<FromClause>,
        kind: JoinKind,
        /// Boolean qualification supplied by `ON`. This is mutually
        /// exclusive with `using` and `natural` in parser-produced trees.
        on: Option<Expr>,
        /// `PostgreSQL` `USING (column, ...) [AS alias]` metadata. The column
        /// list must remain explicit until both input row types are known so
        /// binding can validate each side and construct the merged output.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        using: Option<JoinUsing>,
        /// `NATURAL` derives its `USING` list from the visible columns of both
        /// input row types at binding time.
        #[serde(default)]
        natural: bool,
        /// Alias applied to the complete parenthesized JOIN result. When
        /// present, the input relation names are hidden from the enclosing
        /// query level.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        alias: Option<String>,
        /// Positional aliases for the JOIN output after USING/NATURAL shaping.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        column_aliases: Vec<String>,
        #[allow(dead_code)]
        lateral: bool,
    },
    /// `FROM (VALUES (...)...) [AS <alias>(<col_aliases>)]`.
    Values {
        rows: Vec<Vec<Expr>>,
        alias: Option<String>,
        column_aliases: Vec<String>,
        /// Opaque identity for an engine-injected, SQL-invisible VALUES row
        /// carrier. Parser-produced VALUES sources always leave this unset.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[doc(hidden)]
        internal_relation: Option<InternalRelationId>,
        /// Declared physical attribute types for an internal VALUES carrier;
        /// needed even when the carrier has zero rows.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        #[doc(hidden)]
        internal_column_types: Vec<Option<super::ColumnType>>,
    },
    /// `FROM <fn>(<args>) [AS <alias>(<col_aliases>)]` -- e.g.
    /// `generate_series(1, 5)`, `unnest(arr)`, `regexp_split_to_table`,
    /// `json_each(...)`, `cypher(...) AS (col agtype, ...)`. The engine
    /// dispatches by name.
    Function {
        name: String,
        /// Local function identifier used as `PostgreSQL`'s default output column label. Kept separate from the catalog-qualified lookup name so quoted identifiers containing `.` remain indivisible.
        output_name: String,
        /// Catalog relation bound to a relation-aware table function.
        /// Kept separate from scalar arguments so name resolution,
        /// dependency tracking, and planning never treat it as text data.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        relation: Option<String>,
        args: Vec<Expr>,
        alias: Option<String>,
        column_aliases: Vec<String>,
        /// Append `PostgreSQL`'s one-based `bigint` ordinality column after the function's ordinary output columns.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        ordinality: bool,
        /// Declared column types when the alias used a column
        /// definition list (`AS (col agtype, n int)`); empty when the
        /// alias only renamed columns. Type names are lowercased
        /// `PostgreSQL` internal names (`agtype`, `int4`, `text`, ...).
        #[serde(default)]
        column_types: Vec<String>,
    },
    /// One `PostgreSQL` range-function group. This represents explicit
    /// `ROWS FROM (...)` syntax and the parser transform of an unqualified
    /// multi-argument `unnest(a, b, ...)` into independent unary
    /// `pg_catalog.unnest` members. Members are evaluated independently and
    /// their result columns are concatenated in declaration order.
    FunctionGroup {
        functions: Vec<TableFunction>,
        /// Alias applied to the complete group rather than to an individual
        /// member.
        alias: Option<String>,
        /// Positional aliases for the concatenated group output.
        column_aliases: Vec<String>,
        /// Append one group-wide, one-based `bigint` ordinality column after
        /// every member's ordinary output columns.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        ordinality: bool,
    },
    /// `FROM (SELECT ...) AS <alias>` -- subquery as a relation.
    /// The body re-runs as if a CTE; the alias renames the result
    /// columns when supplied.
    Subquery {
        body: Box<SelectStmt>,
        alias: Option<String>,
        column_aliases: Vec<String>,
    },
}

const fn default_include_descendants() -> bool {
    true
}

/// One function inside a [`FromClause::FunctionGroup`].
///
/// A member owns its column definition list because `ROWS FROM` permits a
/// distinct `AS (name type, ...)` clause after each call. The range item's
/// relation alias, positional aliases, and ordinality remain on the enclosing
/// group.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableFunction {
    pub name: String,
    pub output_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relation: Option<String>,
    pub args: Vec<Expr>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub column_aliases: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub column_types: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinUsing {
    pub columns: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
}

impl FromClause {
    /// All table names referenced under this clause, in declaration
    /// order. Used by the compiler to resolve unqualified column refs.
    pub fn collect_tables(&self, out: &mut Vec<(String, Option<String>)>) {
        match self {
            FromClause::Table {
                name,
                qualifier,
                alias,
                ..
            } => out.push((
                name.clone(),
                Some(alias.as_ref().unwrap_or(qualifier).clone()),
            )),
            FromClause::Join { left, right, .. } => {
                left.collect_tables(out);
                right.collect_tables(out);
            }
            FromClause::Values { alias, .. }
            | FromClause::Function { alias, .. }
            | FromClause::FunctionGroup { alias, .. }
            | FromClause::Subquery { alias, .. } => {
                if let Some(a) = alias {
                    out.push((a.clone(), Some(a.clone())));
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum JoinKind {
    Inner,
    Left,
    Right,
    Full,
    Cross,
}
