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
mod constraint_validation;
mod create_index;
mod create_table;
mod defaults;
mod drop;
mod hierarchy;
mod hierarchy_alter;
mod sequence_ctas;
mod value_conversion;

pub(crate) use alter_table::drop_constraint_dependency;
pub(super) use alter_table::run_alter_table;
pub(crate) use constraint_validation::{
    bind_stored_check_expression_routines, validate_check_expression,
};
pub(super) use create_index::run_create_index;
pub(super) use create_table::{run_create_table, run_create_table_if_not_exists};
pub(crate) use defaults::{bind_stored_schema_expression_routines, validate_default_expression};
pub(super) use drop::run_drop;
use hierarchy::prepare_create_table_hierarchy;
pub(super) use sequence_ctas::{
    run_alter_sequence, run_create_sequence, run_create_table_as, CreateTableAsExecution,
};
pub(super) use value_conversion::{
    coerce_to_column_type, column_type_name, core_value_to_json, json_table_arg,
    json_table_value_to_text, json_to_core_value, value_to_text,
};
pub(crate) use value_conversion::{
    convert_value_to_column_type, convert_value_to_column_type_with_engine,
    validate_vector_dimensions,
};

use drop::ddl_storage_error;
use value_conversion::rewrite_column_values_to_type;

const POSTGRES_SYSTEM_COLUMNS: [&str; 6] = ["tableoid", "xmin", "cmin", "xmax", "cmax", "ctid"];

pub(crate) fn validate_postgres_column_name(name: &str) -> Result<(), SQLError> {
    if POSTGRES_SYSTEM_COLUMNS.contains(&name) {
        return Err(SQLError::Routine {
            sqlstate: "42701".into(),
            message: format!("column name \"{name}\" conflicts with a system column name"),
        });
    }
    Ok(())
}

pub(crate) fn validate_postgres_relation_column_type(
    name: &str,
    ty: &ColumnType,
) -> Result<(), SQLError> {
    let pseudo_type = match ty {
        ColumnType::Void | ColumnType::AnyArray | ColumnType::Record => Some(ty.sql_name()),
        ColumnType::Array(element)
            if matches!(
                element.as_ref(),
                ColumnType::Void | ColumnType::AnyArray | ColumnType::Record
            ) =>
        {
            Some(ty.sql_name())
        }
        _ => None,
    };
    if let Some(pseudo_type) = pseudo_type {
        return Err(SQLError::Routine {
            sqlstate: "42P16".into(),
            message: format!("column \"{name}\" has pseudo-type {pseudo_type}"),
        });
    }
    Ok(())
}

#[cfg(test)]
use value_conversion::coerce_json_value;

#[cfg(test)]
mod tests;
