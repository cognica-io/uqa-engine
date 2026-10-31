//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, Result, SyntaxCopy};

syntax_struct!(crate::ast::CreateForeignServer {
    name,
    fdw_type,
    server_type,
    version,
    options,
    if_not_exists
});

syntax_struct!(crate::ast::CreateForeignTable {
    name,
    server_name,
    columns,
    checks,
    not_null_declarations,
    options,
    if_not_exists
});

syntax_struct!(crate::ast::CreateIndex {
    included_columns,
    column_order,
    predicate,
    name,
    table,
    access_method,
    columns,
    unique,
    nulls_not_distinct,
    if_not_exists,
    options,
    option_namespaces
});

syntax_enum!(crate::ast::CursorDirection {
    Forward,
    Backward,
    Absolute,
    Relative
});

syntax_struct!(crate::ast::DeclareCursorStmt {
    name,
    binary,
    scroll,
    hold,
    query
});

syntax_struct!(crate::ast::DeferredCreateForeignTable {
    name,
    server_name,
    definition_sql,
    if_not_exists
});

syntax_struct!(crate::ast::DeleteStmt {
    table,
    target_relation_bound,
    target_qualifier,
    target_alias,
    include_descendants,
    r#where,
    with,
    using,
    returning,
    returning_aliases
});

syntax_enum!(crate::ast::DiscardTarget {
    All,
    Plans,
    Sequences,
    Temp
});

syntax_enum!(crate::ast::DropKind {
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
    ForeignServer
});

syntax_struct!(crate::ast::DropStmt {
    kind,
    names,
    if_exists,
    cascade
});

syntax_struct!(crate::ast::FetchCursorStmt {
    name,
    direction,
    count,
    move_only
});

syntax_struct!(crate::ast::GeneratedColumn {
    kind,
    expression,
    function_dependencies
});

syntax_enum!(crate::ast::GeneratedColumnKind { Virtual, Stored });

syntax_struct!(crate::ast::IndexColumnOrder {
    descending,
    nulls_first
});

syntax_struct!(crate::ast::InsertStmt {
    table,
    target_relation_bound,
    target_qualifier,
    target_alias,
    include_descendants,
    columns,
    overriding,
    with,
    rows,
    select_source,
    on_conflict,
    returning,
    returning_aliases
});

syntax_struct!(crate::ast::MergeStmt {
    with,
    target,
    target_qualifier,
    target_alias,
    target_column_bindings,
    include_descendants,
    source,
    join_condition,
    when_clauses,
    returning,
    returning_aliases
});

syntax_struct!(crate::ast::MergeTargetColumnBinding {
    object_id,
    attribute_number,
    domain_dependencies
});

syntax_enum!(crate::ast::MergeWhen {
    UpdateMatched { condition, assignments },
    DeleteMatched { condition },
    UpdateNotMatchedBySource { condition, assignments },
    DeleteNotMatchedBySource { condition },
    InsertNotMatched { condition, columns, overriding, values },
    NothingMatched { condition },
    NothingNotMatched { condition },
    NothingNotMatchedBySource { condition }
});

syntax_struct!(crate::ast::OnConflict {
    predicate,
    constraint,
    conflict_columns,
    expressions,
    action
});

syntax_enum!(crate::ast::OnConflictAction {
    Nothing,
    Update { assignments, r#where }
});

syntax_struct!(crate::ast::ReturningAliases {
    old,
    new,
    old_explicit,
    new_explicit
});

syntax_struct!(crate::ast::SelectStmt {
    windows,
    projections,
    values,
    from,
    r#where,
    group_by,
    grouping_sets,
    group_distinct,
    having,
    order_by,
    limit,
    with_ties,
    offset,
    with,
    set_op,
    distinct,
    distinct_on,
    locking
});

syntax_struct!(crate::ast::SetConstraintName {
    catalog,
    schema,
    name
});

syntax_struct!(crate::ast::SetOp {
    kind,
    all,
    left,
    right,
    combined_order_by,
    combined_limit,
    combined_with_ties,
    combined_offset
});

syntax_enum!(crate::ast::SetOpKind {
    Union,
    Intersect,
    Except
});

syntax_enum!(crate::ast::Statement {
    CreateDomain(value0),
    AlterDomain(value0),
    CreateShellType(value0),
    CommentType(value0),
    CreateEnum(value0),
    CreateCompositeType(value0),
    AlterEnum(value0),
    AlterTypeObject(value0),
    GrantType(value0),
    CreateTable(value0),
    CreateTableIfNotExists(value0),
    CreateIndex(value0),
    RenameIndex(value0),
    Insert(value0),
    Select(value0),
    Update(value0),
    Delete(value0),
    Drop(value0),
    AlterTable(value0),
    AlterForeignTable(value0),
    AlterView(value0),
    CreateView { name, column_names, body, or_replace, persistence, options },
    CreateMaterializedView { name, column_names, if_not_exists, with_no_data, options, body },
    RefreshMaterializedView { name, concurrently, with_no_data },
    CreateSchema { name, if_not_exists, authorization },
    AlterSchemaOwner { name, new_owner },
    RenameSchema { name, new_name },
    Notify { channel, payload },
    Listen { channel },
    Unlisten { channel },
    SetVariable { name, value, local, is_default },
    ResetVariable { name },
    ResetAllVariables,
    SetConstraints { constraints, deferred },
    ShowVariable { name },
    Discard { target },
    Load { library },
    Explain { options, body },
    Analyze { table },
    Vacuum(value0),
    LockTable(value0),
    Truncate { tables, cascade, restart_identity },
    Transaction(value0),
    DeclareCursor(value0),
    FetchCursor(value0),
    CloseCursor { name },
    CreateSequence(value0),
    AlterSequence(value0),
    CreateTableAs { name, if_not_exists, column_names, with_no_data, select_into, persistence, on_commit, body },
    Prepare { name, parameter_types, body },
    Execute { name, params },
    Deallocate { name },
    Values { rows },
    CreateForeignWrapper(value0),
    CreateForeignServer(value0),
    CreateForeignTable(value0),
    CreateForeignTableDefinition(value0),
    Merge(value0),
    CreateFunction(value0),
    DropFunction(value0),
    AlterRoutine(value0),
    AlterRoutineOwner(value0),
    RenameRoutine(value0),
    GrantRoutine(value0),
    GrantTable(value0),
    GrantSequence(value0),
    GrantDatabase(value0),
    GrantSchema(value0),
    GrantRole(value0),
    CreateRole(value0),
    AlterRole(value0),
    RenameRole(value0),
    DropRole(value0),
    CreateTrigger(value0),
    DropTrigger(value0),
    CreateRule(value0),
    DropRule(value0),
    DoBlock { language, body },
    Call { name, args },
    Empty
});

syntax_struct!(crate::ast::TransactionCharacteristics {
    isolation,
    read_only,
    deferrable
});

syntax_enum!(crate::ast::TransactionIsolationLevel {
    ReadUncommitted,
    ReadCommitted,
    RepeatableRead,
    Serializable
});

syntax_enum!(crate::ast::TransactionStmt {
    Begin,
    BeginWithCharacteristics(value0),
    Commit,
    CommitAndChain,
    Rollback,
    RollbackAndChain,
    SetCharacteristics(value0),
    SetSessionCharacteristics(value0),
    SetSnapshot(value0),
    Savepoint(value0),
    ReleaseSavepoint(value0),
    RollbackToSavepoint(value0)
});

syntax_struct!(crate::ast::TruncateTarget {
    table,
    include_descendants
});

syntax_struct!(crate::ast::UpdateStmt {
    table,
    target_relation_bound,
    target_qualifier,
    target_alias,
    include_descendants,
    assignments,
    r#where,
    with,
    from,
    returning,
    returning_aliases
});

syntax_struct!(crate::ast::VacuumOption { name, value });

syntax_enum!(crate::ast::VacuumOptionValue {
    Boolean(value0),
    Integer(value0),
    String(value0)
});

syntax_struct!(crate::ast::VacuumStmt { options, targets });

syntax_struct!(crate::ast::VacuumTarget {
    catalog,
    table,
    include_descendants,
    columns
});
