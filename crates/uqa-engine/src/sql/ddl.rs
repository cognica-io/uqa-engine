//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL DDL execution and declared-value conversion.

use super::scalar::eval_lowered_expression;
use super::{
    index_vectors_for_type, value_to_tensor, value_to_vector, AlterTableAction, AlterTableStmt,
    BTreeMap, ColumnType, CreateIndex, CreateTable, DecimalValue, Document, DropKind, DropStmt,
    Engine, HNSWIndexParams, IVFIndexParams, RowUpdateVectors, SQLError, SQLParam, SQLResult,
    TemporalValue, Value, VectorIndexSpec,
};
use crate::CatalogIndexRow;

mod alter_table;
mod create_index;
mod create_table;
mod defaults;
mod drop;
mod hierarchy;
mod sequence_ctas;
mod value_conversion;

pub(super) use alter_table::run_alter_table;
pub(super) use create_index::run_create_index;
pub(super) use create_table::run_create_table;
pub(super) use drop::run_drop;
use hierarchy::prepare_create_table_hierarchy;
pub(super) use sequence_ctas::{
    run_alter_sequence, run_create_sequence, run_create_table_as, CreateTableAsExecution,
};
pub(super) use value_conversion::{
    coerce_to_column_type, column_type_name, core_value_to_json, json_table_arg,
    json_table_value_to_text, json_to_core_value, value_to_text,
};
pub(crate) use value_conversion::{convert_value_to_column_type, validate_vector_dimensions};

use drop::ddl_storage_error;
use value_conversion::rewrite_column_values_to_type;

#[cfg(test)]
use value_conversion::coerce_json_value;

#[cfg(test)]
mod tests;
