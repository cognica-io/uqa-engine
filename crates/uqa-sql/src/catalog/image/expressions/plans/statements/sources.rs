//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{fields, invalid, variant, Node, Part, SQLError};
use crate::ast::{FromClause, TableFunction};

image_struct!(TableFunction {
    name,
    binding,
    output_name,
    relations,
    args,
    column_aliases,
    column_types,
});

image_enum!(FromClause {
    0 => Table { name, qualifier, alias, column_aliases, bound_columns, include_descendants },
    1 => Join { comma, left, right, kind, on, using, natural, alias, column_aliases, lateral },
    2 => Values { lateral, rows, alias, column_aliases, internal_relation, internal_column_types, internal_column_collations },
    3 => Function { name, binding, output_name, relations, args, alias, column_aliases, ordinality, column_types },
    4 => FunctionGroup { functions, alias, column_aliases, ordinality },
    5 => Subquery { lateral, body, alias, column_aliases },
} reject {
    Self::RetainedRows(_) => "execution-scoped stored statement source",
});
