//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Consolidated expression, type, routine, and `PostgreSQL` semantics tests.

#[path = "pg18_semantics.rs"]
mod pg18_semantics;
#[path = "pg_operator_equivalence.rs"]
mod pg_operator_equivalence;
#[path = "sql_datetime_functions.rs"]
mod sql_datetime_functions;
#[path = "sql_expr_evaluator.rs"]
mod sql_expr_evaluator;
#[path = "sql_json.rs"]
mod sql_json;
#[path = "sql_plpgsql.rs"]
mod sql_plpgsql;
#[path = "sql_registered_functions.rs"]
mod sql_registered_functions;
#[path = "sql_routine_identity.rs"]
mod sql_routine_identity;
#[path = "sql_scalar_functions.rs"]
mod sql_scalar_functions;
#[path = "sql_scalar_functions_coverage.rs"]
mod sql_scalar_functions_coverage;
#[path = "sql_table_functions.rs"]
mod sql_table_functions;
#[path = "sql_temporal_types.rs"]
mod sql_temporal_types;
