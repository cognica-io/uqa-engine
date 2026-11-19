//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, Input, InputError, JsonToken, Native, Parts, Stored, Values};
use crate::ast::enums::{AlterEnum, AlterEnumAction, CreateEnum, EnumNeighbor};

stored_struct!(AlterEnum {
    name: required,
    action: required,
});

stored_enum!(AlterEnumAction {
    AddValue {
        label: required,
        if_not_exists: required,
        neighbor: optional,
    },
    RenameValue {
        old: required,
        new: required,
    },
});

stored_struct!(CreateEnum {
    name: required,
    labels: required,
});

stored_struct!(EnumNeighbor {
    label: required,
    after: required,
});
