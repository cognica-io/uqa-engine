//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::special::read_estimate;
use super::{invalid, Input, InputError, JsonToken, Native, Parts, Stored, Values};
use crate::ast::routine_security::{
    AlterRoleStmt, AlterRoutineKind, AlterRoutineOwnerStmt, AlterRoutineStmt, CreateRoleStmt,
    DropRoleStmt, FunctionParallel, GrantRoleStmt, GrantRoutineItem, GrantRoutineStmt,
    RenameRoleStmt, RenameRoutineStmt, RoleAttribute, RoleMembershipAction, RoleMembershipOptions,
    RoutineConfigAction, RoutinePrivilege, RoutineRevokeBehavior, RoutineSecurityAttributes,
};

stored_struct!(AlterRoleStmt {
    name: required,
    attributes: required,
    connection_limit: optional,
    membership_action: optional,
    members: required,
});

stored_enum!(AlterRoutineKind {
    Function,
    Procedure,
    Routine,
});

stored_struct!(AlterRoutineOwnerStmt {
    kind: required,
    name: required,
    arg_types: optional,
    arg_type_references: required,
    new_owner: required,
});

stored_struct!(AlterRoutineStmt {
    kind: required,
    name: required,
    arg_types: defaulted,
    arg_type_references: defaulted,
    volatility: defaulted,
    strict: defaulted,
    security_definer: defaulted,
    leakproof: defaulted,
    parallel: defaulted,
    support: defaulted,
    cost: custom(read_estimate),
    rows: custom(read_estimate),
    config_actions: defaulted,
    attribute_clauses: defaulted,
});

stored_struct!(CreateRoleStmt {
    name: required,
    attributes: required,
    connection_limit: required,
    in_roles: defaulted,
    role_members: defaulted,
    admin_members: defaulted,
});

stored_struct!(DropRoleStmt {
    names: required,
    if_exists: required,
});

stored_enum!(FunctionParallel {
    Unsafe,
    Restricted,
    Safe,
});

stored_struct!(GrantRoleStmt {
    granted_roles: required,
    grantee_roles: required,
    is_grant: required,
    options: required,
    grantor: optional,
    cascade: required,
});

stored_struct!(GrantRoutineItem {
    name: required,
    arg_types: optional,
});

stored_struct!(GrantRoutineStmt {
    kind: required,
    is_grant: required,
    grant_option: required,
    grant_option_only: required,
    items: required,
    schemas: defaulted,
    privileges: defaulted,
    grantees: required,
    grantor: defaulted,
    revoke_behavior: defaulted,
});

stored_struct!(RenameRoleStmt {
    name: required,
    new_name: required,
});

stored_struct!(RenameRoutineStmt {
    kind: required,
    name: required,
    arg_types: defaulted,
    arg_type_references: defaulted,
    new_name: required,
});

stored_enum!(RoleAttribute {
    Superuser,
    Inherit,
    CreateRole,
    CreateDb,
    Login,
    Replication,
    BypassRls,
});

stored_enum!(RoleMembershipAction { Add, Drop });

stored_struct!(RoleMembershipOptions {
    admin: optional,
    inherit: optional,
    set: optional,
});

stored_enum!(RoutineConfigAction {
    Set {
        name: required,
        value: required,
    },
    FromCurrent {
        name: required,
    },
    Reset {
        name: required,
    },
    ResetAll,
});

stored_enum!(RoutinePrivilege {
    Execute,
    Unsupported (value),
    ColumnsUnsupported,
});

stored_enum!(RoutineRevokeBehavior { Restrict, Cascade });

stored_struct!(RoutineSecurityAttributes {
    security_definer: defaulted,
    leakproof: defaulted,
});
