//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored view copying admits complete declarations, relation authority and evaluated cells.

use super::{Copying, Result, SyntaxCopy};

syntax_struct!(crate::catalog::stored_view::StoredView {
    security,
    definition
});
syntax_struct!(crate::catalog::stored_view::StoredViewDefinition {
    object_id,
    query,
    output_columns,
    persistence,
    options,
    kind,
    materialized_rows,
    materialized_column_types,
    materialized_column_collations,
    populated,
    catalog_oids,
    row_type_array_name
});

copy_inline!(
    crate::catalog::view::StoredViewKind,
    crate::catalog::relation_oids::RelationCatalogOids
);
