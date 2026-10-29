//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, Result, SyntaxCopy};

syntax_struct!(crate::ast::AlterRoleStmt {
    name,
    attributes,
    connection_limit,
    membership_action,
    members
});

syntax_enum!(crate::ast::AlterRoutineKind {
    Function,
    Procedure,
    Routine
});

syntax_struct!(crate::ast::AlterRoutineOwnerStmt {
    kind,
    name,
    arg_types,
    arg_type_references,
    new_owner
});

syntax_struct!(crate::ast::AlterRoutineStmt {
    kind,
    name,
    arg_types,
    arg_type_references,
    volatility,
    strict,
    security_definer,
    leakproof,
    parallel,
    support,
    cost,
    rows,
    config_actions,
    attribute_clauses
});

syntax_struct!(crate::ast::CreateRoleStmt {
    name,
    attributes,
    connection_limit,
    in_roles,
    role_members,
    admin_members
});

syntax_struct!(crate::ast::DropRoleStmt { names, if_exists });

syntax_enum!(crate::ast::FunctionParallel {
    Unsafe,
    Restricted,
    Safe
});

syntax_struct!(crate::ast::GrantRoleStmt {
    granted_roles,
    grantee_roles,
    is_grant,
    options,
    grantor,
    cascade
});

syntax_struct!(crate::ast::GrantRoutineItem { name, arg_types });

syntax_struct!(crate::ast::GrantRoutineStmt {
    kind,
    is_grant,
    grant_option,
    grant_option_only,
    items,
    schemas,
    privileges,
    grantees,
    grantor,
    revoke_behavior
});

syntax_struct!(crate::ast::RenameRoleStmt { name, new_name });

syntax_struct!(crate::ast::RenameRoutineStmt {
    kind,
    name,
    arg_types,
    arg_type_references,
    new_name
});

syntax_enum!(crate::ast::RoleAttribute {
    Superuser,
    Inherit,
    CreateRole,
    CreateDb,
    Login,
    Replication,
    BypassRls
});

syntax_enum!(crate::ast::RoleMembershipAction { Add, Drop });

syntax_struct!(crate::ast::RoleMembershipOptions {
    admin,
    inherit,
    set
});

syntax_enum!(crate::ast::RoutineConfigAction {
    Set { name, value },
    FromCurrent { name },
    Reset { name },
    ResetAll
});

syntax_enum!(crate::ast::RoutinePrivilege {
    Execute,
    Unsupported(value0),
    ColumnsUnsupported
});

syntax_enum!(crate::ast::RoutineRevokeBehavior { Restrict, Cascade });

syntax_struct!(crate::ast::RoutineSecurityAttributes {
    security_definer,
    leakproof
});
