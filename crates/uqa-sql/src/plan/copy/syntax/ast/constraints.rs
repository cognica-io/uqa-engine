//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, Result, SyntaxCopy};

syntax_struct!(crate::ast::ColumnClause {
    kind,
    name,
    no_inherit
});

syntax_enum!(crate::ast::ColumnClauseKind {
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
    NotEnforced
});

syntax_struct!(crate::ast::ColumnDeclaration {
    collation,
    serial,
    serial_array,
    clauses
});

syntax_struct!(crate::ast::ColumnDef {
    name,
    ty,
    collation_oid,
    object_id,
    attribute_number,
    missing_value,
    primary_key,
    not_null,
    not_null_explicit,
    not_null_name,
    not_null_identity,
    not_null_validated,
    not_null_no_inherit,
    not_null_is_local,
    auto_increment,
    unique,
    default,
    generated,
    default_catalog_oid,
    check,
    check_name,
    check_enforced,
    check_validated,
    check_no_inherit,
    check_is_local,
    check_object_id,
    check_catalog_oid,
    references
});

syntax_struct!(crate::ast::CreateTable {
    name,
    qualifier,
    columns,
    if_not_exists,
    checks,
    foreign_keys,
    foreign_key_order,
    key_constraints,
    persistence,
    on_commit,
    hierarchy,
    check_order,
    element_order,
    not_null_declarations,
    untyped_columns
});

syntax_enum!(crate::ast::DeclaredCheck {
    Column(value0),
    Table(value0)
});

syntax_enum!(crate::ast::DeclaredElement {
    Column(value0),
    NotNull(value0),
    PrimaryKey { columns },
    DeferrableKey
});

syntax_enum!(crate::ast::DeclaredForeignKey {
    Column(value0),
    Table(value0)
});

syntax_struct!(crate::ast::DeferredCreateTable {
    name,
    persistence,
    definition_sql
});

syntax_struct!(crate::ast::DetachedPartitionConstraint { spec, bound });

syntax_struct!(crate::ast::ForeignKey {
    referenced_index,
    referenced_key,
    name,
    object_id,
    catalog_identity,
    local_columns,
    ref_table,
    ref_columns,
    on_update,
    on_delete,
    on_delete_set_columns,
    match_type,
    enforced,
    validated,
    deferrable,
    initially_deferred,
    period,
    referenced_partitions
});

syntax_enum!(crate::ast::ForeignKeyAction {
    NoAction,
    Restrict,
    Cascade,
    SetNull,
    SetDefault
});

syntax_enum!(crate::ast::ForeignKeyMatch { Simple, Full });

syntax_struct!(crate::ast::ForeignKeyRef {
    referenced_index,
    referenced_key,
    name,
    object_id,
    catalog_identity,
    table,
    column,
    on_update,
    on_delete,
    match_type,
    enforced,
    validated,
    deferrable,
    initially_deferred,
    period,
    referenced_partitions
});

syntax_struct!(crate::ast::NotNullDeclaration {
    column,
    name,
    no_inherit,
    explicit
});

syntax_struct!(crate::ast::TableCheck {
    name,
    object_id,
    catalog_oid,
    is_local,
    expr,
    enforced,
    validated,
    no_inherit,
    partition_constraint
});

syntax_struct!(crate::ast::TableKeyConstraint {
    catalog_identity,
    index_identity,
    name,
    kind,
    columns,
    included_columns,
    nulls_not_distinct,
    without_overlaps
});

syntax_enum!(crate::ast::TableKeyConstraintKind { PrimaryKey, Unique });
