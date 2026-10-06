//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The `ALTER TABLE` statement and the actions it applies in order.

use serde::{Deserialize, Serialize};

use super::{
    default_true, AutoIncrementKind, ColumnDeclaration, ColumnDef, ColumnType, DeferredSQLError,
    EventEnableMode, Expr, ForeignKey, IdentitySequenceDeclaration, PartitionBound,
    RelationPersistence, RoleSpecification, SequenceDeclaration, TableCheck, TableKeyConstraint,
};

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
        #[serde(default)]
        checks: Vec<TableCheck>,
        #[serde(default)]
        foreign_keys: Vec<ForeignKey>,
        #[serde(default)]
        key_constraints: Vec<TableKeyConstraint>,
        if_not_exists: bool,
        /// The clauses the column writes, which `transformColumnDefinition` checks once the relation is found.
        #[serde(default)]
        declaration: ColumnDeclaration,
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
    SetPersistence {
        persistence: RelationPersistence,
    },
    ChangeOwner {
        owner: RoleSpecification,
    },
    SetSchema {
        schema: String,
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
        if_exists: bool,
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
    /// `ALTER COLUMN name ADD GENERATED { ALWAYS | BY DEFAULT } AS IDENTITY [ ( options ) ]`.
    AddIdentity {
        name: String,
        kind: AutoIncrementKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        declaration: Option<Box<IdentitySequenceDeclaration>>,
    },
    /// `ALTER COLUMN name` followed by `SET GENERATED { ALWAYS | BY DEFAULT }`, `RESTART [ [ WITH ] value ]` and `SET sequence_option` in any combination.
    SetIdentity {
        name: String,
        /// The generation `SET GENERATED` gives the column.
        kind: Option<AutoIncrementKind>,
        /// Whether `SET GENERATED` is repeated, which `PostgreSQL` reports after it has changed the sequence.
        repeated_kind: bool,
        /// The sequence options, kept as written: `PostgreSQL` reads them as `ALTER SEQUENCE` reads its options, and only for an identity column.
        #[serde(default)]
        sequence: SequenceDeclaration,
        /// The first error collecting the sequence options raised, which waits until they are read.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<DeferredSQLError>,
    },
    /// `ALTER COLUMN name DROP IDENTITY [ IF EXISTS ]`.
    DropIdentity {
        name: String,
        if_exists: bool,
    },
}
