//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Column and table constraint nodes shared by CREATE and ALTER TABLE.

use serde::{Deserialize, Serialize};

use super::{
    deserialize_auto_increment, AutoIncrement, ColumnType, Expr, GeneratedColumn, OnCommitAction,
    RelationPersistence, TableHierarchy,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct ColumnDef {
    pub name: String,
    pub ty: ColumnType,
    pub primary_key: bool,
    pub not_null: bool,
    /// Whether `NOT NULL` was declared as its own constraint instead of being
    /// implied by `PRIMARY KEY` or an auto-incrementing identity.
    #[serde(default)]
    pub not_null_explicit: bool,
    /// Durable `PostgreSQL` 18 `NOT NULL` constraint name. Parsing leaves an
    /// unnamed declaration as `None`; table registration assigns and persists
    /// `PostgreSQL`'s generated name before the constraint becomes visible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_null_name: Option<String>,
    /// Whether the named `NOT NULL` constraint has been validated against
    /// every pre-existing row. `NOT VALID` still enforces future writes.
    #[serde(default = "default_true")]
    pub not_null_validated: bool,
    /// Durable `NO INHERIT` state for `PostgreSQL` 18 named `NOT NULL`
    /// constraints.
    #[serde(default)]
    pub not_null_no_inherit: bool,
    /// Sequence provenance for `SERIAL` / `BIGSERIAL` and identity columns. The custom decoder accepts the legacy boolean representation written by releases that merged both SQL features into one table counter.
    #[serde(
        default,
        deserialize_with = "deserialize_auto_increment",
        skip_serializing_if = "Option::is_none"
    )]
    pub auto_increment: Option<AutoIncrement>,
    /// `UNIQUE` column constraint -- the engine rejects an INSERT
    /// whose value for this column already exists in another row.
    #[serde(default)]
    pub unique: bool,
    /// `DEFAULT <expr>`. Evaluated at INSERT time when the column is
    /// not present in the row tuple. Persisted in catalog metadata so
    /// reopened engines keep the same INSERT semantics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Expr>,
    /// `PostgreSQL` 18 generated-column definition. Stored values are refreshed
    /// on every row write; virtual values are evaluated from the physical row
    /// only when a logical row is read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated: Option<GeneratedColumn>,
    /// `CHECK (<expr>)` column-level constraint. Evaluated at INSERT
    /// (and UPDATE-replace) time against the row being written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<Expr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check_name: Option<String>,
    #[serde(default = "default_true")]
    pub check_enforced: bool,
    #[serde(default = "default_true")]
    pub check_validated: bool,
    #[serde(default)]
    pub check_no_inherit: bool,
    /// Column-level `REFERENCES parent[(col)]` foreign key. An omitted column is resolved to the referenced primary key before publication.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub references: Option<ForeignKeyRef>,
}

/// `REFERENCES table[(column)]` reference target.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct ForeignKeyRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub table: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<String>,
    #[serde(default)]
    pub on_update: ForeignKeyAction,
    #[serde(default)]
    pub on_delete: ForeignKeyAction,
    #[serde(default)]
    pub match_type: ForeignKeyMatch,
    #[serde(default = "default_true")]
    pub enforced: bool,
    #[serde(default = "default_true")]
    pub validated: bool,
    #[serde(default)]
    pub deferrable: bool,
    #[serde(default)]
    pub initially_deferred: bool,
    /// `REFERENCES table (..., PERIOD column)` temporal coverage semantics.
    #[serde(default)]
    pub period: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateTable {
    pub name: String,
    /// Local SQL relation identifier used while binding expressions declared inside the table definition.
    pub qualifier: String,
    pub columns: Vec<ColumnDef>,
    /// `CREATE TABLE IF NOT EXISTS` - silently ignore the statement
    /// when a table with this name already exists.
    pub if_not_exists: bool,
    /// Table-level `CHECK (...)` constraints. Each entry is an
    /// expression that must evaluate truthy against every row.
    #[allow(dead_code)]
    pub checks: Vec<TableCheck>,
    /// Table-level `FOREIGN KEY (col, ...) REFERENCES parent(col, ...)`.
    pub foreign_keys: Vec<ForeignKey>,
    /// Every declared `PRIMARY KEY` / `UNIQUE` constraint, including
    /// column-level declarations. Keeping the typed key (rather than only
    /// setting per-column flags) preserves composite-key and `NULLS NOT
    /// DISTINCT` semantics through planning and catalog persistence.
    #[serde(default)]
    pub key_constraints: Vec<TableKeyConstraint>,
    /// `PostgreSQL` relation persistence selected by `TEMPORARY` or `UNLOGGED`.
    #[serde(default)]
    pub persistence: RelationPersistence,
    /// Transaction-end behavior for temporary tables.
    #[serde(default)]
    pub on_commit: OnCommitAction,
    /// Direct inheritance and declarative-partitioning metadata. The engine
    /// resolves parent names and merges their row types atomically at create
    /// time, then persists the canonical hierarchy with the table schema.
    #[serde(default)]
    pub hierarchy: TableHierarchy,
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
    /// Stored alongside the table definition so reopen preserves `pg_class.relpersistence` for unlogged tables.
    #[serde(default)]
    pub persistence: RelationPersistence,
    /// Permanent and unlogged tables always use the default. Temporary tables are session-local and therefore never write this field to disk.
    #[serde(default)]
    pub on_commit: OnCommitAction,
    /// Durable relation hierarchy and partition-bound metadata.
    #[serde(default)]
    pub hierarchy: TableHierarchy,
}

/// `CHECK (expr)` constraint with an optional name (`CONSTRAINT <name>
/// CHECK (...)`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableCheck {
    pub name: Option<String>,
    pub expr: Expr,
    #[serde(default = "default_true")]
    pub enforced: bool,
    #[serde(default = "default_true")]
    pub validated: bool,
    #[serde(default)]
    pub no_inherit: bool,
}

/// Table-level foreign key. Compilation preserves an omitted referenced column list as empty; validation fills it from the primary key before publication.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
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
    #[serde(default = "default_true")]
    pub validated: bool,
    #[serde(default)]
    pub deferrable: bool,
    #[serde(default)]
    pub initially_deferred: bool,
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
