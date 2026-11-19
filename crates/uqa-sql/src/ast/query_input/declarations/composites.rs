//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, Input, InputError, JsonToken, Native, Parts, Stored, Values};
use crate::ast::composites::{
    CompositeAttributeAddition, CompositeAttributeDefinition, CompositeAttributeRemoval,
    CreateCompositeType,
};

stored_struct!(CompositeAttributeAddition {
    attribute: required,
    declaration: required,
});

stored_struct!(CompositeAttributeDefinition {
    name: required,
    ty: required,
    collation: optional,
    setof: defaulted,
});

stored_struct!(CompositeAttributeRemoval {
    name: required,
    if_exists: required,
    cascade: required,
});

stored_struct!(CreateCompositeType {
    name: required,
    attributes: required,
});
