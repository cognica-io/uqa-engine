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

mod constraints;
mod cte;
mod events;
mod expressions;
mod from;
mod function_binding;
mod locking;
mod ranges;
mod relation_hierarchy;
mod relation_lifecycle;
mod routine_security;
mod routines;
mod sequence;
mod types;

pub use constraints::*;
pub use cte::*;
pub use events::*;
pub use expressions::*;
pub use from::*;
pub use function_binding::*;
pub use locking::*;
pub use ranges::*;
pub use relation_hierarchy::*;
pub use relation_lifecycle::*;
pub use routine_security::*;
pub use routines::*;
pub use sequence::*;
pub use types::*;

const fn default_include_descendants() -> bool {
    true
}

const fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GeneratedColumnKind {
    Virtual,
    Stored,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedColumn {
    pub kind: GeneratedColumnKind,
    pub expression: Box<Expr>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub function_dependencies: Vec<GeneratedFunctionDependency>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateIndex {
    pub name: Option<String>,
    pub table: String,
    /// `gin`, `btree`, `ivf`, `hnsw`, `rtree`, ...
    pub access_method: String,
    pub columns: Vec<String>,
    /// `CREATE INDEX IF NOT EXISTS`.
    pub if_not_exists: bool,
    /// Storage parameters from `WITH (k = v, ...)`. Stored verbatim;
    /// known keys (`analyzer`, `lists`, `probes`, ...)
    /// are interpreted by the engine.
    pub options: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DropStmt {
    pub kind: DropKind,
    pub names: Vec<String>,
    pub if_exists: bool,
    pub cascade: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DropKind {
    Table,
    Index,
    View,
    MaterializedView,
    Schema,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlterTableStmt {
    pub table: String,
    /// Local SQL relation identifier used while binding new or replaced generation expressions.
    pub qualifier: String,
    pub if_exists: bool,
    /// Whether the target omitted `ONLY` and therefore allows recursive ALTER behavior.
    #[serde(default = "default_true")]
    pub recurse: bool,
    pub actions: Vec<AlterTableAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[expect(
    clippy::large_enum_variant,
    reason = "preserves the stable AST serde shape"
)]
pub enum AlterTableAction {
    AddInheritance {
        parent: String,
    },
    DropInheritance {
        parent: String,
    },
    AttachPartition {
        partition: String,
        bound: PartitionBound,
    },
    DetachPartition {
        partition: String,
        concurrently: bool,
        finalize: bool,
    },
    AddColumn {
        column: ColumnDef,
        if_not_exists: bool,
    },
    AddKeyConstraint {
        constraint: TableKeyConstraint,
    },
    AddCheckConstraint {
        constraint: TableCheck,
    },
    AddForeignKeyConstraint {
        constraint: ForeignKey,
    },
    AddNotNullConstraint {
        name: Option<String>,
        column: String,
        validated: bool,
        no_inherit: bool,
    },
    ValidateConstraint {
        name: String,
    },
    AlterConstraint {
        name: String,
        enforceability: Option<bool>,
        deferrability: Option<(bool, bool)>,
        no_inherit: Option<bool>,
    },
    DropConstraint {
        name: String,
        if_exists: bool,
        cascade: bool,
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
    RenameTrigger {
        from: String,
        to: String,
    },
    RenameConstraint {
        from: String,
        to: String,
    },
    RenameRule {
        from: String,
        to: String,
    },
    SetTriggerEnableMode {
        name: Option<String>,
        user_only: bool,
        mode: EventEnableMode,
    },
    SetRuleEnableMode {
        name: String,
        mode: EventEnableMode,
    },
    SetDefault {
        name: String,
        default: Expr,
    },
    DropDefault {
        name: String,
    },
    SetExpression {
        name: String,
        expression: Expr,
    },
    DropExpression {
        name: String,
    },
    SetNotNull {
        name: String,
    },
    DropNotNull {
        name: String,
    },
    AlterColumnType {
        name: String,
        ty: ColumnType,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        using: Option<Expr>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InsertStmt {
    pub table: String,
    /// SQL-visible target relation name: explicit alias, otherwise the local relation name.
    pub target_qualifier: String,
    #[serde(default = "default_include_descendants")]
    pub include_descendants: bool,
    pub columns: Vec<String>,
    /// Common table expressions defined with `WITH [RECURSIVE] ...`.
    pub with: Vec<CTE>,
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
    /// `RETURNING ...` projection list. Empty when absent.
    pub returning: Vec<Projection>,
    /// `PostgreSQL` 18 names for the old and new row images visible to
    /// `RETURNING`. The defaults are `old` and `new`.
    pub returning_aliases: ReturningAliases,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReturningAliases {
    pub old: String,
    pub new: String,
    #[serde(default)]
    pub old_explicit: bool,
    #[serde(default)]
    pub new_explicit: bool,
}

impl Default for ReturningAliases {
    fn default() -> Self {
        Self {
            old: "old".into(),
            new: "new".into(),
            old_explicit: false,
            new_explicit: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OnConflict {
    /// Conflict target columns parsed from the `ON CONFLICT (col, ...)`
    /// list. Empty when the clause uses `ON CONFLICT DO NOTHING` with
    /// no target.
    pub conflict_columns: Vec<String>,
    pub action: OnConflictAction,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SelectStmt {
    pub projections: Vec<Projection>,
    /// Rows owned by a `VALUES` query body. `PostgreSQL` represents `VALUES`
    /// through the same query node used for `SELECT`, so nested query bodies
    /// such as CTEs and set-operation branches must retain them here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<Vec<Expr>>,
    pub from: Option<FromClause>,
    pub r#where: Option<Expr>,
    pub group_by: Vec<Expr>,
    /// Expanded GROUPING SETS / ROLLUP / CUBE specification. When
    /// non-empty the executor produces one row per grouping set;
    /// `group_by` is treated as a single grouping set in that case.
    /// Each inner Vec lists the grouping-key expressions for that
    /// set (an empty inner Vec means the global grand-total bucket).
    pub grouping_sets: Vec<Vec<Expr>>,
    /// `GROUP BY DISTINCT` -- remove duplicate grouping sets after grouping expressions have been resolved against their input types.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub group_distinct: bool,
    /// `HAVING <expr>`. Evaluated against each aggregated row and
    /// filters out groups whose predicate is falsy. Mirrors PG's
    /// `havingClause`.
    pub having: Option<Expr>,
    pub order_by: Vec<OrderBy>,
    /// `LIMIT <expr>`. Stored as an expression so `LIMIT $1` and any
    /// other constant-folding integer expression resolves at execute
    /// time. `None` means no LIMIT clause was supplied.
    pub limit: Option<Expr>,
    /// `FETCH ... WITH TIES`. The row-count expression remains in [`Self::limit`]; this flag extends the boundary through every row whose complete `ORDER BY` key equals the last requested row.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub with_ties: bool,
    /// `OFFSET <expr>`. Same shape as [`SelectStmt::limit`].
    pub offset: Option<Expr>,
    /// Common table expressions defined with `WITH [RECURSIVE] ...`.
    pub with: Vec<CTE>,
    /// Optional set operation: `Some` for UNION / INTERSECT / EXCEPT.
    /// Parsed statements carry both operands in [`SetOp`]; `left` remains
    /// optional only for backward-compatible deserialization.
    pub set_op: Option<Box<SetOp>>,
    /// `SELECT DISTINCT` -- de-duplicate the final result rows. Set by
    /// the compiler whenever the parsed `distinct_clause` is non-empty.
    pub distinct: bool,
    /// `SELECT DISTINCT ON (<expr>, ...)` keys. Empty for plain
    /// `SELECT DISTINCT`.
    pub distinct_on: Vec<Expr>,
    /// `FOR { UPDATE | NO KEY UPDATE | SHARE | KEY SHARE }` row-locking clauses, in source order. Empty when the query does not lock rows.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub locking: Vec<LockingClause>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetOp {
    pub kind: SetOpKind,
    pub all: bool,
    /// Explicit left-hand subtree. Parsed set operations are left-associative,
    /// so a chain such as `a UNION b UNION c` carries `(a UNION b)` here
    /// instead of flattening it back to only `a`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left: Option<Box<SelectStmt>>,
    pub right: SelectStmt,
    /// `ORDER BY` applied to the combined `lhs <op> rhs` result.
    /// Distinct from the LHS / RHS branches' own `ORDER BY`.
    pub combined_order_by: Vec<OrderBy>,
    /// `LIMIT` applied to the combined result. `None` means no
    /// outer LIMIT clause was supplied.
    pub combined_limit: Option<Expr>,
    /// Whether the combined set-operation limit is `FETCH ... WITH TIES`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub combined_with_ties: bool,
    /// `OFFSET` applied to the combined result.
    pub combined_offset: Option<Expr>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SetOpKind {
    Union,
    Intersect,
    Except,
}

/// `DISCARD` target. Mirrors `PostgreSQL`'s `DiscardMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiscardTarget {
    All,
    Plans,
    Sequences,
    Temp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateStmt {
    pub table: String,
    pub target_qualifier: String,
    #[serde(default = "default_include_descendants")]
    pub include_descendants: bool,
    pub assignments: Vec<(String, Expr)>,
    pub r#where: Option<Expr>,
    /// Common table expressions defined with `WITH [RECURSIVE] ...`.
    pub with: Vec<CTE>,
    /// `UPDATE t SET ... FROM other [JOIN ...]` -- the engine joins
    /// the target with this clause before applying the assignments.
    pub from: Option<FromClause>,
    /// `RETURNING ...` projection list. Empty when absent.
    pub returning: Vec<Projection>,
    pub returning_aliases: ReturningAliases,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteStmt {
    pub table: String,
    pub target_qualifier: String,
    #[serde(default = "default_include_descendants")]
    pub include_descendants: bool,
    pub r#where: Option<Expr>,
    /// Common table expressions defined with `WITH [RECURSIVE] ...`.
    pub with: Vec<CTE>,
    /// `DELETE FROM t USING other [JOIN ...]` -- the engine joins
    /// the target with this clause and deletes target rows whose
    /// joined image satisfies WHERE.
    pub using: Option<FromClause>,
    /// `RETURNING ...` projection list. Empty when absent.
    pub returning: Vec<Projection>,
    pub returning_aliases: ReturningAliases,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetConstraintName {
    pub catalog: Option<String>,
    pub schema: Option<String>,
    pub name: String,
}

/// One parser-normalized `VACUUM` option. Keeping the parsed value in the SQL AST lets execution enforce `PostgreSQL`'s transaction-block error before validating command options.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VacuumOption {
    pub name: String,
    pub value: Option<VacuumOptionValue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VacuumOptionValue {
    Boolean(bool),
    Integer(i32),
    String(String),
}

/// One relation (and optional ANALYZE column list) named by `VACUUM`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VacuumTarget {
    pub catalog: Option<String>,
    pub table: String,
    #[serde(default = "default_include_descendants")]
    pub include_descendants: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VacuumStmt {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<VacuumOption>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<VacuumTarget>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    AlterViewOptions(AlterViewOptionsStmt),
    /// `CREATE [OR REPLACE] VIEW name [(column_name, ...)] AS SELECT ...`. The body is the underlying `SelectStmt`; views are materialised lazily on every reference (no row caching).
    CreateView {
        name: String,
        #[serde(default)]
        column_names: Vec<String>,
        body: Box<SelectStmt>,
        or_replace: bool,
        #[serde(default)]
        persistence: RelationPersistence,
        /// Validated `PostgreSQL` view reloptions in declaration order.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        options: Vec<(String, String)>,
    },
    /// `CREATE MATERIALIZED VIEW ... AS SELECT ... [WITH [NO] DATA]`.
    CreateMaterializedView {
        name: String,
        #[serde(default)]
        column_names: Vec<String>,
        #[serde(default)]
        if_not_exists: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        with_no_data: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        options: Vec<(String, String)>,
        body: Box<SelectStmt>,
    },
    /// `REFRESH MATERIALIZED VIEW [CONCURRENTLY] name [WITH [NO] DATA]`.
    RefreshMaterializedView {
        name: String,
        concurrently: bool,
        with_no_data: bool,
    },
    /// `CREATE SCHEMA [IF NOT EXISTS] name`. This AST entry records the
    /// command for the engine's durable schema catalog and namespace
    /// resolver.
    CreateSchema {
        name: String,
        if_not_exists: bool,
    },
    /// `SET <name> [TO|=] <value>` - runtime parameter assignment.
    /// The engine gives `search_path` resolution semantics and stores other
    /// parameters in the logical session for subsequent `SHOW` statements.
    SetVariable {
        name: String,
        value: String,
    },
    /// `RESET <name>` restores one runtime parameter to its session default.
    ResetVariable {
        name: String,
    },
    /// `RESET ALL` restores every resettable runtime parameter.
    ResetAllVariables,
    /// `SET CONSTRAINTS { ALL | name [, ...] } { DEFERRED | IMMEDIATE }`. An empty constraint list represents `ALL`; qualified names retain their SQL spelling so execution can apply schema-search semantics.
    SetConstraints {
        constraints: Vec<SetConstraintName>,
        deferred: bool,
    },
    /// `SHOW <variable>` - return the runtime parameter as one
    /// `(name -> value)` row.
    ShowVariable {
        name: String,
    },
    /// `DISCARD [ALL|PLANS|SEQUENCES|TEMP|TEMPORARY]` - clear session state.
    /// The engine resets session variables, prepared statements, sequence state, and the current session's temporary relations as requested.
    Discard {
        target: DiscardTarget,
    },
    /// `LOAD 'library'` - load a shared library into the session. The
    /// engine embeds its extension surface, so libraries it provides
    /// natively (Apache AGE) load as no-ops and unknown libraries fail
    /// like a missing `$libdir` file.
    Load {
        library: String,
    },
    /// `EXPLAIN ...`. Carries the inner statement so the engine can
    /// emit the planner output.
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
    /// `VACUUM [options] [relations]`. Execution enforces `PostgreSQL`'s transaction-block restriction before validating options and dispatching storage maintenance.
    Vacuum(VacuumStmt),
    /// `TRUNCATE TABLE t1, t2 ...`. Wipes the listed table hierarchies unless
    /// a target uses `ONLY`.
    Truncate {
        tables: Vec<TruncateTarget>,
        cascade: bool,
        #[serde(default)]
        restart_identity: bool,
    },
    /// `BEGIN` / `COMMIT` / `ROLLBACK` / `SAVEPOINT name`.
    Transaction(TransactionStmt),
    /// `DECLARE name [BINARY] [SCROLL] CURSOR [WITH HOLD] FOR query`.
    DeclareCursor(DeclareCursorStmt),
    /// `FETCH` or `MOVE` over a named SQL cursor.
    FetchCursor(FetchCursorStmt),
    /// `CLOSE name` or `CLOSE ALL`. `None` represents `ALL`.
    CloseCursor {
        name: Option<String>,
    },
    /// `CREATE SEQUENCE name [START n] [INCREMENT n]`.
    CreateSequence(CreateSequence),
    /// `ALTER SEQUENCE name [RESTART [WITH n]] [INCREMENT [BY] n]
    /// [START [WITH] n]`.
    AlterSequence(AlterSequence),
    /// `CREATE TABLE name AS SELECT ...`.
    CreateTableAs {
        name: String,
        if_not_exists: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        column_names: Vec<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        with_no_data: bool,
        #[serde(default)]
        persistence: RelationPersistence,
        #[serde(default)]
        on_commit: OnCommitAction,
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
    /// `CREATE [OR REPLACE] FUNCTION | PROCEDURE ...`. Boxed: the
    /// definition (parameters + body source) dwarfs other variants.
    CreateFunction(Box<CreateFunction>),
    /// `DROP FUNCTION | PROCEDURE [IF EXISTS] name[(args)] [, ...]`.
    DropFunction(DropFunctionStmt),
    /// `ALTER FUNCTION | PROCEDURE | ROUTINE name[(input_types)]` volatility and null-input attributes.
    AlterRoutine(AlterRoutineStmt),
    AlterRoutineOwner(AlterRoutineOwnerStmt),
    GrantRoutine(GrantRoutineStmt),
    GrantRole(GrantRoleStmt),
    CreateRole(CreateRoleStmt),
    AlterRole(AlterRoleStmt),
    DropRole(DropRoleStmt),
    /// `CREATE [OR REPLACE] TRIGGER ... ON relation`.
    CreateTrigger(CreateTrigger),
    /// `DROP TRIGGER [IF EXISTS] name ON relation`.
    DropTrigger(DropTrigger),
    /// `CREATE [OR REPLACE] RULE ... ON relation`.
    CreateRule(CreateRule),
    /// `DROP RULE [IF EXISTS] name ON relation`.
    DropRule(DropRule),
    /// `DO [LANGUAGE lang] $$ ... $$` - anonymous code block.
    DoBlock {
        language: String,
        body: String,
    },
    /// `CALL proc(args)` - procedure invocation. `OUT` / `INOUT`
    /// parameters shape the result row.
    Call {
        name: String,
        args: Vec<Expr>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TruncateTarget {
    pub table: String,
    #[serde(default = "default_include_descendants")]
    pub include_descendants: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeStmt {
    pub target: String,
    pub target_qualifier: String,
    pub target_alias: Option<String>,
    #[serde(default = "default_include_descendants")]
    pub include_descendants: bool,
    pub source: FromClause,
    pub join_condition: Expr,
    pub when_clauses: Vec<MergeWhen>,
    /// `MERGE ... RETURNING ...` projection list. Empty when absent.
    pub returning: Vec<Projection>,
    pub returning_aliases: ReturningAliases,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MergeWhen {
    /// `WHEN MATCHED [AND <cond>] THEN UPDATE SET ...`.
    UpdateMatched {
        condition: Option<Expr>,
        assignments: Vec<(String, Expr)>,
    },
    /// `WHEN MATCHED [AND <cond>] THEN DELETE`.
    DeleteMatched { condition: Option<Expr> },
    /// `WHEN NOT MATCHED BY SOURCE [AND <cond>] THEN UPDATE SET ...`.
    UpdateNotMatchedBySource {
        condition: Option<Expr>,
        assignments: Vec<(String, Expr)>,
    },
    /// `WHEN NOT MATCHED BY SOURCE [AND <cond>] THEN DELETE`.
    DeleteNotMatchedBySource { condition: Option<Expr> },
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
    /// `WHEN NOT MATCHED BY SOURCE [AND <cond>] THEN DO NOTHING`.
    NothingNotMatchedBySource { condition: Option<Expr> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateForeignServer {
    pub name: String,
    pub fdw_type: String,
    pub options: Vec<(String, String)>,
    pub if_not_exists: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateForeignTable {
    pub name: String,
    pub server_name: String,
    pub columns: Vec<ColumnDef>,
    pub options: Vec<(String, String)>,
    pub if_not_exists: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransactionIsolationLevel {
    ReadUncommitted,
    ReadCommitted,
    RepeatableRead,
    Serializable,
}

impl TransactionIsolationLevel {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReadUncommitted => "read uncommitted",
            Self::ReadCommitted => "read committed",
            Self::RepeatableRead => "repeatable read",
            Self::Serializable => "serializable",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransactionCharacteristics {
    pub isolation: Option<TransactionIsolationLevel>,
    pub read_only: Option<bool>,
    pub deferrable: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransactionStmt {
    Begin,
    BeginWithCharacteristics(TransactionCharacteristics),
    Commit,
    CommitAndChain,
    Rollback,
    RollbackAndChain,
    SetCharacteristics(TransactionCharacteristics),
    SetSessionCharacteristics(TransactionCharacteristics),
    SetSnapshot(String),
    Savepoint(String),
    ReleaseSavepoint(String),
    RollbackToSavepoint(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CursorDirection {
    Forward,
    Backward,
    Absolute,
    Relative,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeclareCursorStmt {
    pub name: String,
    pub binary: bool,
    /// `None` lets the query determine scrollability, while `Some(true)` and `Some(false)` represent explicit `SCROLL` and `NO SCROLL`.
    pub scroll: Option<bool>,
    pub hold: bool,
    pub query: Box<SelectStmt>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchCursorStmt {
    pub name: String,
    pub direction: CursorDirection,
    /// `PostgreSQL` uses `i64::MAX` for `ALL`; negative counts reverse `FORWARD` and `BACKWARD`.
    pub count: i64,
    pub move_only: bool,
}

#[cfg(test)]
mod tests;
