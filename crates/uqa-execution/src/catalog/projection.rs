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

mod identity_claims;
pub use identity_claims::{
    catalog_oid_in_use, largest_catalog_oid, validate_catalog_identity_claim,
};
pub(crate) use identity_claims::{legacy_relation_claims, relation_claims};

mod request;
pub(crate) use request::CatalogRequest;

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
    build_requested_catalog_rows(
        context,
        catalog,
        resolution,
        session,
        name,
        &CatalogRequest::default(),
    )
}

pub(crate) fn build_requested_catalog_rows(
    context: &CatalogContext<'_>,
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    session: &dyn CatalogSession,
    name: &str,
    request: &CatalogRequest,
) -> Result<Option<Vec<ResultRow>>, SQLError> {
    let Some(relation) = catalog.virtual_relation_resolved(resolution, name)? else {
        return ag_catalog::build_age_label_relation_rows(catalog, resolution, name);
    };
    let metadata = (!uqa_sql::catalog::SystemRelation::Projected(relation)
        .tracks_serializable_reads())
    .then(|| catalog.metadata_view());
    let catalog = metadata.as_deref().unwrap_or(catalog);
    let mut catalog_resolution = resolution.clone();
    catalog_resolution.set_lookup_mode(crate::catalog::RelationLookupMode::Bound);
    let resolution = &catalog_resolution;
    let output = CatalogOutput(*context);
    let output = Some(&output as &dyn uqa_sql::expr::EngineHook);
    Ok(Some(match relation {
        VirtualRelation::InformationSchemaCatalogName => build_info_catalog_name(),
        VirtualRelation::InformationSchemata => build_info_schemata(catalog, resolution)?,
        VirtualRelation::InformationTables => build_info_tables(context, catalog, resolution)?,
        VirtualRelation::InformationColumns => {
            build_info_columns(context, catalog, resolution, request)?
        }
        VirtualRelation::InformationColumnPrivileges => {
            build_info_column_privileges(context, catalog, resolution, false)?
        }
        VirtualRelation::InformationRoleColumnGrants => {
            build_info_column_privileges(context, catalog, resolution, true)?
        }
        VirtualRelation::InformationViews => build_info_views(context, catalog, resolution)?,
        VirtualRelation::InformationRoutines => build_info_routines(catalog, resolution)?,
        VirtualRelation::InformationSequences => build_info_sequences(catalog, session)?,
        VirtualRelation::InformationTableConstraints => {
            build_info_table_constraints(catalog, resolution)?
        }
        VirtualRelation::InformationKeyColumnUsage => {
            build_info_key_column_usage(catalog, resolution)?
        }
        VirtualRelation::PgNamespace => build_pg_namespace(catalog)?,
        VirtualRelation::PgClass => build_pg_class(context, catalog, resolution)?,
        VirtualRelation::PgInherits => build_pg_inherits(catalog, resolution)?,
        VirtualRelation::PgPartitionedTable => {
            build_pg_partitioned_table(context, catalog, resolution)?
        }
        VirtualRelation::PgAttribute => build_pg_attribute(context, catalog, resolution)?,
        VirtualRelation::PgAttrdef => build_pg_attrdef(output, catalog, resolution)?,
        VirtualRelation::PgConstraint => build_pg_constraint(catalog, resolution)?,
        VirtualRelation::PgIndex => build_pg_index(output, catalog, resolution)?,
        VirtualRelation::PgTrigger => build_pg_trigger(context, catalog, resolution)?,
        VirtualRelation::PgRewrite => build_pg_rewrite(output, catalog, resolution)?,
        VirtualRelation::PgRules => build_pg_rules(output, catalog, resolution)?,
        VirtualRelation::PgTables => build_pg_tables(catalog, resolution)?,
        VirtualRelation::PgViews => build_pg_views(output, catalog, resolution)?,
        VirtualRelation::PgIndexes => build_pg_indexes(output, catalog, resolution)?,
        VirtualRelation::PgType => {
            if request.includes("typdefault") || request.includes("typdefaultbin") {
                build_pg_type(output, catalog, resolution)?
            } else {
                pg_catalog::build_pg_type_without_defaults(catalog, resolution)?
            }
        }
        VirtualRelation::PgRange => build_pg_range(),
        VirtualRelation::PgEnum => build_pg_enum(catalog),
        VirtualRelation::PgProc => {
            if request.includes("proargdefaults") {
                build_pg_proc(output, catalog, resolution)?
            } else {
                pg_proc::build_pg_proc_without_defaults(catalog, resolution)?
            }
        }
        VirtualRelation::PgLanguage => build_pg_language(),
        VirtualRelation::PgForeignDataWrapper => pg_catalog::foreign::wrappers(catalog)?,
        VirtualRelation::PgForeignServer => pg_catalog::foreign::servers(catalog)?,
        VirtualRelation::PgForeignTable => pg_catalog::foreign::tables(catalog)?,
        VirtualRelation::PgDatabase => build_pg_database(catalog)?,
        VirtualRelation::PgAuthid => build_pg_authid(catalog),
        VirtualRelation::PgAuthMembers => build_pg_auth_members(catalog)?,
        VirtualRelation::PgRoles => build_pg_roles(catalog),
        VirtualRelation::PgUser => build_pg_user(catalog),
        VirtualRelation::PgSettings => build_pg_settings(session)?,
        VirtualRelation::PgPreparedStatements => prepared_statements::rows(session)?,
        VirtualRelation::PgCursors => cursors::rows(session),
        VirtualRelation::PgDescription => Vec::new(),
        VirtualRelation::PgDepend => {
            dependencies::retained_dependencies(context, catalog, resolution)?.depend_rows()
        }
        VirtualRelation::PgShdepend => {
            dependencies::retained_dependencies(context, catalog, resolution)?.shared_depend_rows()
        }
        VirtualRelation::PgMatviews => build_pg_matviews(output, catalog, resolution)?,
        VirtualRelation::PgSequences => build_pg_sequences(catalog, session)?,
        VirtualRelation::AgGraph => build_ag_graph(catalog)?,
        VirtualRelation::AgLabel => build_ag_label(catalog)?,
    }))
}

mod ag_catalog;
pub(crate) use ag_catalog::named_label_relation_oid;
mod builtin_routines;
pub(crate) use builtin_routines::native_foreign_handlers;
pub(crate) use builtin_routines::PG18_BUILTIN_ROUTINE_GROUPS;
pub use builtin_routines::{
    builtin_routine_identities, builtin_routine_identity, BuiltinRoutineIdentity,
};
pub use regtypes::catalog_routine_type_oid;
mod cursors;
mod dependencies;
pub(super) use dependencies::DependencyCatalogCache;
pub use dependencies::{
    pg_describe_object_value, role_dependency_detail, CatalogDependencies, CatalogObject,
    RelationKind,
};
pub(super) use pg_catalog::IndexRelations;
mod events;
pub(super) use events::EventDefinitions;
use uqa_sql::catalog::expression_text;
mod index_definition;
mod mutation;
pub use index_definition::pg_get_indexdef_value;
mod routine_definitions;
pub use mutation::virtual_relation_mutation_error;
pub use regtypes::resolve_regprocedure_input_oid;
pub(crate) use regtypes::routine_oid_exists;
pub use regtypes::{format_type_name, format_type_value};
pub(super) use routine_definitions::RoutineDefinitions;
pub use routine_definitions::{
    pg_get_function_arguments_value, pg_get_function_identity_arguments_value,
    pg_get_function_result_value, pg_get_function_sqlbody_value, pg_get_functiondef_value,
};
mod view_definition;
pub use view_definition::pg_get_viewdef_value;
pub use view_definition::{rename_view_column_query, view_query_references_column};
mod helpers;
pub(super) use helpers::constraints::ConstraintDefinitions;
pub(crate) use helpers::index_definitions::index_key_definition;
pub use uqa_sql::catalog::result_type::{postgres_result_type, SQLTypeMetadata};
mod information_schema;
mod output;
pub use output::CatalogOutput;
mod partitioning;
mod pg_catalog;
#[cfg(test)]
pub(crate) use pg_catalog::TYPE_PROJECTION_BUILDS;
mod pg_namespace;
mod pg_proc;
pub(crate) use pg_proc::{routine_oid_in_use, user_routine_catalog_oid};
mod pg_settings;
mod plpgsql;
mod prepared_statements;
mod regtypes;
pub fn plpgsql_catalog(
    context: &CatalogContext<'_>,
) -> Result<uqa_sql::plpgsql::PlpgsqlCatalog, SQLError> {
    let catalog = context.catalog_read_view();
    let resolution = context.session_execution_view().relation_name_resolution();
    // Routine restoration already owns the catalog boundary. Resolve the search path against the same captured definitions; refreshing here can reenter the registry lock and mix namespace generations.
    let search_path = context
        .namespaces
        .current_schema_names_with_catalog(&catalog, true)
        .map_err(|error| SQLError::Internal(error.to_string()))?;
    plpgsql::plpgsql_catalog(&catalog, &resolution, search_path)
}
mod relation_catalog;
mod schema;

#[derive(Debug, Clone)]
pub struct RuntimeConstraint {
    pub identity: ConstraintIdentity,
    pub deferrable: bool,
    pub catalog_oid: Option<i64>,
    /// The catalog row of the constraint this one derives from, whose `SET CONSTRAINTS` mode it follows.
    pub parent_oid: Option<i64>,
}

pub fn runtime_constraints(
    context: &CatalogContext<'_>,
) -> Result<Vec<RuntimeConstraint>, SQLError> {
    let catalog = context.catalog_read_view();
    let resolution = context.session_execution_view().relation_name_resolution();
    let mut constraints = helpers::constraints::constraint_catalog_rows(&catalog, &resolution)?
        .iter()
        .map(|constraint| {
            Ok(RuntimeConstraint {
                identity: ConstraintIdentity {
                    relation: RelationIdentity::new(&constraint.schema, &constraint.table),
                    name: constraint.name.clone(),
                    object_id: constraint.object_id,
                },
                deferrable: constraint.state.deferrable(),
                catalog_oid: constraint.catalog_oid,
                parent_oid: constraint.parent_oid,
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
            catalog_oid: None,
            parent_oid: None,
        });
    }
    Ok(constraints)
}

pub fn schema_object_oid(catalog: &CatalogReadView, name: &str) -> i64 {
    helpers::oids::namespace_oid(catalog, name)
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
pub use events::{
    event_relation_oid, legacy_rule_catalog_oid, pg_get_ruledef_value, pg_get_triggerdef_value,
    rule_catalog_oid,
};
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
/// The OID a sequence created before OIDs were recorded derives from its identity.
pub fn legacy_sequence_relation_oid(object_id: [u8; 16]) -> i64 {
    helpers::oids::stable_object_oid("relation", &object_id)
}
pub fn view_relation_oid(view: &crate::catalog::view::StoredView) -> i64 {
    i64::from(view.relation_oids().relation)
}

pub fn view_rowtype_oid(view: &crate::catalog::view::StoredView) -> i64 {
    i64::from(view.relation_oids().reltype())
}

pub fn foreign_table_relation_oid(table: &crate::catalog::foreign::StoredForeignTable) -> i64 {
    i64::from(table.relation_oids().relation)
}

pub fn foreign_table_rowtype_oid(table: &crate::catalog::foreign::StoredForeignTable) -> i64 {
    i64::from(table.relation_oids().reltype())
}
pub fn snapshot_table_relation_oid(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    table: &str,
) -> Result<i64, SQLError> {
    pg_catalog::table_relation_oid_from(catalog, resolution, table)
}
use pg_catalog::{
    build_pg_attrdef, build_pg_attribute, build_pg_auth_members, build_pg_authid,
    build_pg_constraint, build_pg_database, build_pg_enum, build_pg_index, build_pg_indexes,
    build_pg_language, build_pg_matviews, build_pg_range, build_pg_roles, build_pg_sequences,
    build_pg_tables, build_pg_type, build_pg_user, build_pg_views,
};
use pg_namespace::build_pg_namespace;
pub use pg_namespace::{pg_is_other_temp_schema_value, pg_my_temp_schema_value};
use pg_proc::build_pg_proc;
use pg_settings::build_pg_settings;
pub use regtypes::{
    format_type_object, named_type_exists, resolve_bound_regclass_oid, resolve_catalog_column_type,
    resolve_catalog_user_type_by_oid, resolve_regclass_kind_by_oid, resolve_regclass_oid,
    resolve_regcollation_oid, resolve_regnamespace_oid, resolve_regobject_oid,
    resolve_regproc_input_oid, resolve_regprocedure_oid, resolve_regrole_oid, resolve_regtype_oid,
    resolve_regtype_output, resolve_type_object_oid, row_type_relation, type_privilege_oid,
    RegtypeOutputCatalog,
};

pub fn resolve_catalog_column_type_name(
    context: &CatalogContext<'_>,
    type_name: &str,
) -> Result<uqa_sql::ast::ColumnType, SQLError> {
    if let Some(identity) = uqa_sql::ast::UserTypeIdentity::parse(type_name) {
        return resolve_catalog_column_type(context, type_name).ok_or_else(|| {
            SQLError::Internal(format!("cache lookup failed for type {}", identity.oid))
        });
    }
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

pub use partitioning::{
    partition_bound_node, partition_key_columns, partition_key_expression,
    partition_key_types_for_table,
};

pub use regtypes::relation_oid::lookup_regclass_oid;

pub use helpers::views::view_columns_for;

pub(crate) use pg_catalog::legacy_index_relations;
pub use pg_catalog::pg_get_constraintdef_value;

pub(crate) use pg_catalog::CatalogIndexRelation;

pub(crate) use regtypes::relation_oid::resolved_relation_oid;
