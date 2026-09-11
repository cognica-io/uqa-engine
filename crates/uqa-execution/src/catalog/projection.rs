//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `information_schema` and `pg_catalog` virtual row dispatch.

use uqa_sql::{ResultRow, SQLError};

use crate::catalog::context::CatalogContext;
use crate::catalog::services::CatalogSession;
use crate::catalog::{CatalogReadView, RelationNameResolution};
use uqa_core::RelationIdentity;
use uqa_sql::catalog::constraints::ConstraintIdentity;

pub use uqa_sql::catalog::domain::domain_object_oid;

pub fn is_virtual_catalog_relation(resolution: &RelationNameResolution, name: &str) -> bool {
    resolve_virtual_relation(resolution, name).is_some()
}

pub fn build_info_schema_rows(
    context: &CatalogContext<'_>,
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    session: &dyn CatalogSession,
    name: &str,
) -> Result<Option<Vec<ResultRow>>, SQLError> {
    let Some(relation) = resolve_virtual_relation(resolution, name) else {
        return ag_catalog::build_age_label_relation_rows(catalog, resolution, name);
    };
    let mut catalog_resolution = resolution.clone();
    catalog_resolution.set_lookup_mode(crate::catalog::RelationLookupMode::Bound);
    let resolution = &catalog_resolution;
    Ok(Some(match relation {
        VirtualRelation::InformationSchemaCatalogName => build_info_catalog_name(),
        VirtualRelation::InformationSchemata => build_info_schemata(catalog, resolution)?,
        VirtualRelation::InformationTables => build_info_tables(context, catalog, resolution)?,
        VirtualRelation::InformationColumns => build_info_columns(context, catalog, resolution)?,
        VirtualRelation::InformationColumnPrivileges => {
            build_info_column_privileges(context, catalog, resolution, false)?
        }
        VirtualRelation::InformationRoleColumnGrants => {
            build_info_column_privileges(context, catalog, resolution, true)?
        }
        VirtualRelation::InformationViews => build_info_views(context, catalog, resolution)?,
        VirtualRelation::InformationRoutines => build_info_routines(catalog)?,
        VirtualRelation::InformationSequences => build_info_sequences(catalog, session),
        VirtualRelation::InformationTableConstraints => {
            build_info_table_constraints(catalog, resolution)?
        }
        VirtualRelation::InformationKeyColumnUsage => {
            build_info_key_column_usage(catalog, resolution)?
        }
        VirtualRelation::PgNamespace => build_pg_namespace(catalog, resolution)?,
        VirtualRelation::PgClass => build_pg_class(context, catalog, resolution)?,
        VirtualRelation::PgInherits => build_pg_inherits(catalog, resolution)?,
        VirtualRelation::PgPartitionedTable => {
            build_pg_partitioned_table(context, catalog, resolution)?
        }
        VirtualRelation::PgAttribute => build_pg_attribute(context, catalog, resolution)?,
        VirtualRelation::PgAttrdef => build_pg_attrdef(catalog, resolution)?,
        VirtualRelation::PgConstraint => build_pg_constraint(catalog, resolution)?,
        VirtualRelation::PgIndex => build_pg_index(catalog, resolution)?,
        VirtualRelation::PgTrigger => build_pg_trigger(context, catalog, resolution)?,
        VirtualRelation::PgRewrite => build_pg_rewrite(catalog, resolution)?,
        VirtualRelation::PgRules => build_pg_rules(catalog, resolution)?,
        VirtualRelation::PgTables => build_pg_tables(catalog, resolution)?,
        VirtualRelation::PgViews => build_pg_views(catalog, resolution)?,
        VirtualRelation::PgIndexes => build_pg_indexes(catalog, resolution)?,
        VirtualRelation::PgType => build_pg_type(catalog),
        VirtualRelation::PgRange => build_pg_range(),
        VirtualRelation::PgProc => build_pg_proc(catalog)?,
        VirtualRelation::PgDatabase => build_pg_database(catalog)?,
        VirtualRelation::PgAuthMembers => build_pg_auth_members(catalog),
        VirtualRelation::PgRoles => build_pg_roles(catalog),
        VirtualRelation::PgUser => build_pg_user(catalog),
        VirtualRelation::PgSettings => build_pg_settings(session)?,
        VirtualRelation::PgPreparedStatements => prepared_statements::rows(session)?,
        VirtualRelation::PgDescription => Vec::new(),
        VirtualRelation::PgMatviews => build_pg_matviews(catalog, resolution)?,
        VirtualRelation::PgSequences => build_pg_sequences(catalog, session)?,
        VirtualRelation::AgGraph => build_ag_graph(catalog)?,
        VirtualRelation::AgLabel => build_ag_label(catalog)?,
    }))
}

mod ag_catalog;
mod builtin_routines;
mod events;
use uqa_sql::catalog::expression_text;
mod index_definition;
mod mutation;
pub use index_definition::pg_get_indexdef_value;
pub use mutation::virtual_relation_mutation_error;
pub use regtypes::format_type_value;
mod view_definition;
pub use view_definition::pg_get_viewdef_value;
pub use view_definition::{rename_view_column_query, view_query_references_column};
mod helpers;
pub use uqa_sql::catalog::result_type::{postgres_result_type, SQLTypeMetadata};
mod information_schema;
mod partitioning;
mod pg_catalog;
mod pg_namespace;
mod pg_proc;
mod pg_settings;
mod plpgsql;
mod prepared_statements;
mod regtypes;
pub fn plpgsql_catalog(
    context: &CatalogContext<'_>,
) -> Result<uqa_sql::plpgsql::PlpgsqlCatalog, SQLError> {
    let catalog = context.catalog_read_view();
    let resolution = context.session_execution_view().relation_name_resolution();
    let search_path = context
        .current_schema_names(true)
        .map_err(|error| SQLError::Internal(error.to_string()))?;
    plpgsql::plpgsql_catalog(&catalog, &resolution, search_path)
}
mod relation_catalog;
mod schema;

#[derive(Debug, Clone)]
pub struct RuntimeConstraint {
    pub identity: ConstraintIdentity,
    pub deferrable: bool,
}

pub fn runtime_constraints(
    context: &CatalogContext<'_>,
) -> Result<Vec<RuntimeConstraint>, SQLError> {
    let catalog = context.catalog_read_view();
    let resolution = context.session_execution_view().relation_name_resolution();
    let mut constraints = helpers::constraints::constraint_catalog_rows(&catalog, &resolution)?
        .into_iter()
        .map(|constraint| {
            Ok(RuntimeConstraint {
                identity: ConstraintIdentity {
                    relation: RelationIdentity::new(constraint.schema, constraint.table),
                    name: constraint.name,
                    object_id: constraint.object_id,
                },
                deferrable: constraint.state.deferrable(),
            })
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    for (trigger, _) in events::catalog_triggers(&catalog, &resolution)? {
        if !trigger.definition.constraint {
            continue;
        }
        let relation =
            RelationIdentity::from_legacy_name(&trigger.definition.table).map_err(|error| {
                SQLError::Internal(format!(
                    "decode constraint-trigger relation `{}`: {error}",
                    trigger.definition.table
                ))
            })?;
        constraints.push(RuntimeConstraint {
            identity: ConstraintIdentity {
                relation,
                name: trigger
                    .constraint_name
                    .clone()
                    .unwrap_or_else(|| trigger.definition.name.clone()),
                object_id: trigger.object_id,
            },
            deferrable: trigger.definition.deferrability.is_deferrable(),
        });
    }
    Ok(constraints)
}

pub fn schema_object_oid(name: &str) -> i64 {
    helpers::oids::schema_oid(name)
}

pub fn resolve_age_label_relation_name(
    context: &CatalogContext<'_>,
    name: &str,
) -> Result<Option<String>, SQLError> {
    let catalog = context.catalog_read_view();
    let resolution = context.session_execution_view().relation_name_resolution();
    ag_catalog::resolve_age_label_relation_name(&catalog, &resolution, name)
}

pub fn query_source_column_names(
    context: &CatalogContext<'_>,
    name: &str,
    relations_bound: bool,
) -> Result<Option<Vec<String>>, SQLError> {
    let catalog = context.catalog_read_view();
    let mut resolution = context.session_execution_view().relation_name_resolution();
    if relations_bound {
        resolution.set_lookup_mode(crate::catalog::RelationLookupMode::Bound);
    }
    if catalog.sequence_resolved(&resolution, name)?.is_some() {
        return Ok(Some(vec![
            "last_value".into(),
            "log_cnt".into(),
            "is_called".into(),
        ]));
    }
    if let Some(view) = catalog.view_resolved(&resolution, name)? {
        let schema =
            context.stored_view_schema_with_catalog(view, catalog.clone(), resolution.clone())?;
        return Ok(Some(
            schema
                .columns()
                .iter()
                .enumerate()
                .map(|(position, name)| schema.public_name(position).unwrap_or(name).to_string())
                .collect(),
        ));
    }
    if let Some(table) = catalog.table_resolved(&resolution, name)? {
        return Ok(Some(
            table
                .columns
                .iter()
                .map(|column| column.name.clone())
                .collect(),
        ));
    }
    if let Some(table) = catalog.foreign_table_resolved(&resolution, name)? {
        return Ok(Some(
            table
                .columns
                .iter()
                .map(|column| column.name.clone())
                .collect(),
        ));
    }
    Ok(virtual_relation_schema(&catalog, &resolution, name)?
        .map(|columns| columns.into_iter().map(|(name, _)| name).collect()))
}

use ag_catalog::{build_ag_graph, build_ag_label};
use events::{build_pg_rewrite, build_pg_rules, build_pg_trigger};
pub use events::{event_relation_oid, pg_get_ruledef_value, pg_get_triggerdef_value};
use information_schema::{
    build_info_catalog_name, build_info_column_privileges, build_info_columns,
    build_info_key_column_usage, build_info_routines, build_info_schemata, build_info_sequences,
    build_info_table_constraints, build_info_tables, build_info_views,
};
use partitioning::build_pg_partitioned_table;
pub use partitioning::{pg_get_expr_value, pg_get_partkeydef_value};
pub fn table_relation_oid(context: &CatalogContext<'_>, table: &str) -> Result<i64, SQLError> {
    let catalog = context.catalog_read_view();
    let mut resolution = context.session_execution_view().relation_name_resolution();
    resolution.set_lookup_mode(crate::catalog::RelationLookupMode::Bound);
    snapshot_table_relation_oid(&catalog, &resolution, table)
}
pub fn sequence_relation_oid(object_id: [u8; 16]) -> i64 {
    helpers::oids::stable_object_oid("relation", &object_id)
}
pub fn view_relation_oid(view: &crate::catalog::view::StoredView) -> i64 {
    helpers::oids::stable_object_oid("relation", &view.object_id)
}

pub fn view_rowtype_oid(view: &crate::catalog::view::StoredView) -> i64 {
    helpers::oids::stable_object_oid("rowtype", &view.object_id)
}

pub fn foreign_table_relation_oid(table: &crate::catalog::foreign::StoredForeignTable) -> i64 {
    helpers::oids::stable_object_oid("relation", &table.object_id)
}

pub fn foreign_table_rowtype_oid(table: &crate::catalog::foreign::StoredForeignTable) -> i64 {
    helpers::oids::stable_object_oid("rowtype", &table.object_id)
}
pub fn snapshot_table_relation_oid(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    table: &str,
) -> Result<i64, SQLError> {
    pg_catalog::table_relation_oid_from(catalog, resolution, table)
}
use pg_catalog::{
    build_pg_attrdef, build_pg_attribute, build_pg_auth_members, build_pg_constraint,
    build_pg_database, build_pg_index, build_pg_indexes, build_pg_matviews, build_pg_range,
    build_pg_roles, build_pg_sequences, build_pg_tables, build_pg_type, build_pg_user,
    build_pg_views,
};
use pg_namespace::build_pg_namespace;
use pg_proc::build_pg_proc;
use pg_settings::build_pg_settings;
pub use regtypes::{
    resolve_bound_regclass_oid, resolve_catalog_column_type, resolve_catalog_domain_type_by_oid,
    resolve_regclass_kind_by_oid, resolve_regclass_oid, resolve_regnamespace_oid,
    resolve_regobject_oid, resolve_regprocedure_oid, resolve_regrole_oid, resolve_regtype_oid,
    resolve_regtype_output, RegtypeOutputCatalog,
};

pub fn resolve_catalog_column_type_name(
    context: &CatalogContext<'_>,
    type_name: &str,
) -> Result<uqa_sql::ast::ColumnType, SQLError> {
    let parsed = uqa_sql::parse_regtype_name(type_name)?;
    if let Some(parsed) = parsed.as_ref() {
        if parsed.has_type_modifiers {
            let name = parsed
                .names
                .iter()
                .map(|name| format!("\"{}\"", name.replace('"', "\"\"")))
                .collect::<Vec<_>>()
                .join(".");
            if resolve_catalog_column_type(context, &name)
                .is_some_and(|ty| matches!(ty, uqa_sql::ColumnType::Domain { .. }))
            {
                return Err(SQLError::Routine {
                    sqlstate: "42601".into(),
                    message: format!(
                        "type modifier is not allowed for type \"{}\"",
                        parsed.names.join(".")
                    ),
                });
            }
        }
    }
    resolve_catalog_column_type(context, type_name).ok_or_else(|| SQLError::Routine {
        sqlstate: "42704".into(),
        message: format!(
            "type \"{}\" does not exist",
            parsed.as_ref().map_or_else(
                || type_name.to_string(),
                |name| format!(
                    "{}{}",
                    name.names.join("."),
                    if name.array_dimensions > 0 { "[]" } else { "" }
                )
            )
        ),
    })
}

use relation_catalog::{build_pg_class, build_pg_inherits};
use schema::{resolve_virtual_relation, VirtualRelation};
pub use schema::{virtual_relation_accepts_row_lock, virtual_relation_schema};

pub use partitioning::partition_bound_node;

pub use regtypes::relation_oid::lookup_regclass_oid;

pub use helpers::views::view_columns_for;
