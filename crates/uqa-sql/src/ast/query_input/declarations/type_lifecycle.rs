//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, Input, InputError, JsonToken, Native, Parts, Stored, Values};
use crate::ast::type_lifecycle::{
    AlterTypeObject, AlterTypeObjectAction, CommentType, TypeObjectKind,
};

stored_struct!(AlterTypeObject {
    kind: required,
    name: required,
    action: required,
});

stored_enum!(AlterTypeObjectAction {
    RenameTo (value),
    SetSchema (value),
    OwnerTo (value),
    RenameAttribute {
        from: required,
        to: required,
    },
    AddAttributes (value),
    AlterAttributes {
        removals: required,
        type_changes: defaulted,
        additions: required,
    },
});

stored_struct!(CommentType {
    kind: required,
    name: required,
    description: optional,
});

stored_enum!(TypeObjectKind { Type, Domain });
