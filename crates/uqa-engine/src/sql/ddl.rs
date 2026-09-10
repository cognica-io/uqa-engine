//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL DDL execution and declared-value conversion.

use super::scalar::eval_lowered_expression;
use super::{
    index_vectors_for_type, AlterTableAction, AlterTableStmt, BTreeMap, ColumnType, CreateTable,
    Document, DropKind, DropStmt, Engine, RowUpdateVectors, SQLError, SQLResult, Value,
};
use crate::CatalogIndexRow;

mod alter_table;
use uqa_sql::schema::check_inheritance;
use uqa_sql::schema::indexes::names as constraint_indexes;
mod constraint_validation;
mod create_index;
mod create_table;
mod defaults;
mod drop;
mod hierarchy_alter;
mod sequence_ctas;
mod value_conversion;

pub(super) use alter_table::run_alter_table;
pub(crate) use alter_table::{drop_column_cascade, drop_constraint_dependency};
pub(crate) use constraint_validation::{
    bind_stored_check_expression_routines, validate_check_expression,
};
pub(super) use create_index::run_create_index;
pub(super) use create_table::{run_create_table, run_create_table_if_not_exists};
pub(crate) use defaults::{bind_stored_schema_expression_routines, validate_default_expression};
pub(crate) use drop::drop_index_dependency;
pub(super) use drop::run_drop;
pub(super) use sequence_ctas::{
    run_alter_sequence, run_create_sequence, run_create_table_as, CreateTableAsExecution,
};
pub(super) use value_conversion::{
    coerce_assignment_value, coerce_to_column_type, column_type_name, json_table_arg,
    json_table_value_to_text, json_to_core_value,
};
pub(crate) use value_conversion::{
    convert_value_to_column_type, convert_value_to_column_type_with_engine,
    validate_vector_dimensions,
};

use drop::ddl_storage_error;
use value_conversion::rewrite_column_values_to_type;

pub(crate) use uqa_sql::schema::columns::{
    validate_postgres_column_name, validate_postgres_relation_column_type, POSTGRES_SYSTEM_COLUMNS,
};
