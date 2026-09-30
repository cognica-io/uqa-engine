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
#[path = "queries/sql_cte_commands.rs"]
mod sql_cte_commands;
#[path = "queries/sql_cte_pg18_controls.rs"]
mod sql_cte_pg18_controls;
#[path = "sql_cursor.rs"]
mod sql_cursor;
#[path = "queries/sql_domains.rs"]
mod sql_domains;
#[path = "sql_dpccp_join_order.rs"]
mod sql_dpccp_join_order;
#[path = "queries/sql_enums.rs"]
mod sql_enums;
#[path = "sql_explain.rs"]
mod sql_explain;
#[path = "queries/sql_failing_row_details.rs"]
mod sql_failing_row_details;
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
#[path = "queries/sql_legacy_vectors.rs"]
mod sql_legacy_vectors;
#[path = "sql_limit_offset.rs"]
mod sql_limit_offset;
#[path = "queries/sql_literal_coercion.rs"]
mod sql_literal_coercion;
#[path = "sql_nulls_order.rs"]
mod sql_nulls_order;
#[path = "sql_offset_like.rs"]
mod sql_offset_like;
#[path = "queries/sql_partition_bounds.rs"]
mod sql_partition_bounds;
#[path = "sql_prepared.rs"]
mod sql_prepared;
#[path = "queries/sql_routine_late_binding.rs"]
mod sql_routine_late_binding;
#[path = "queries/sql_routine_lookup.rs"]
mod sql_routine_lookup;
#[path = "sql_row_locks.rs"]
mod sql_row_locks;
#[path = "queries/sql_row_locks_recheck.rs"]
mod sql_row_locks_recheck;
#[path = "queries/sql_simple_query.rs"]
mod sql_simple_query;
#[path = "sql_subqueries.rs"]
mod sql_subqueries;
#[path = "sql_subquery.rs"]
mod sql_subquery;
#[path = "queries/sql_type_lifecycle.rs"]
mod sql_type_lifecycle;
#[path = "queries/sql_type_lifecycle_reopen.rs"]
mod sql_type_lifecycle_reopen;
#[path = "queries/sql_type_usage.rs"]
mod sql_type_usage;
#[path = "sql_window.rs"]
mod sql_window;
#[path = "sql_window_frame.rs"]
mod sql_window_frame;
