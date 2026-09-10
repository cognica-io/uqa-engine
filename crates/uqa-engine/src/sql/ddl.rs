//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL DDL execution and declared-value conversion.

use super::{AlterTableAction, AlterTableStmt, DropKind, DropStmt, Engine, SQLError, SQLResult};

mod alter_table;
mod constraint_validation;
mod create_index;
mod create_table;
mod defaults;
mod drop;
mod sequence_ctas;

pub(super) use alter_table::run_alter_table;
pub(crate) use alter_table::{drop_column_cascade, drop_constraint_dependency};
pub(crate) use constraint_validation::validate_check_expression;
pub(super) use create_index::run_create_index;
pub(super) use create_table::{run_create_table, run_create_table_if_not_exists};
pub(crate) use defaults::validate_default_expression;
pub(super) use drop::run_drop;
pub(super) use sequence_ctas::{
    run_alter_sequence, run_create_sequence, run_create_table_as, CreateTableAsExecution,
};
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
