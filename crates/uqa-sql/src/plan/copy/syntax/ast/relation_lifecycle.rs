//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, Result, SyntaxCopy};

syntax_enum!(crate::ast::AlterForeignTableAction {
    DropColumns(value0),
    DropConstraints(value0),
    OwnerTo(value0),
    RenameTo(value0),
    SetSchema(value0)
});

syntax_struct!(crate::ast::AlterForeignTableStmt {
    name,
    if_exists,
    action
});

syntax_struct!(crate::ast::AlterSequence {
    name,
    if_exists,
    restart,
    increment,
    start,
    data_type,
    min_value,
    max_value,
    cycle,
    cache_size,
    ownership,
    persistence,
    role_owner,
    lifecycle
});

syntax_enum!(crate::ast::AlterViewAction {
    Set(value0),
    Reset(value0),
    OwnerTo(value0),
    RenameTo(value0),
    RenameColumn { from, to },
    SetSchema(value0)
});

syntax_enum!(crate::ast::AlterViewKind {
    View,
    MaterializedView
});

syntax_struct!(crate::ast::AlterViewStmt {
    name,
    kind,
    if_exists,
    action
});

syntax_struct!(crate::ast::CreateSequence {
    name,
    if_not_exists,
    start,
    increment,
    persistence,
    data_type,
    min_value,
    max_value,
    cycle,
    cache_size,
    ownership,
    restart
});

syntax_enum!(crate::ast::DatabasePrivilege {
    Connect,
    Create,
    Temporary,
    Unsupported(value0)
});

syntax_enum!(crate::ast::DatabaseRevokeBehavior { Restrict, Cascade });

syntax_struct!(crate::ast::DropColumnAction {
    name,
    if_exists,
    cascade
});

syntax_struct!(crate::ast::DropConstraintAction {
    name,
    if_exists,
    cascade
});

syntax_struct!(crate::ast::GrantDatabaseStmt {
    is_grant,
    grant_option,
    grant_option_only,
    privileges,
    databases,
    grantees,
    grantor,
    revoke_behavior
});

syntax_struct!(crate::ast::GrantSchemaStmt {
    is_grant,
    grant_option,
    grant_option_only,
    privileges,
    schemas,
    grantees,
    grantor,
    revoke_behavior
});

syntax_struct!(crate::ast::GrantSequenceStmt {
    is_grant,
    grant_option,
    grant_option_only,
    privileges,
    target,
    grantees,
    grantor,
    revoke_behavior
});

syntax_enum!(crate::ast::GrantSequenceTarget {
    Sequences { names },
    AllSequencesInSchemas { schemas }
});

syntax_struct!(crate::ast::GrantTableStmt {
    is_grant,
    grant_option,
    grant_option_only,
    privileges,
    target,
    grantees,
    grantor,
    revoke_behavior
});

syntax_enum!(crate::ast::GrantTableTarget {
    Relations { names },
    AllTablesInSchemas { schemas }
});

syntax_enum!(crate::ast::OnCommitAction {
    PreserveRows,
    DeleteRows,
    Drop
});

syntax_enum!(crate::ast::RelationPersistence {
    Permanent,
    Unlogged,
    Temporary
});

syntax_enum!(crate::ast::SchemaPrivilege {
    Usage,
    Create,
    Unsupported(value0)
});

syntax_enum!(crate::ast::SchemaRevokeBehavior { Restrict, Cascade });

syntax_enum!(crate::ast::SequenceBound {
    Unchanged,
    Default,
    Value(value0)
});

syntax_enum!(crate::ast::SequenceDataType {
    SmallInt,
    Integer,
    BigInt
});

syntax_enum!(crate::ast::SequenceLifecycle {
    Unchanged,
    RenameTo { name },
    SetSchema { schema }
});

syntax_enum!(crate::ast::SequenceOwnership {
    Unchanged,
    Unowned,
    Column { table, column }
});

syntax_enum!(crate::ast::SequencePrivilege {
    Select,
    Update,
    Usage,
    ColumnsUnsupported,
    Unsupported(value0)
});

syntax_enum!(crate::ast::SequenceRestart {
    Unchanged,
    FromStart,
    With(value0)
});

syntax_enum!(crate::ast::SequenceRevokeBehavior { Restrict, Cascade });

syntax_enum!(crate::ast::TablePrivilege {
    Select,
    Insert,
    Update,
    Delete,
    Truncate,
    References,
    Trigger,
    Maintain,
    Usage,
    Unsupported(value0)
});

syntax_struct!(crate::ast::TablePrivilegeSpec { privilege, columns });

syntax_enum!(crate::ast::TableRevokeBehavior { Restrict, Cascade });
