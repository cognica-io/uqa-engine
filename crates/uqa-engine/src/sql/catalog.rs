//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `information_schema` and `pg_catalog` virtual row synthesis.

use uqa_core::Value;
use uqa_sql::ast::{ColumnDef as SQLColumnDef, ColumnType, Expr};
use uqa_sql::registry::registered_names;
use uqa_sql::{ResultRow, SQLError};

use crate::engine_user_functions::{canonical_routine_type_name, routine_signature_types};
use crate::{Engine, RelationIdentity};

use super::{column_type_name, value_to_text};

pub(super) fn build_info_schema_rows(
    engine: &Engine,
    name: &str,
) -> Result<Option<Vec<ResultRow>>, SQLError> {
    let lower = name.to_ascii_lowercase();
    let is_information_schema = lower.starts_with("information_schema.");
    let is_pg_catalog = lower.starts_with("pg_catalog.");
    let stripped: &str = lower
        .strip_prefix("information_schema.")
        .or_else(|| lower.strip_prefix("pg_catalog."))
        .unwrap_or(&lower);
    Ok(match (is_information_schema, is_pg_catalog, stripped) {
        (true, _, "schemata") => Some(build_info_schemata(engine)?),
        (true, _, "tables") => Some(build_info_tables(engine)?),
        (true, _, "columns") => Some(build_info_columns(engine)?),
        (true, _, "views") => Some(build_info_views(engine)?),
        (true, _, "routines") => Some(build_info_routines(engine)?),
        (true, _, "sequences") => Some(build_info_sequences(engine)?),
        (true, _, "table_constraints") => Some(build_info_table_constraints(engine)?),
        (true, _, "key_column_usage") => Some(build_info_key_column_usage(engine)?),
        (_, true, "pg_namespace") | (false, false, "pg_namespace") => {
            Some(build_pg_namespace(engine)?)
        }
        (_, true, "pg_class") | (false, false, "pg_class") => Some(build_pg_class(engine)?),
        (_, true, "pg_attribute") | (false, false, "pg_attribute") => {
            Some(build_pg_attribute(engine)?)
        }
        (_, true, "pg_attrdef") | (false, false, "pg_attrdef") => Some(build_pg_attrdef(engine)?),
        (_, true, "pg_constraint") | (false, false, "pg_constraint") => {
            Some(build_pg_constraint(engine)?)
        }
        (_, true, "pg_index") | (false, false, "pg_index") => Some(build_pg_index(engine)?),
        (_, true, "pg_tables") | (false, false, "pg_tables") => Some(build_pg_tables(engine)?),
        (_, true, "pg_views") | (false, false, "pg_views") => Some(build_pg_views(engine)?),
        (_, true, "pg_indexes") | (false, false, "pg_indexes") => Some(build_pg_indexes(engine)?),
        (_, true, "pg_type") | (false, false, "pg_type") => Some(build_pg_type()),
        (_, true, "pg_proc") | (false, false, "pg_proc") => Some(build_pg_proc(engine)?),
        (_, true, "pg_database") | (false, false, "pg_database") => Some(build_pg_database()),
        (_, true, "pg_roles") | (false, false, "pg_roles") => Some(build_pg_roles()),
        (_, true, "pg_user") | (false, false, "pg_user") => Some(build_pg_user()),
        (_, true, "pg_settings") | (false, false, "pg_settings") => {
            Some(build_pg_settings(engine)?)
        }
        (_, true, "pg_description") | (false, false, "pg_description") => Some(Vec::new()),
        (_, true, "pg_matviews") | (false, false, "pg_matviews") => Some(Vec::new()),
        (_, true, "pg_sequences") | (false, false, "pg_sequences") => {
            Some(build_pg_sequences(engine)?)
        }
        _ => None,
    })
}

mod helpers;
mod information_schema;
mod pg_catalog;

use information_schema::{
    build_info_columns, build_info_key_column_usage, build_info_routines, build_info_schemata,
    build_info_sequences, build_info_table_constraints, build_info_tables, build_info_views,
};
use pg_catalog::{
    build_pg_attrdef, build_pg_attribute, build_pg_class, build_pg_constraint, build_pg_database,
    build_pg_index, build_pg_indexes, build_pg_namespace, build_pg_proc, build_pg_roles,
    build_pg_sequences, build_pg_settings, build_pg_tables, build_pg_type, build_pg_user,
    build_pg_views,
};
