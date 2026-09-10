//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL DDL execution and declared-value conversion.

use super::{DropKind, DropStmt, Engine, SQLError, SQLResult};

mod drop;

pub(super) use drop::run_drop;
pub(super) use uqa_sql::assignment::conversion::{
    coerce_assignment_value, column_type_name, json_table_arg, json_table_value_to_text,
    json_to_core_value,
};
pub(crate) use uqa_sql::assignment::conversion::{
    convert_value_to_column_type, validate_vector_dimensions,
};

pub(crate) use uqa_sql::schema::columns::{
    validate_postgres_column_name, validate_postgres_relation_column_type,
};

pub(crate) use uqa_sql::assignment::conversion::convert_value_to_column_type_with_context as convert_value_to_column_type_with_engine;
