//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, Input, InputError, JsonToken, Native, Parts, Stored, Values};
use crate::ast::relation_hierarchy::{
    PartitionBound, PartitionIdentityOverride, PartitionRangeDatum, PartitionSpec,
    PartitionStrategy, TableHierarchy,
};

stored_enum!(PartitionBound {
    Default,
    List (value),
    Range {
        lower: required,
        upper: required,
    },
    Hash {
        modulus: required,
        remainder: required,
    },
});

stored_struct!(PartitionIdentityOverride {
    column: required,
    original: defaulted,
});

stored_enum!(PartitionRangeDatum {
    MinValue,
    Value (value),
    MaxValue,
});

stored_struct!(PartitionSpec {
    strategy: required,
    keys: required,
});

stored_enum!(PartitionStrategy { List, Range, Hash });

stored_struct!(TableHierarchy {
    parents: defaulted,
    parent_sequence_numbers: defaulted,
    partition_spec: defaulted,
    partition_bound: defaulted,
    local_columns: defaulted,
    partition_identity_overrides: defaulted,
    partition_inherited_key_constraints: defaulted,
    partition_inherited_foreign_keys: defaulted,
});
