//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, Input, InputError, JsonToken, Native, Parts, Stored, Values};
use crate::ast::type_privileges::{GrantTypeStmt, TypePrivilege, TypeRevokeBehavior};

stored_struct!(GrantTypeStmt {
    kind: required,
    is_grant: required,
    grant_option: required,
    grant_option_only: required,
    privileges: required,
    names: required,
    grantees: required,
    grantor: optional,
    revoke_behavior: required,
});

stored_enum!(TypePrivilege {
    Usage,
    Unsupported (value),
});

stored_enum!(TypeRevokeBehavior { Restrict, Cascade });
