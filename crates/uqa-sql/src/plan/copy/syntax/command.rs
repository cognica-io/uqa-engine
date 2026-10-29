//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, Result, SyntaxCopy};

syntax_enum!(crate::plan::CommandPlan {
    CreateTable(value0),
    CreateTableIfNotExists(value0),
    CreateIndex(value0),
    RenameIndex(value0),
    Insert(value0),
    Update(value0),
    Delete(value0),
    Drop(value0),
    AlterTable(value0),
    AlterView(value0),
    CreateView { name, column_names, query, or_replace, persistence, options },
    CreateMaterializedView { name, column_names, if_not_exists, with_no_data, options, query },
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
    Explain { options, body, unbound_parameters },
    Analyze { table },
    Vacuum(value0),
    LockTable(value0),
    Truncate { tables, cascade, restart_identity },
    Transaction(value0),
    DeclareCursor { name, binary, scroll, hold, query },
    FetchCursor(value0),
    CloseCursor { name },
    CreateSequence(value0),
    CreateDomain(value0),
    AlterDomain(value0),
    CreateShellType(value0),
    CommentType(value0),
    CreateEnum(value0),
    CreateCompositeType(value0),
    AlterEnum(value0),
    AlterTypeObject(value0),
    GrantType(value0),
    AlterSequence(value0),
    CreateTableAs { name, if_not_exists, column_names, with_no_data, select_into, persistence, on_commit, query },
    Prepare { name, parameter_types, body },
    Execute { name, params },
    Deallocate { name },
    CreateForeignWrapper(value0),
    CreateForeignServer(value0),
    CreateForeignTable(value0),
    CreateForeignTableDefinition(value0),
    AlterForeignTable(value0),
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

impl SyntaxCopy for crate::plan::InsertPlan {
    fn copy_syntax(&self, copying: &mut Copying<'_>) -> Result<Self> {
        copying.insert(self)
    }
}

impl SyntaxCopy for crate::plan::UpdatePlan {
    fn copy_syntax(&self, copying: &mut Copying<'_>) -> Result<Self> {
        copying.update(self)
    }
}

impl SyntaxCopy for crate::plan::DeletePlan {
    fn copy_syntax(&self, copying: &mut Copying<'_>) -> Result<Self> {
        copying.delete(self)
    }
}

impl SyntaxCopy for crate::plan::MergePlan {
    fn copy_syntax(&self, copying: &mut Copying<'_>) -> Result<Self> {
        copying.merge(self)
    }
}
