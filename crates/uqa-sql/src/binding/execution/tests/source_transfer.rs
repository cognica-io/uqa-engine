//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::binding::{bind_query_block_with_sources, ExecutionSourceBindings, SOURCE_BINDINGS};
use crate::plan::SourcePlan;
use std::sync::Arc;

fn enter_derived_blocks(
    block: &QueryBlockPlan,
    inherited: Option<&ExecutionSourceBindings<'_>>,
    context: &crate::binding::BindingContext<'_>,
    retain_sources: bool,
) {
    let (schema, sources) = inherited
        .and_then(|sources| sources.query_source(block, None, false))
        .unwrap_or_else(|| {
            if retain_sources {
                let bound =
                    bind_query_block_with_sources(&Routines, block, &[], context, None, false)
                        .unwrap();
                (bound.schema, bound.sources)
            } else {
                let (_, schema) =
                    bind_query_block_for_execution(&Routines, block, &[], context, None, false)
                        .unwrap();
                (schema, None)
            }
        });
    assert_eq!(schema.column_type(0), Some(&ColumnType::Integer));
    if let Some(SourcePlan::Subquery { body, .. }) = &block.from {
        let RelationalPlan::QueryBlock(child) = &body.root else {
            panic!("derived query block");
        };
        enter_derived_blocks(
            child,
            sources.as_deref().map(|value| &**value),
            context,
            retain_sources,
        );
    }
}

#[test]
fn nested_execution_sources_are_bound_once() {
    let context = empty_binding_context();
    for depth in [1, 8, 32] {
        let mut query = block("SELECT value FROM (VALUES (7)) AS input(value)");
        for _ in 0..depth {
            let mut parent = block("SELECT value FROM (SELECT 0 AS value) AS nested OFFSET 0");
            let Some(SourcePlan::Subquery { body, .. }) = &mut parent.from else {
                panic!("derived source");
            };
            body.root = RelationalPlan::QueryBlock(query);
            query = parent;
        }
        SOURCE_BINDINGS.with(|count| count.set(0));
        enter_derived_blocks(&query, None, &context, false);
        let original = SOURCE_BINDINGS.with(std::cell::Cell::get);
        assert_eq!(original, (depth + 1) * (depth + 2) / 2);
        SOURCE_BINDINGS.with(|count| count.set(0));
        enter_derived_blocks(&query, None, &context, true);
        assert_eq!(
            SOURCE_BINDINGS.with(std::cell::Cell::get),
            depth + 1,
            "derived depth {depth}"
        );
        eprintln!(
            "derived depth {depth}: original {original}, transferred {}",
            depth + 1
        );
    }
}

#[test]
fn derived_source_transfer_requires_original_syntax_and_complete_outer_schema() {
    let query = block(
        "SELECT * FROM (SELECT value FROM (VALUES (outer_row.value)) AS input(value)) AS derived",
    );
    let context = empty_binding_context();
    let outer = crate::RowSchema::with_qualified_types(
        "outer_row",
        vec!["value".into()],
        vec![Some(ColumnType::Integer)],
    );
    let bound =
        bind_query_block_with_sources(&Routines, &query, &[], &context, Some(&outer), false)
            .unwrap();
    let sources = bound.sources.unwrap();
    let source = query.from.as_ref().unwrap();
    assert!(sources.matches_source(source));
    assert!(!sources.matches_source(&source.clone()));
    let SourcePlan::Subquery { body, .. } = source else {
        panic!("derived table");
    };
    let RelationalPlan::QueryBlock(child) = &body.root else {
        panic!("query block");
    };
    let (schema, _) = sources.query_source(child, Some(&outer), false).unwrap();
    assert_eq!(schema.column_type(0), Some(&ColumnType::Integer));
    assert!(sources
        .query_source(&child.clone(), Some(&outer), false)
        .is_none());
    assert!(sources.query_source(child, None, false).is_none());
    assert!(sources.query_source(child, Some(&outer), true).is_none());
    let changed = crate::RowSchema::with_qualified_types(
        "outer_row",
        vec!["value".into()],
        vec![Some(ColumnType::Text)],
    );
    assert!(sources.query_source(child, Some(&changed), false).is_none());
    let changed_qualifier = crate::RowSchema::with_qualified_types(
        "other",
        vec!["value".into()],
        vec![Some(ColumnType::Integer)],
    );
    assert!(sources
        .query_source(child, Some(&changed_qualifier), false)
        .is_none());
}

#[test]
fn source_transfer_does_not_skip_local_cte_scopes_or_routine_selection() {
    let context = empty_binding_context();
    for sql in [
        "SELECT * FROM (WITH input AS (SELECT 1 AS value) SELECT value FROM input) AS derived",
        "SELECT * FROM (SELECT value FROM generate_series(1, 3) AS series(value)) AS derived",
    ] {
        let query = block(sql);
        let bound =
            bind_query_block_with_sources(&Routines, &query, &[], &context, None, false).unwrap();
        assert!(bound.sources.is_none(), "{sql}");
        assert_eq!(bound.schema.column_type(0), Some(&ColumnType::Integer));
        let Some(SourcePlan::Subquery { body, .. }) = &query.from else {
            panic!("derived table");
        };
        if body.ctes.is_empty() {
            let RelationalPlan::QueryBlock(child) = &body.root else {
                panic!("query block");
            };
            let bound = bind_query_block_with_sources(&Routines, child, &[], &context, None, false)
                .unwrap();
            let Some(SourcePlan::Function {
                binding: Some(binding),
                ..
            }) = &bound.block.from
            else {
                panic!("selected table function identity");
            };
            assert!(binding.builtin);
        }
    }
}

#[test]
fn source_binding_tree_keeps_join_sides_separate() {
    let query = block("SELECT * FROM (SELECT 1 AS a) AS left_input CROSS JOIN (SELECT 'text'::text AS b) AS right_input");
    let context = empty_binding_context();
    let bound =
        bind_query_block_with_sources(&Routines, &query, &[], &context, None, false).unwrap();
    let tree = bound.sources.unwrap();
    let Some(SourcePlan::Join { left, right, .. }) = &query.from else {
        panic!("join");
    };
    let left_binding = tree.join_child(false).unwrap();
    let right_binding = tree.join_child(true).unwrap();
    assert!(left_binding.matches_source(left));
    assert!(right_binding.matches_source(right));
    assert!(!left_binding.matches_source(right));
    assert!(!Arc::ptr_eq(&left_binding, &right_binding));
}
