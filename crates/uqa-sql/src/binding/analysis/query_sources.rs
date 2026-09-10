//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Source-owned retrieval scores and deferred wildcard descriptors.

use crate::plan::{ProjectionPlan, QueryBlockPlan};
use crate::ColumnType;
use crate::{ColumnIdentity, RowSchema, ScalarExpr};
use std::collections::BTreeSet;

pub(in crate::binding) fn with_query_source_columns(
    schema: &RowSchema,
    block: &QueryBlockPlan,
) -> RowSchema {
    let schema = super::with_unqualified_table_pseudo_columns(schema);
    let (Some(source), Some(predicate)) = (&block.from, &block.r#where) else {
        return schema;
    };
    let nullable = crate::semantics::outer_join_nullable_qualifiers(source);
    let mut scored = BTreeSet::new();
    for part in crate::semantics::flatten_and_filter_parts(predicate) {
        if !crate::semantics::contains_retrieval(part) || super::super::expr_contains_subquery(part)
        {
            continue;
        }
        let mut qualifiers = crate::semantics::expr_qualifiers(part);
        part.visit(&mut |expression| {
            if let ScalarExpr::Column(column) = expression {
                qualifiers.extend(schema.identities().iter().filter_map(|identity| {
                    (identity.column() == column)
                        .then(|| identity.qualifier().map(str::to_string))
                        .flatten()
                }));
            }
        });
        if qualifiers.len() == 1 {
            let qualifier = qualifiers.into_iter().next().expect("one qualifier");
            if !nullable.contains(&qualifier) && schema.has_qualified_column(&qualifier, "_score") {
                scored.insert(qualifier);
            }
        }
    }
    if scored.len() != 1 {
        return schema;
    }
    RowSchema::with_typed_virtual_identities(
        &schema,
        &[(
            ColumnIdentity::unqualified("_score"),
            Some(ColumnType::DoublePrecision),
        )],
    )
}

pub(in crate::binding) fn with_projected_open_columns(
    output: &RowSchema,
    projections: &[ProjectionPlan],
    source: &RowSchema,
) -> RowSchema {
    let open = projections.iter().any(|projection| match &projection.expr {
        ScalarExpr::Star => source.columns_are_open(None),
        ScalarExpr::QualifiedStar(qualifier) => source.columns_are_open(Some(qualifier)),
        _ => false,
    });
    if open {
        RowSchema::with_open_columns(output, None)
    } else {
        output.clone()
    }
}
