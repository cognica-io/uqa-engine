//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, Result, SyntaxCopy};

syntax_enum!(crate::ast::PartitionBound {
    Default,
    List(value0),
    Range { lower, upper },
    Hash { modulus, remainder }
});

syntax_struct!(crate::ast::PartitionIdentityOverride { column, original });

syntax_enum!(crate::ast::PartitionRangeDatum {
    MinValue,
    Value(value0),
    MaxValue
});

syntax_struct!(crate::ast::PartitionSpec { strategy, keys });

syntax_enum!(crate::ast::PartitionStrategy { List, Range, Hash });

syntax_struct!(crate::ast::TableHierarchy {
    parents,
    parent_sequence_numbers,
    partition_spec,
    partition_bound,
    local_columns,
    partition_identity_overrides,
    partition_inherited_key_constraints,
    partition_inherited_foreign_keys
});
