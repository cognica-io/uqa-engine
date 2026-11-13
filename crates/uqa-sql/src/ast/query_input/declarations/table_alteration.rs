//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, Input, InputError, JsonToken, Native, Parts, Stored, Values};
use crate::ast::table_alteration::{AlterTableAction, AlterTableStmt};

stored_enum!(AlterTableAction {
    AddInheritance {
        parent: required,
    },
    DropInheritance {
        parent: required,
    },
    AttachPartition {
        partition: required,
        bound: required,
    },
    DetachPartition {
        partition: required,
        concurrently: required,
        finalize: required,
    },
    AddColumn {
        column: required,
        checks: defaulted,
        foreign_keys: defaulted,
        key_constraints: defaulted,
        if_not_exists: required,
        declaration: defaulted,
    },
    AddKeyConstraint {
        constraint: required,
    },
    AddCheckConstraint {
        constraint: required,
    },
    AddForeignKeyConstraint {
        constraint: required,
    },
    AddNotNullConstraint {
        name: optional,
        column: required,
        validated: required,
        no_inherit: required,
    },
    ValidateConstraint {
        name: required,
    },
    AlterConstraint {
        name: required,
        enforceability: optional,
        deferrability: optional,
        no_inherit: optional,
    },
    DropConstraint {
        name: required,
        if_exists: required,
        cascade: required,
    },
    DropColumn {
        name: required,
        if_exists: required,
        cascade: required,
    },
    RenameColumn {
        from: required,
        to: required,
    },
    RenameTable {
        to: required,
    },
    RenameTrigger {
        from: required,
        to: required,
    },
    RenameConstraint {
        from: required,
        to: required,
    },
    RenameRule {
        from: required,
        to: required,
    },
    SetPersistence {
        persistence: required,
    },
    ChangeOwner {
        owner: required,
    },
    SetSchema {
        schema: required,
    },
    SetTriggerEnableMode {
        name: optional,
        user_only: required,
        mode: required,
    },
    SetRuleEnableMode {
        name: required,
        mode: required,
    },
    SetDefault {
        name: required,
        default: required,
    },
    DropDefault {
        name: required,
    },
    SetExpression {
        name: required,
        expression: required,
    },
    DropExpression {
        name: required,
        if_exists: required,
    },
    SetNotNull {
        name: required,
    },
    DropNotNull {
        name: required,
    },
    AlterColumnType {
        name: required,
        ty: required,
        collation: defaulted,
        using: defaulted,
    },
    AddIdentity {
        name: required,
        kind: required,
        declaration: defaulted,
    },
    SetIdentity {
        name: required,
        kind: optional,
        repeated_kind: required,
        sequence: defaulted,
        error: defaulted,
    },
    DropIdentity {
        name: required,
        if_exists: required,
    },
});

stored_struct!(AlterTableStmt {
    table: required,
    qualifier: required,
    if_exists: required,
    recurse: default(true),
    actions: required,
});
