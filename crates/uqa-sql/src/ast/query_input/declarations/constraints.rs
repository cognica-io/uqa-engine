//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::special::read_auto_increment;
use super::{invalid, Input, InputError, JsonToken, Native, Parts, Stored, Values};
use crate::ast::constraints::{
    ColumnClause, ColumnClauseKind, ColumnDeclaration, ColumnDef, CreateTable, DeclaredCheck,
    DeclaredElement, DeclaredForeignKey, DeferredCreateTable, DetachedPartitionConstraint,
    ForeignKey, ForeignKeyAction, ForeignKeyMatch, ForeignKeyRef, NotNullDeclaration, TableCheck,
    TableKeyConstraint, TableKeyConstraintKind,
};

stored_struct!(ColumnClause {
    kind: required,
    name: optional,
    no_inherit: required,
});

stored_enum!(ColumnClauseKind {
    Null,
    NotNull,
    Default,
    Identity,
    Generated,
    Check,
    PrimaryKey,
    Unique,
    ForeignKey,
    Deferrable,
    NotDeferrable,
    InitiallyDeferred,
    InitiallyImmediate,
    Enforced,
    NotEnforced,
});

stored_struct!(ColumnDeclaration {
    collation: defaulted,
    serial: required,
    serial_array: required,
    clauses: required,
});

stored_struct!(ColumnDef {
    name: required,
    ty: required,
    collation_oid: defaulted,
    object_id: defaulted,
    attribute_number: defaulted,
    missing_value: defaulted,
    primary_key: required,
    not_null: required,
    not_null_explicit: defaulted,
    not_null_name: defaulted,
    not_null_identity: defaulted,
    not_null_validated: default(true),
    not_null_no_inherit: defaulted,
    not_null_is_local: default(true),
    auto_increment: custom(read_auto_increment),
    unique: defaulted,
    default: defaulted,
    generated: defaulted,
    default_catalog_oid: defaulted,
    check: defaulted,
    check_name: defaulted,
    check_enforced: default(true),
    check_validated: default(true),
    check_no_inherit: defaulted,
    check_is_local: default(true),
    check_object_id: defaulted,
    check_catalog_oid: defaulted,
    references: defaulted,
});

stored_struct!(CreateTable {
    name: required,
    qualifier: required,
    columns: required,
    if_not_exists: required,
    checks: required,
    foreign_keys: required,
    foreign_key_order: defaulted,
    key_constraints: defaulted,
    persistence: defaulted,
    on_commit: defaulted,
    hierarchy: defaulted,
    check_order: defaulted,
    element_order: defaulted,
    not_null_declarations: defaulted,
    untyped_columns: defaulted,
});

stored_enum!(DeclaredCheck {
    Column (value),
    Table (value),
});

stored_enum!(DeclaredElement {
    Column (value),
    NotNull (value),
    PrimaryKey {
        columns: required,
    },
    DeferrableKey,
});

stored_enum!(DeclaredForeignKey {
    Column (value),
    Table (value),
});

stored_struct!(DeferredCreateTable {
    name: required,
    persistence: required,
    definition_sql: required,
});

stored_struct!(DetachedPartitionConstraint {
    spec: required,
    bound: required,
});

stored_struct!(ForeignKey {
    referenced_index: defaulted,
    referenced_key: defaulted,
    name: optional,
    object_id: defaulted,
    catalog_identity: defaulted,
    local_columns: required,
    ref_table: required,
    ref_columns: required,
    on_update: defaulted,
    on_delete: defaulted,
    on_delete_set_columns: defaulted,
    match_type: defaulted,
    enforced: default(true),
    validated: default(true),
    deferrable: defaulted,
    initially_deferred: defaulted,
    period: defaulted,
    referenced_partitions: defaulted,
});

stored_enum!(ForeignKeyAction {
    NoAction,
    Restrict,
    Cascade,
    SetNull,
    SetDefault,
});

stored_enum!(ForeignKeyMatch { Simple, Full });

stored_struct!(ForeignKeyRef {
    referenced_index: defaulted,
    referenced_key: defaulted,
    name: defaulted,
    object_id: defaulted,
    catalog_identity: defaulted,
    table: required,
    column: defaulted,
    on_update: defaulted,
    on_delete: defaulted,
    match_type: defaulted,
    enforced: default(true),
    validated: default(true),
    deferrable: defaulted,
    initially_deferred: defaulted,
    period: defaulted,
    referenced_partitions: defaulted,
});

stored_struct!(NotNullDeclaration {
    column: required,
    name: optional,
    no_inherit: required,
    explicit: required,
});

stored_struct!(TableCheck {
    name: optional,
    object_id: defaulted,
    catalog_oid: defaulted,
    is_local: default(true),
    expr: required,
    enforced: default(true),
    validated: default(true),
    no_inherit: defaulted,
    partition_constraint: defaulted,
});

stored_struct!(TableKeyConstraint {
    catalog_identity: defaulted,
    index_identity: defaulted,
    name: optional,
    kind: required,
    columns: required,
    included_columns: defaulted,
    nulls_not_distinct: defaulted,
    without_overlaps: defaulted,
});

stored_enum!(TableKeyConstraintKind { PrimaryKey, Unique });
