//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, Input, InputError, JsonToken, Native, Parts, Stored, Values};
use crate::ast::domains::{
    AlterDomain, AlterDomainAction, CreateDomain, DomainCheck, DomainNotNull,
};

stored_struct!(AlterDomain {
    name: required,
    action: required,
});

stored_enum!(AlterDomainAction {
    SetDefault {
        expression: optional,
    },
    AddCheck {
        constraint: required,
    },
    AddNotNull {
        constraint: required,
    },
    DropConstraint {
        name: required,
        if_exists: required,
        cascade: required,
    },
    ValidateConstraint {
        name: required,
    },
});

stored_struct!(CreateDomain {
    name: required,
    base: required,
    collation: optional,
    collation_oid: defaulted,
    default: optional,
    not_null: optional,
    checks: required,
});

stored_struct!(DomainCheck {
    name: optional,
    catalog_identity: defaulted,
    expression: required,
    validated: default(true),
});

stored_struct!(DomainNotNull {
    name: optional,
    catalog_identity: defaulted,
});
