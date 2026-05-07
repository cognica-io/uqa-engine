//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Internal SQL AST. Lifts the relevant subset of the `libpg_query`
//! protobuf tree into a Rust enum the compiler walks. Statements not
//! yet supported parse cleanly but compile to
//! [`crate::SQLError::Unsupported`].

use serde::{Deserialize, Serialize};
use uqa_core::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ColumnType {
    Integer,
    Text,
    Real,
    /// `NUMERIC(precision, scale)` -- exact decimal storage. When
    /// `scale` is `Some(s)` the engine rounds `INSERT` values to `s`
    /// fractional digits. `precision` is captured for round-tripping
    /// the catalog text but is not currently enforced.
    Numeric {
        precision: Option<u32>,
        scale: Option<u32>,
    },
    /// `VECTOR(N)` columns store an `N`-dimensional `f32` embedding.
    Vector(u32),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct ColumnDef {
    pub name: String,
    pub ty: ColumnType,
    pub primary_key: bool,
    pub not_null: bool,
    /// `SERIAL` / `BIGSERIAL` columns auto-allocate from a per-table
    /// monotonic counter when the value is omitted from `INSERT`.
    #[serde(default)]
    pub auto_increment: bool,
    /// `UNIQUE` column constraint -- the engine rejects an INSERT
    /// whose value for this column already exists in another row.
    #[serde(default)]
    pub unique: bool,
    /// `DEFAULT <expr>`. Evaluated at INSERT time when the column is
    /// not present in the row tuple. Skipped from serde because
    /// `Expr` is not serializable; catalog reload re-parses the
    /// `CREATE TABLE` text from the catalog body.
    #[serde(skip)]
    pub default: Option<Expr>,
    /// `CHECK (<expr>)` column-level constraint. Evaluated at INSERT
    /// (and UPDATE-replace) time against the row being written.
    #[serde(skip)]
    pub check: Option<Expr>,
    /// `REFERENCES parent(col)` column-level FOREIGN KEY. The engine
    /// rejects INSERT / UPDATE whose value is not present in the
    /// referenced (table, column) pair.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub references: Option<ForeignKeyRef>,
}

/// `REFERENCES table(column)` reference target.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForeignKeyRef {
    pub table: String,
    pub column: String,
}

#[derive(Debug, Clone)]
pub struct CreateTable {
    pub name: String,
    pub columns: Vec<ColumnDef>,
    /// `CREATE TABLE IF NOT EXISTS` — silently ignore the statement
    /// when a table with this name already exists.
    pub if_not_exists: bool,
    /// Table-level `CHECK (...)` constraints. Each entry is an
    /// expression that must evaluate truthy against every row.
    #[allow(dead_code)]
    pub checks: Vec<TableCheck>,
    /// Table-level `FOREIGN KEY (col, ...) REFERENCES parent(col, ...)`.
    pub foreign_keys: Vec<ForeignKey>,
}

/// `CHECK (expr)` constraint with an optional name (`CONSTRAINT <name>
/// CHECK (...)`).
#[derive(Debug, Clone)]
pub struct TableCheck {
    pub name: Option<String>,
    pub expr: Expr,
}

/// Table-level foreign key. `local_columns.len()` matches
/// `ref_columns.len()`; the engine joins on the position-aligned
/// pairs.
#[derive(Debug, Clone)]
pub struct ForeignKey {
    pub name: Option<String>,
    pub local_columns: Vec<String>,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct CreateIndex {
    pub name: Option<String>,
    pub table: String,
    /// `gin`, `btree`, `ivf`, `rtree`, `hnsw`, ...
    pub access_method: String,
    pub columns: Vec<String>,
    /// `CREATE INDEX IF NOT EXISTS`.
    pub if_not_exists: bool,
    /// Storage parameters from `WITH (k = v, ...)`. Stored verbatim;
    /// known keys (`analyzer`, `lists`, `m`, `ef_construction`, ...)
    /// are interpreted by the engine.
    pub options: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct DropStmt {
    pub kind: DropKind,
    pub names: Vec<String>,
    pub if_exists: bool,
    pub cascade: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropKind {
    Table,
    Index,
    View,
    Schema,
}

#[derive(Debug, Clone)]
pub struct AlterTableStmt {
    pub table: String,
    pub if_exists: bool,
    pub action: AlterTableAction,
}

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum AlterTableAction {
    AddColumn {
        column: ColumnDef,
        if_not_exists: bool,
    },
    DropColumn {
        name: String,
        if_exists: bool,
        cascade: bool,
    },
    RenameColumn {
        from: String,
        to: String,
    },
    RenameTable {
        to: String,
    },
}

#[derive(Debug, Clone)]
pub struct InsertStmt {
    pub table: String,
    pub columns: Vec<String>,
    /// Inline `VALUES (...) (...)` rows. Empty when the statement is
    /// an `INSERT ... SELECT` form; in that case `select_source` is
    /// populated with the underlying SELECT.
    pub rows: Vec<Vec<ValueExpr>>,
    /// Populated when the statement is `INSERT INTO t (...) SELECT ...`.
    /// The engine materialises the inner select first and then writes
    /// each row through the standard INSERT path.
    pub select_source: Option<Box<SelectStmt>>,
    /// `ON CONFLICT (...) DO ...` clause. `None` for plain
    /// `INSERT INTO ... VALUES ...` without conflict handling.
    pub on_conflict: Option<OnConflict>,
}

#[derive(Debug, Clone)]
pub struct OnConflict {
    /// Conflict target columns parsed from the `ON CONFLICT (col, ...)`
    /// list. Empty when the clause uses `ON CONFLICT DO NOTHING` with
    /// no target.
    pub conflict_columns: Vec<String>,
    pub action: OnConflictAction,
}

#[derive(Debug, Clone)]
pub enum OnConflictAction {
    /// `DO NOTHING` -- skip conflicting rows silently.
    Nothing,
    /// `DO UPDATE SET col = expr [, ...] [WHERE pred]` -- apply the
    /// listed assignments to the existing row when the conflict
    /// target matches.
    Update {
        assignments: Vec<(String, Expr)>,
        r#where: Option<Expr>,
    },
}

#[derive(Debug, Clone)]
pub struct SelectStmt {
    pub projections: Vec<Projection>,
    pub from: Option<FromClause>,
    pub r#where: Option<Expr>,
    pub group_by: Vec<Expr>,
    /// Expanded GROUPING SETS / ROLLUP / CUBE specification. When
    /// non-empty the executor produces one row per grouping set;
    /// `group_by` is treated as a single grouping set in that case.
    /// Each inner Vec lists the grouping-key expressions for that
    /// set (an empty inner Vec means the global grand-total bucket).
    pub grouping_sets: Vec<Vec<Expr>>,
    /// `HAVING <expr>`. Evaluated against each aggregated row and
    /// filters out groups whose predicate is falsy. Mirrors PG's
    /// `havingClause`.
    pub having: Option<Expr>,
    pub order_by: Vec<OrderBy>,
    /// `LIMIT <expr>`. Stored as an expression so `LIMIT $1` and any
    /// other constant-folding integer expression resolves at execute
    /// time. `None` means no LIMIT clause was supplied.
    pub limit: Option<Expr>,
    /// `OFFSET <expr>`. Same shape as [`SelectStmt::limit`].
    pub offset: Option<Expr>,
    /// Common table expressions defined with `WITH [RECURSIVE] ...`.
    pub with: Vec<CTE>,
    /// Optional set operation: `Some` for UNION / INTERSECT / EXCEPT,
    /// with the right-hand operand as a sub-select.
    pub set_op: Option<Box<SetOp>>,
    /// `SELECT DISTINCT` -- de-duplicate the final result rows. Set by
    /// the compiler whenever the parsed `distinct_clause` is non-empty.
    pub distinct: bool,
}

#[derive(Debug, Clone)]
pub struct CTE {
    pub name: String,
    pub recursive: bool,
    pub query: Box<SelectStmt>,
}

#[derive(Debug, Clone)]
pub struct SetOp {
    pub kind: SetOpKind,
    pub all: bool,
    pub right: SelectStmt,
    /// `ORDER BY` applied to the combined `lhs <op> rhs` result.
    /// Distinct from the LHS / RHS branches' own `ORDER BY`.
    pub combined_order_by: Vec<OrderBy>,
    /// `LIMIT` applied to the combined result. `None` means no
    /// outer LIMIT clause was supplied.
    pub combined_limit: Option<Expr>,
    /// `OFFSET` applied to the combined result.
    pub combined_offset: Option<Expr>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetOpKind {
    Union,
    Intersect,
    Except,
}

#[derive(Debug, Clone)]
pub enum FromClause {
    /// `FROM <table> [AS <alias>]`.
    Table { name: String, alias: Option<String> },
    /// `FROM left <kind> right ON predicate`. `lateral` is true when
    /// the right side is a LATERAL subquery / function -- the engine
    /// re-evaluates it for every left row.
    Join {
        left: Box<FromClause>,
        right: Box<FromClause>,
        kind: JoinKind,
        on: Option<Expr>,
        #[allow(dead_code)]
        lateral: bool,
    },
    /// `FROM (VALUES (...)...) [AS <alias>(<col_aliases>)]`.
    Values {
        rows: Vec<Vec<Expr>>,
        alias: Option<String>,
        column_aliases: Vec<String>,
    },
    /// `FROM <fn>(<args>) [AS <alias>(<col_aliases>)]` -- e.g.
    /// `generate_series(1, 5)`, `unnest(arr)`, `regexp_split_to_table`,
    /// `json_each(...)`. The engine dispatches by name.
    Function {
        name: String,
        args: Vec<Expr>,
        alias: Option<String>,
        column_aliases: Vec<String>,
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

impl FromClause {
    /// All table names referenced under this clause, in declaration
    /// order. Used by the compiler to resolve unqualified column refs.
    pub fn collect_tables(&self, out: &mut Vec<(String, Option<String>)>) {
        match self {
            FromClause::Table { name, alias } => out.push((name.clone(), alias.clone())),
            FromClause::Join { left, right, .. } => {
                left.collect_tables(out);
                right.collect_tables(out);
            }
            FromClause::Values { alias, .. }
            | FromClause::Function { alias, .. }
            | FromClause::Subquery { alias, .. } => {
                if let Some(a) = alias {
                    out.push((a.clone(), Some(a.clone())));
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinKind {
    Inner,
    Left,
    Right,
    Full,
    Cross,
}

#[derive(Debug, Clone)]
pub struct Projection {
    pub expr: Expr,
    pub alias: Option<String>,
}

#[derive(Debug, Clone)]
pub struct OrderBy {
    pub expr: Expr,
    pub descending: bool,
    /// `NULLS FIRST` / `NULLS LAST` placement. `None` means the
    /// SQL-standard default — `NULLS LAST` for ASC and `NULLS FIRST`
    /// for DESC. Mirrors `PostgreSQL` semantics.
    pub nulls: Option<NullsOrder>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NullsOrder {
    First,
    Last,
}

/// `DISCARD` target. Mirrors `PostgreSQL`'s `DiscardMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscardTarget {
    All,
    Plans,
    Sequences,
    Temp,
}

#[derive(Debug, Clone)]
pub struct WindowSpec {
    pub partition_by: Vec<Expr>,
    pub order_by: Vec<OrderBy>,
    /// `ROWS` / `RANGE` frame, or `None` when not specified (defaults
    /// to `RANGE BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW`).
    pub frame: Option<WindowFrame>,
}

#[derive(Debug, Clone)]
pub struct WindowFrame {
    pub mode: FrameMode,
    pub start: FrameBound,
    pub end: FrameBound,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameMode {
    Rows,
    Range,
    Groups,
}

#[derive(Debug, Clone)]
pub enum FrameBound {
    UnboundedPreceding,
    UnboundedFollowing,
    CurrentRow,
    Preceding(Box<Expr>),
    Following(Box<Expr>),
}

/// Scalar expression nodes the compiler handles.
#[derive(Debug, Clone)]
pub enum Expr {
    Star,
    /// Unqualified column reference (`col`).
    Column(String),
    /// Qualified column reference (`table.col` or `alias.col`).
    QualifiedColumn {
        qualifier: String,
        column: String,
    },
    Literal(Value),
    /// A positional bind parameter (`$1`, `$2`, ...).
    Param(usize),
    /// `text_match(...)`, `knn_match(...)`, etc. — dispatched through
    /// the function registry.
    Func {
        name: String,
        args: Vec<Expr>,
        /// `func(DISTINCT expr)` — only meaningful for aggregate
        /// functions. Mirrors PostgreSQL's `agg_distinct`.
        distinct: bool,
        /// `func(expr ORDER BY ...)` — only meaningful for ordered
        /// aggregates (`STRING_AGG`, `ARRAY_AGG`, `PERCENTILE_*`).
        order_by: Vec<OrderBy>,
        /// `func(...) FILTER (WHERE expr)` — aggregate-level row filter.
        filter: Option<Box<Expr>>,
    },
    /// `ARRAY[1.0, 2.0, ...]` literal — currently restricted to numeric
    /// elements (vectors).
    Array(Vec<Expr>),
    /// `lhs op rhs` — comparison or arithmetic.
    Binary {
        op: BinaryOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    /// `NOT expr`.
    Not(Box<Expr>),
    /// `cond_1 AND cond_2 AND ...` (n-ary).
    And(Vec<Expr>),
    /// `cond_1 OR cond_2 OR ...` (n-ary).
    Or(Vec<Expr>),
    /// `expr IS NULL` / `expr IS NOT NULL`.
    IsNull {
        expr: Box<Expr>,
        negated: bool,
    },
    /// `expr BETWEEN low AND high`.
    Between {
        expr: Box<Expr>,
        low: Box<Expr>,
        high: Box<Expr>,
    },
    /// `expr IN (a, b, c)` literal list.
    InList {
        expr: Box<Expr>,
        list: Vec<Expr>,
        negated: bool,
    },
    /// `func(args) OVER (PARTITION BY ... ORDER BY ...)`.
    WindowCall {
        name: String,
        args: Vec<Expr>,
        spec: WindowSpec,
    },
    /// `CASE [base] WHEN cond THEN result ... [ELSE default] END`.
    /// `base` lifts simple-form `CASE expr WHEN val THEN ...` into an
    /// optional comparison anchor; searched-form `CASE WHEN cond ...`
    /// leaves it `None`.
    Case {
        base: Option<Box<Expr>>,
        when: Vec<(Expr, Expr)>,
        else_branch: Option<Box<Expr>>,
    },
    /// `CAST(expr AS type)`. The type name is preserved verbatim so
    /// the evaluator can apply the correct coercion.
    Cast {
        expr: Box<Expr>,
        ty: String,
    },
    /// `(SELECT ...)` scalar subquery: yields a single row / single
    /// column value at evaluation time.
    ScalarSubquery(Box<SelectStmt>),
    /// `EXISTS (SELECT ...)` -- truthy when the body produces at
    /// least one row.
    Exists {
        body: Box<SelectStmt>,
        negated: bool,
    },
    /// `expr [NOT] IN (SELECT ...)` set membership against a
    /// subquery. Evaluator runs the body once per top-level
    /// expression and tests membership.
    InSubquery {
        expr: Box<Expr>,
        body: Box<SelectStmt>,
        negated: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    Add,
    Subtract,
    Multiply,
    Divide,
}

/// `Expr` restricted to value-producing forms used by `INSERT` rows.
pub type ValueExpr = Expr;

#[derive(Debug, Clone)]
pub struct UpdateStmt {
    pub table: String,
    pub assignments: Vec<(String, Expr)>,
    pub r#where: Option<Expr>,
    /// `UPDATE t SET ... FROM other [JOIN ...]` -- the engine joins
    /// the target with this clause before applying the assignments.
    pub from: Option<FromClause>,
}

#[derive(Debug, Clone)]
pub struct DeleteStmt {
    pub table: String,
    pub r#where: Option<Expr>,
    /// `DELETE FROM t USING other [JOIN ...]` -- the engine joins
    /// the target with this clause and deletes target rows whose
    /// joined image satisfies WHERE.
    pub using: Option<FromClause>,
}

#[derive(Debug, Clone)]
pub enum Statement {
    CreateTable(CreateTable),
    CreateIndex(CreateIndex),
    Insert(InsertStmt),
    /// `SelectStmt` is the largest variant by far (CTEs + set-ops + n-ary
    /// expression trees), so we box it to keep the enum's stack footprint
    /// proportional to the smaller variants.
    Select(Box<SelectStmt>),
    Update(UpdateStmt),
    Delete(DeleteStmt),
    Drop(DropStmt),
    AlterTable(AlterTableStmt),
    /// `CREATE [OR REPLACE] VIEW name AS SELECT ...`. The body is the
    /// underlying `SelectStmt`; views are materialised lazily on every
    /// reference (no row caching).
    CreateView {
        name: String,
        body: Box<SelectStmt>,
        or_replace: bool,
    },
    /// `CREATE SCHEMA [IF NOT EXISTS] name`. Engine maps schemas onto
    /// optional table prefixes; this AST entry just records the
    /// command so callers can no-op or migrate as needed.
    CreateSchema {
        name: String,
        if_not_exists: bool,
    },
    /// `SET <name> [TO|=] <value>` — runtime parameter assignment.
    /// Currently the engine recognises `search_path`; everything else
    /// is recorded as a no-op for forward compatibility.
    SetVariable {
        name: String,
        value: String,
    },
    /// `SHOW <variable>` — return the runtime parameter as a single
    /// `(name -> value)` row. Mirrors Python `_compile_show`.
    ShowVariable {
        name: String,
    },
    /// `DISCARD [ALL|PLANS|SEQUENCES|TEMP|TEMPORARY]` — clear session
    /// state. Mirrors Python `_compile_discard`. The engine resets
    /// session vars, prepared statements and temp tables.
    Discard {
        target: DiscardTarget,
    },
    /// `EXPLAIN ...`. Carries the inner statement so the engine can
    /// emit the planner output. No-op when the engine does not have
    /// an EXPLAIN driver.
    Explain {
        analyze: bool,
        verbose: bool,
        format: Option<String>,
        body: Box<Statement>,
    },
    /// `ANALYZE [table]`. The engine refreshes per-column statistics
    /// for cardinality estimation; the AST simply records the target.
    Analyze {
        table: Option<String>,
    },
    /// `TRUNCATE TABLE t1, t2 ...`. Wipes the listed tables.
    Truncate {
        tables: Vec<String>,
        cascade: bool,
    },
    /// `BEGIN` / `COMMIT` / `ROLLBACK` / `SAVEPOINT name`.
    Transaction(TransactionStmt),
    /// `CREATE SEQUENCE name [START n] [INCREMENT n]`.
    CreateSequence(CreateSequence),
    /// `ALTER SEQUENCE name [RESTART [WITH n]] [INCREMENT [BY] n]
    /// [START [WITH] n]`.
    AlterSequence(AlterSequence),
    /// `CREATE TABLE name AS SELECT ...`.
    CreateTableAs {
        name: String,
        if_not_exists: bool,
        body: Box<SelectStmt>,
    },
    /// `PREPARE name AS <inner>`.
    Prepare {
        name: String,
        body: Box<Statement>,
    },
    /// `EXECUTE name (param1, param2, ...)`.
    Execute {
        name: String,
        params: Vec<Expr>,
    },
    /// `DEALLOCATE name | DEALLOCATE ALL`. `None` means ALL.
    Deallocate {
        name: Option<String>,
    },
    /// `SELECT * FROM (VALUES ...) [AS alias]` -- a standalone VALUES
    /// statement (also reachable from a SET-OP body).
    Values {
        rows: Vec<Vec<Expr>>,
    },
    /// `CREATE SERVER name FOREIGN DATA WRAPPER type OPTIONS (...)`.
    CreateForeignServer(CreateForeignServer),
    /// `CREATE FOREIGN TABLE name (...) SERVER server OPTIONS (...)`.
    CreateForeignTable(CreateForeignTable),
    /// `MERGE INTO target USING source ON cond WHEN MATCHED THEN ...
    /// WHEN NOT MATCHED THEN ...`. SQL:2003 conditional UPSERT.
    Merge(MergeStmt),
}

#[derive(Debug, Clone)]
pub struct MergeStmt {
    pub target: String,
    pub target_alias: Option<String>,
    pub source: FromClause,
    pub join_condition: Expr,
    pub when_clauses: Vec<MergeWhen>,
}

#[derive(Debug, Clone)]
pub enum MergeWhen {
    /// `WHEN MATCHED [AND <cond>] THEN UPDATE SET ...`.
    UpdateMatched {
        condition: Option<Expr>,
        assignments: Vec<(String, Expr)>,
    },
    /// `WHEN MATCHED [AND <cond>] THEN DELETE`.
    DeleteMatched { condition: Option<Expr> },
    /// `WHEN NOT MATCHED [AND <cond>] THEN INSERT (cols) VALUES (vals)`.
    InsertNotMatched {
        condition: Option<Expr>,
        columns: Vec<String>,
        values: Vec<Expr>,
    },
    /// `WHEN MATCHED [AND <cond>] THEN DO NOTHING`.
    NothingMatched { condition: Option<Expr> },
    /// `WHEN NOT MATCHED [AND <cond>] THEN DO NOTHING`.
    NothingNotMatched { condition: Option<Expr> },
}

#[derive(Debug, Clone)]
pub struct CreateForeignServer {
    pub name: String,
    pub fdw_type: String,
    pub options: Vec<(String, String)>,
    pub if_not_exists: bool,
}

#[derive(Debug, Clone)]
pub struct CreateForeignTable {
    pub name: String,
    pub server_name: String,
    pub columns: Vec<ColumnDef>,
    pub options: Vec<(String, String)>,
    pub if_not_exists: bool,
}

#[derive(Debug, Clone)]
pub struct CreateSequence {
    pub name: String,
    pub if_not_exists: bool,
    pub start: i64,
    pub increment: i64,
}

#[derive(Debug, Clone, Default)]
pub struct AlterSequence {
    pub name: String,
    /// `RESTART [WITH n]`. `Some(None)` for `RESTART` (uses `start`),
    /// `Some(Some(n))` for explicit value, `None` when not specified.
    pub restart: Option<Option<i64>>,
    pub increment: Option<i64>,
    pub start: Option<i64>,
}

#[derive(Debug, Clone)]
pub enum TransactionStmt {
    Begin,
    Commit,
    Rollback,
    Savepoint(String),
    ReleaseSavepoint(String),
    RollbackToSavepoint(String),
}
