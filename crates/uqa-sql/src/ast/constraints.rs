//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable table constraints, including `PostgreSQL` 18 temporal flags.

use serde::{Deserialize, Serialize};

use super::Expr;

/// `REFERENCES table(column)` reference target.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForeignKeyRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub table: String,
    pub column: String,
    #[serde(default)]
    pub on_update: ForeignKeyAction,
    #[serde(default)]
    pub on_delete: ForeignKeyAction,
    #[serde(default)]
    pub match_type: ForeignKeyMatch,
    #[serde(default = "default_true")]
    pub enforced: bool,
    /// `REFERENCES table (..., PERIOD column)` temporal coverage semantics.
    #[serde(default)]
    pub period: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TableKeyConstraintKind {
    PrimaryKey,
    Unique,
}

/// A table key whose columns are compared as one tuple.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableKeyConstraint {
    pub name: Option<String>,
    pub kind: TableKeyConstraintKind,
    pub columns: Vec<String>,
    /// `PostgreSQL` UNIQUE keys normally treat every NULL-containing tuple as
    /// distinct. `UNIQUE NULLS NOT DISTINCT` opts into NULL equality.
    #[serde(default)]
    pub nulls_not_distinct: bool,
    /// The final key column is a range or multirange compared by overlap.
    #[serde(default)]
    pub without_overlaps: bool,
}

/// Durable table-level constraints that do not fit in `ColumnDef`.
///
/// `serde(default)` on the catalog field containing this structure keeps
/// databases written before constraint persistence backward compatible.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TableConstraintSet {
    #[serde(default)]
    pub checks: Vec<TableCheck>,
    #[serde(default)]
    pub foreign_keys: Vec<ForeignKey>,
    #[serde(default)]
    pub key_constraints: Vec<TableKeyConstraint>,
}

/// `CHECK (expr)` constraint with an optional name (`CONSTRAINT <name>
/// CHECK (...)`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableCheck {
    pub name: Option<String>,
    pub expr: Expr,
    #[serde(default = "default_true")]
    pub enforced: bool,
}

/// Table-level foreign key. `local_columns.len()` matches
/// `ref_columns.len()`; the engine joins on the position-aligned pairs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForeignKey {
    pub name: Option<String>,
    pub local_columns: Vec<String>,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
    #[serde(default)]
    pub on_update: ForeignKeyAction,
    #[serde(default)]
    pub on_delete: ForeignKeyAction,
    /// Optional column subset for `ON DELETE SET NULL (...)` and
    /// `ON DELETE SET DEFAULT (...)`. Empty means every local FK
    /// column participates.
    #[serde(default)]
    pub on_delete_set_columns: Vec<String>,
    #[serde(default)]
    pub match_type: ForeignKeyMatch,
    #[serde(default = "default_true")]
    pub enforced: bool,
    /// The final local and referenced columns use `PostgreSQL` PERIOD coverage.
    #[serde(default)]
    pub period: bool,
}

const fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ForeignKeyAction {
    #[default]
    NoAction,
    Restrict,
    Cascade,
    SetNull,
    SetDefault,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ForeignKeyMatch {
    #[default]
    Simple,
    Full,
}
