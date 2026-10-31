//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, Result, SyntaxCopy};

syntax_enum!(crate::ast::AlterTableAction {
    AddInheritance { parent },
    DropInheritance { parent },
    AttachPartition { partition, bound },
    DetachPartition { partition, concurrently, finalize },
    AddColumn { column, checks, foreign_keys, key_constraints, if_not_exists, declaration },
    AddKeyConstraint { constraint },
    AddCheckConstraint { constraint },
    AddForeignKeyConstraint { constraint },
    AddNotNullConstraint { name, column, validated, no_inherit },
    ValidateConstraint { name },
    AlterConstraint { name, enforceability, deferrability, no_inherit },
    DropConstraint { name, if_exists, cascade },
    DropColumn { name, if_exists, cascade },
    RenameColumn { from, to },
    RenameTable { to },
    RenameTrigger { from, to },
    RenameConstraint { from, to },
    RenameRule { from, to },
    SetPersistence { persistence },
    ChangeOwner { owner },
    SetSchema { schema },
    SetTriggerEnableMode { name, user_only, mode },
    SetRuleEnableMode { name, mode },
    SetDefault { name, default },
    DropDefault { name },
    SetExpression { name, expression },
    DropExpression { name, if_exists },
    SetNotNull { name },
    DropNotNull { name },
    AlterColumnType { name, ty, collation, using },
    AddIdentity { name, kind, declaration },
    SetIdentity { name, kind, repeated_kind, sequence, error },
    DropIdentity { name, if_exists }
});

syntax_struct!(crate::ast::AlterTableStmt {
    table,
    qualifier,
    if_exists,
    recurse,
    actions
});
