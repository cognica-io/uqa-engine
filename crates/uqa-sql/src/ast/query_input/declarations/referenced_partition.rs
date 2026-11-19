//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, Input, InputError, JsonToken, Native, Parts, Stored, Values};
use crate::ast::referenced_partition::ReferencedPartitionConstraint;

stored_struct!(ReferencedPartitionConstraint {
    partition: required,
    parent: defaulted,
    name: required,
    catalog_identity: required,
    validated: required,
});
