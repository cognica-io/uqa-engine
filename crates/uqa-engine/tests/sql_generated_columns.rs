//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 generated-column DDL, DML, catalog, dependency, and persistence parity.

use std::collections::BTreeMap;
use std::sync::Arc;

use tempfile::TempDir;
use uqa_core::{Predicate, Value};
use uqa_engine::operator_tree_bridge::EngineDriver;
use uqa_engine::Engine;
use uqa_operators::{OperatorTree, SumMonoid};
use uqa_planner::executor::{OperatorOutput, OperatorTreeDriver};
use uqa_sql::ColumnType;

#[path = "sql_generated_columns/array_transforms.rs"]
mod array_transforms;

#[path = "sql_generated_columns/behavior.rs"]
mod behavior;
#[path = "sql_generated_columns/catalog_and_persistence.rs"]
mod catalog_and_persistence;
#[path = "sql_generated_columns/checksums.rs"]
mod checksums;
#[path = "sql_generated_columns/gamma_functions.rs"]
mod gamma_functions;
#[path = "sql_generated_columns/json_strip_nulls.rs"]
mod json_strip_nulls;
#[path = "sql_generated_columns/md5_overloads.rs"]
mod md5_overloads;
#[path = "sql_generated_columns/mutations_and_functions.rs"]
mod mutations_and_functions;
#[path = "sql_generated_columns/operators.rs"]
mod operators;
#[path = "sql_generated_columns/reverse_overloads.rs"]
mod reverse_overloads;
#[path = "sql_generated_columns/string_binary_lengths.rs"]
mod string_binary_lengths;
#[path = "sql_generated_columns/typing.rs"]
mod typing;
#[path = "sql_generated_columns/validation.rs"]
mod validation;

fn int(row: &uqa_sql::ResultRow, column: &str) -> i64 {
    match row.get(column) {
        Some(Value::Int(value)) => *value,
        other => panic!("expected integer column `{column}`, got {other:?}"),
    }
}
