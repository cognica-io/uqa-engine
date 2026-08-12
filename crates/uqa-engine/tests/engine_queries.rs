//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Consolidated relational planning and query-execution integration tests.

#[path = "aggregate_monoid.rs"]
mod aggregate_monoid;
#[path = "correlated_outer_qualifier.rs"]
mod correlated_outer_qualifier;
#[path = "join_correctness.rs"]
mod join_correctness;
#[path = "manual_sql_examples.rs"]
mod manual_sql_examples;
#[path = "operator_tree_full_surface.rs"]
mod operator_tree_full_surface;
#[path = "operator_tree_pipeline.rs"]
mod operator_tree_pipeline;
#[path = "optimizer_passes.rs"]
mod optimizer_passes;
#[path = "sql_aggregates.rs"]
mod sql_aggregates;
#[path = "sql_blocking_spill.rs"]
mod sql_blocking_spill;
#[path = "sql_correlated_subqueries.rs"]
mod sql_correlated_subqueries;
#[path = "sql_cte.rs"]
mod sql_cte;
#[path = "sql_cursor.rs"]
mod sql_cursor;
#[path = "sql_dpccp_join_order.rs"]
mod sql_dpccp_join_order;
#[path = "sql_explain.rs"]
mod sql_explain;
#[path = "sql_filter_aggregate.rs"]
mod sql_filter_aggregate;
#[path = "sql_golden.rs"]
mod sql_golden;
#[path = "sql_golden_sqlite.rs"]
mod sql_golden_sqlite;
#[path = "sql_grouping_sets.rs"]
mod sql_grouping_sets;
#[path = "sql_integer_literal_normalization.rs"]
mod sql_integer_literal_normalization;
#[path = "sql_join.rs"]
mod sql_join;
#[path = "sql_joins.rs"]
mod sql_joins;
#[path = "sql_lateral.rs"]
mod sql_lateral;
#[path = "sql_limit_offset.rs"]
mod sql_limit_offset;
#[path = "sql_nulls_order.rs"]
mod sql_nulls_order;
#[path = "sql_offset_like.rs"]
mod sql_offset_like;
#[path = "sql_prepared.rs"]
mod sql_prepared;
#[path = "sql_subqueries.rs"]
mod sql_subqueries;
#[path = "sql_subquery.rs"]
mod sql_subquery;
#[path = "sql_window.rs"]
mod sql_window;
#[path = "sql_window_frame.rs"]
mod sql_window_frame;
