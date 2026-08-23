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
    let Some(relation) = resolve_virtual_relation(engine, name) else {
        return Ok(None);
    };
    Ok(Some(match relation {
        VirtualRelation::InformationSchemaCatalogName => build_info_catalog_name(),
        VirtualRelation::InformationSchemata => build_info_schemata(engine)?,
        VirtualRelation::InformationTables => build_info_tables(engine)?,
        VirtualRelation::InformationColumns => build_info_columns(engine)?,
        VirtualRelation::InformationViews => build_info_views(engine)?,
        VirtualRelation::InformationRoutines => build_info_routines(engine)?,
        VirtualRelation::InformationSequences => build_info_sequences(engine)?,
        VirtualRelation::InformationTableConstraints => build_info_table_constraints(engine)?,
        VirtualRelation::InformationKeyColumnUsage => build_info_key_column_usage(engine)?,
        VirtualRelation::PgNamespace => build_pg_namespace(engine)?,
        VirtualRelation::PgClass => build_pg_class(engine)?,
        VirtualRelation::PgAttribute => build_pg_attribute(engine)?,
        VirtualRelation::PgAttrdef => build_pg_attrdef(engine)?,
        VirtualRelation::PgConstraint => build_pg_constraint(engine)?,
        VirtualRelation::PgIndex => build_pg_index(engine)?,
        VirtualRelation::PgTables => build_pg_tables(engine)?,
        VirtualRelation::PgViews => build_pg_views(engine)?,
        VirtualRelation::PgIndexes => build_pg_indexes(engine)?,
        VirtualRelation::PgType => build_pg_type(),
        VirtualRelation::PgProc => build_pg_proc(engine)?,
        VirtualRelation::PgDatabase => build_pg_database(),
        VirtualRelation::PgRoles => build_pg_roles(),
        VirtualRelation::PgUser => build_pg_user(),
        VirtualRelation::PgSettings => build_pg_settings(engine)?,
        VirtualRelation::PgDescription | VirtualRelation::PgMatviews => Vec::new(),
        VirtualRelation::PgSequences => build_pg_sequences(engine)?,
        VirtualRelation::AgGraph => build_ag_graph(engine)?,
        VirtualRelation::AgLabel => build_ag_label(engine)?,
    }))
}

mod ag_catalog;
mod builtin_routines;
mod expression_text;
mod helpers;
mod information_schema;
mod pg_catalog;
mod schema;

use ag_catalog::{build_ag_graph, build_ag_label};
use information_schema::{
    build_info_catalog_name, build_info_columns, build_info_key_column_usage, build_info_routines,
    build_info_schemata, build_info_sequences, build_info_table_constraints, build_info_tables,
    build_info_views,
};
use pg_catalog::{
    build_pg_attrdef, build_pg_attribute, build_pg_class, build_pg_constraint, build_pg_database,
    build_pg_index, build_pg_indexes, build_pg_namespace, build_pg_proc, build_pg_roles,
    build_pg_sequences, build_pg_settings, build_pg_tables, build_pg_type, build_pg_user,
    build_pg_views,
};
pub(in crate::sql) use schema::virtual_relation_schema;
use schema::{resolve_virtual_relation, VirtualRelation};
