//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::special::read_sequence_restart;
use super::{invalid, Input, InputError, JsonToken, Native, Parts, Stored, Values};
use crate::ast::relation_lifecycle::{
    AlterForeignTableAction, AlterSequence, AlterViewAction, AlterViewKind, AlterViewStmt,
    CreateSequence, DatabasePrivilege, DatabaseRevokeBehavior, DropColumnAction,
    DropConstraintAction, GrantDatabaseStmt, GrantSchemaStmt, GrantSequenceStmt,
    GrantSequenceTarget, GrantTableStmt, GrantTableTarget, OnCommitAction, RelationPersistence,
    SchemaPrivilege, SchemaRevokeBehavior, SequenceBound, SequenceDataType, SequenceLifecycle,
    SequenceOwnership, SequencePrivilege, SequenceRestart, SequenceRevokeBehavior, TablePrivilege,
    TablePrivilegeSpec, TableRevokeBehavior,
};

stored_enum!(AlterForeignTableAction {
    DropColumns (value),
    DropConstraints (value),
    OwnerTo (value),
    RenameTo (value),
    SetSchema (value),
});

stored_struct!(AlterSequence {
    name: required,
    if_exists: defaulted,
    restart: custom(read_sequence_restart),
    increment: optional,
    start: optional,
    data_type: defaulted,
    min_value: defaulted,
    max_value: defaulted,
    cycle: defaulted,
    cache_size: optional,
    ownership: defaulted,
    persistence: defaulted,
    role_owner: defaulted,
    lifecycle: defaulted,
});

stored_enum!(AlterViewAction {
    Set (value),
    Reset (value),
    OwnerTo (value),
    RenameTo (value),
    RenameColumn {
        from: required,
        to: required,
    },
    SetSchema (value),
});

stored_enum!(AlterViewKind {
    View,
    MaterializedView,
});

stored_struct!(AlterViewStmt {
    name: required,
    kind: required,
    if_exists: required,
    action: required,
});

stored_struct!(CreateSequence {
    name: required,
    if_not_exists: required,
    start: required,
    increment: required,
    persistence: defaulted,
    data_type: defaulted,
    min_value: defaulted,
    max_value: defaulted,
    cycle: defaulted,
    cache_size: default(1),
    ownership: defaulted,
    restart: defaulted,
});

stored_enum!(DatabasePrivilege {
    Connect,
    Create,
    Temporary,
    Unsupported (value),
});

stored_enum!(DatabaseRevokeBehavior { Restrict, Cascade });

stored_struct!(DropColumnAction {
    name: required,
    if_exists: required,
    cascade: required,
});

stored_struct!(DropConstraintAction {
    name: required,
    if_exists: required,
    cascade: required,
});

stored_struct!(GrantDatabaseStmt {
    is_grant: required,
    grant_option: required,
    grant_option_only: required,
    privileges: required,
    databases: required,
    grantees: required,
    grantor: defaulted,
    revoke_behavior: defaulted,
});

stored_struct!(GrantSchemaStmt {
    is_grant: required,
    grant_option: required,
    grant_option_only: required,
    privileges: required,
    schemas: required,
    grantees: required,
    grantor: defaulted,
    revoke_behavior: defaulted,
});

stored_struct!(GrantSequenceStmt {
    is_grant: required,
    grant_option: required,
    grant_option_only: required,
    privileges: required,
    target: required,
    grantees: required,
    grantor: defaulted,
    revoke_behavior: defaulted,
});

stored_enum!(GrantSequenceTarget {
    Sequences {
        names: required,
    },
    AllSequencesInSchemas {
        schemas: required,
    },
});

stored_struct!(GrantTableStmt {
    is_grant: required,
    grant_option: required,
    grant_option_only: required,
    privileges: required,
    target: required,
    grantees: required,
    grantor: defaulted,
    revoke_behavior: defaulted,
});

stored_enum!(GrantTableTarget {
    Relations {
        names: required,
    },
    AllTablesInSchemas {
        schemas: required,
    },
});

stored_enum!(OnCommitAction {
    PreserveRows,
    DeleteRows,
    Drop,
});

stored_enum!(RelationPersistence {
    Permanent,
    Unlogged,
    Temporary,
});

stored_enum!(SchemaPrivilege {
    Usage,
    Create,
    Unsupported (value),
});

stored_enum!(SchemaRevokeBehavior { Restrict, Cascade });

stored_enum!(SequenceBound {
    Unchanged,
    Default,
    Value (value),
});

stored_enum!(SequenceDataType {
    SmallInt,
    Integer,
    BigInt,
});

stored_enum!(SequenceLifecycle {
    Unchanged,
    RenameTo {
        name: required,
    },
    SetSchema {
        schema: required,
    },
});

stored_enum!(SequenceOwnership {
    Unchanged,
    Unowned,
    Column {
        table: required,
        column: required,
    },
});

stored_enum!(SequencePrivilege {
    Select,
    Update,
    Usage,
    ColumnsUnsupported,
    Unsupported (value),
});

stored_enum!(SequenceRestart {
    Unchanged,
    FromStart,
    With (value),
});

stored_enum!(SequenceRevokeBehavior { Restrict, Cascade });

stored_enum!(TablePrivilege {
    Select,
    Insert,
    Update,
    Delete,
    Truncate,
    References,
    Trigger,
    Maintain,
    Usage,
    Unsupported (value),
});

stored_struct!(TablePrivilegeSpec {
    privilege: required,
    columns: defaulted,
});

stored_enum!(TableRevokeBehavior { Restrict, Cascade });
