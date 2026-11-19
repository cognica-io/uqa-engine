//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::special::read_explain;
use super::{invalid, Input, InputError, JsonToken, Native, Parts, Stored, Values};
use crate::ast::{
    CreateForeignServer, CreateForeignTable, CreateIndex, CursorDirection, DeclareCursorStmt,
    DeferredCreateForeignTable, DiscardTarget, DropKind, DropStmt, FetchCursorStmt,
    GeneratedColumn, GeneratedColumnKind, IndexColumnOrder, SetConstraintName, Statement,
    TransactionCharacteristics, TransactionIsolationLevel, TransactionStmt, TruncateTarget,
    VacuumOption, VacuumOptionValue, VacuumStmt, VacuumTarget,
};

stored_struct!(CreateForeignServer {
    name: required,
    fdw_type: required,
    server_type: defaulted,
    version: defaulted,
    options: required,
    if_not_exists: required,
});

stored_struct!(CreateForeignTable {
    name: required,
    server_name: required,
    columns: required,
    checks: defaulted,
    not_null_declarations: defaulted,
    options: required,
    if_not_exists: required,
});

stored_struct!(CreateIndex {
    included_columns: defaulted,
    column_order: defaulted,
    predicate: defaulted,
    name: optional,
    table: required,
    access_method: required,
    columns: required,
    unique: defaulted,
    nulls_not_distinct: defaulted,
    if_not_exists: required,
    options: required,
    option_namespaces: defaulted,
});

stored_enum!(CursorDirection {
    Forward,
    Backward,
    Absolute,
    Relative,
});

stored_struct!(DeclareCursorStmt {
    name: required,
    binary: required,
    scroll: optional,
    hold: required,
    query: required,
});

stored_struct!(DeferredCreateForeignTable {
    name: required,
    server_name: required,
    definition_sql: required,
    if_not_exists: default(true),
});

stored_enum!(DiscardTarget {
    All,
    Plans,
    Sequences,
    Temp,
});

stored_enum!(DropKind {
    ForeignWrapper,
    Table,
    ForeignTable,
    Index,
    View,
    MaterializedView,
    Schema,
    Sequence,
    Domain,
    Type,
    ForeignServer,
});

stored_struct!(DropStmt {
    kind: required,
    names: required,
    if_exists: required,
    cascade: required,
});

stored_struct!(FetchCursorStmt {
    name: required,
    direction: required,
    count: required,
    move_only: required,
});

stored_struct!(GeneratedColumn {
    kind: required,
    expression: required,
    function_dependencies: defaulted,
});

stored_enum!(GeneratedColumnKind { Virtual, Stored });

stored_struct!(IndexColumnOrder {
    descending: required,
    nulls_first: required,
});

stored_struct!(SetConstraintName {
    catalog: optional,
    schema: optional,
    name: required,
});

stored_enum!(Statement {
    CreateDomain (value),
    AlterDomain (value),
    CreateShellType (value),
    CommentType (value),
    CreateEnum (value),
    CreateCompositeType (value),
    AlterEnum (value),
    AlterTypeObject (value),
    GrantType (value),
    CreateTable (value),
    CreateTableIfNotExists (value),
    CreateIndex (value),
    RenameIndex (value),
    Insert (value),
    Select (value),
    Update (value),
    Delete (value),
    Drop (value),
    AlterTable (value),
    AlterForeignTable (value),
    AlterView (value),
    CreateView {
        name: required,
        column_names: defaulted,
        body: required,
        or_replace: required,
        persistence: defaulted,
        options: defaulted,
    },
    CreateMaterializedView {
        name: required,
        column_names: defaulted,
        if_not_exists: defaulted,
        with_no_data: defaulted,
        options: defaulted,
        body: required,
    },
    RefreshMaterializedView {
        name: required,
        concurrently: required,
        with_no_data: required,
    },
    CreateSchema {
        name: optional,
        if_not_exists: required,
        authorization: defaulted,
    },
    AlterSchemaOwner {
        name: required,
        new_owner: required,
    },
    RenameSchema {
        name: required,
        new_name: required,
    },
    Notify {
        channel: required,
        payload: required,
    },
    Listen {
        channel: required,
    },
    Unlisten {
        channel: optional,
    },
    SetVariable {
        name: required,
        value: required,
        local: defaulted,
        is_default: defaulted,
    },
    ResetVariable {
        name: required,
    },
    ResetAllVariables,
    SetConstraints {
        constraints: required,
        deferred: required,
    },
    ShowVariable {
        name: required,
    },
    Discard {
        target: required,
    },
    Load {
        library: required,
    },
    Explain => read_explain,
    Analyze {
        table: optional,
    },
    Vacuum (value),
    LockTable (value),
    Truncate {
        tables: required,
        cascade: required,
        restart_identity: defaulted,
    },
    Transaction (value),
    DeclareCursor (value),
    FetchCursor (value),
    CloseCursor {
        name: optional,
    },
    CreateSequence (value),
    AlterSequence (value),
    CreateTableAs {
        name: required,
        if_not_exists: required,
        column_names: defaulted,
        with_no_data: defaulted,
        select_into: defaulted,
        persistence: defaulted,
        on_commit: defaulted,
        body: required,
    },
    Prepare {
        name: required,
        parameter_types: defaulted,
        body: required,
    },
    Execute {
        name: required,
        params: required,
    },
    Deallocate {
        name: optional,
    },
    Values {
        rows: required,
    },
    CreateForeignWrapper (value),
    CreateForeignServer (value),
    CreateForeignTable (value),
    CreateForeignTableDefinition ["CreateForeignTableIfNotExists"] (value),
    Merge (value),
    CreateFunction (value),
    DropFunction (value),
    AlterRoutine (value),
    AlterRoutineOwner (value),
    RenameRoutine (value),
    GrantRoutine (value),
    GrantTable (value),
    GrantSequence (value),
    GrantDatabase (value),
    GrantSchema (value),
    GrantRole (value),
    CreateRole (value),
    AlterRole (value),
    RenameRole (value),
    DropRole (value),
    CreateTrigger (value),
    DropTrigger (value),
    CreateRule (value),
    DropRule (value),
    DoBlock {
        language: required,
        body: required,
    },
    Call {
        name: required,
        args: required,
    },
    Empty,
});

stored_struct!(TransactionCharacteristics {
    isolation: optional,
    read_only: optional,
    deferrable: optional,
});

stored_enum!(TransactionIsolationLevel {
    ReadUncommitted,
    ReadCommitted,
    RepeatableRead,
    Serializable,
});

stored_enum!(TransactionStmt {
    Begin,
    BeginWithCharacteristics (value),
    Commit,
    CommitAndChain,
    Rollback,
    RollbackAndChain,
    SetCharacteristics (value),
    SetSessionCharacteristics (value),
    SetSnapshot (value),
    Savepoint (value),
    ReleaseSavepoint (value),
    RollbackToSavepoint (value),
});

stored_struct!(TruncateTarget {
    table: required,
    include_descendants: default(true),
});

stored_struct!(VacuumOption {
    name: required,
    value: optional,
});

stored_enum!(VacuumOptionValue {
    Boolean (value),
    Integer (value),
    String (value),
});

stored_struct!(VacuumStmt {
    options: defaulted,
    targets: defaulted,
});

stored_struct!(VacuumTarget {
    catalog: optional,
    table: required,
    include_descendants: default(true),
    columns: defaulted,
});
