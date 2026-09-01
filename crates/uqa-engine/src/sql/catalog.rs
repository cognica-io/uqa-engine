//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `information_schema` and `pg_catalog` virtual row dispatch.

use uqa_sql::{ResultRow, SQLError};

use crate::engine_capabilities::{CatalogReadView, RelationNameResolution, SessionExecutionView};
use crate::{ConstraintIdentity, Engine, RelationIdentity};

pub(super) fn build_info_schema_rows(
    engine: &Engine,
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    session: SessionExecutionView<'_>,
    name: &str,
) -> Result<Option<Vec<ResultRow>>, SQLError> {
    let Some(relation) = resolve_virtual_relation(resolution, name) else {
        return ag_catalog::build_age_label_relation_rows(catalog, resolution, name);
    };
    Ok(Some(match relation {
        VirtualRelation::InformationSchemaCatalogName => build_info_catalog_name(),
        VirtualRelation::InformationSchemata => build_info_schemata(catalog, resolution)?,
        VirtualRelation::InformationTables => build_info_tables(engine, catalog)?,
        VirtualRelation::InformationColumns => build_info_columns(engine, catalog, resolution)?,
        VirtualRelation::InformationViews => build_info_views(engine, catalog)?,
        VirtualRelation::InformationRoutines => build_info_routines(catalog)?,
        VirtualRelation::InformationSequences => build_info_sequences(catalog)?,
        VirtualRelation::InformationTableConstraints => {
            build_info_table_constraints(catalog, resolution)?
        }
        VirtualRelation::InformationKeyColumnUsage => {
            build_info_key_column_usage(catalog, resolution)?
        }
        VirtualRelation::PgNamespace => build_pg_namespace(catalog, resolution)?,
        VirtualRelation::PgClass => build_pg_class(engine, catalog, resolution)?,
        VirtualRelation::PgInherits => build_pg_inherits(catalog, resolution)?,
        VirtualRelation::PgPartitionedTable => {
            build_pg_partitioned_table(engine, catalog, resolution)?
        }
        VirtualRelation::PgAttribute => build_pg_attribute(engine, catalog, resolution)?,
        VirtualRelation::PgAttrdef => build_pg_attrdef(catalog, resolution)?,
        VirtualRelation::PgConstraint => build_pg_constraint(catalog, resolution)?,
        VirtualRelation::PgIndex => build_pg_index(catalog, resolution)?,
        VirtualRelation::PgTrigger => build_pg_trigger(engine, catalog, resolution)?,
        VirtualRelation::PgRewrite => build_pg_rewrite(catalog, resolution)?,
        VirtualRelation::PgRules => build_pg_rules(catalog, resolution)?,
        VirtualRelation::PgTables => build_pg_tables(catalog, resolution)?,
        VirtualRelation::PgViews => build_pg_views(catalog)?,
        VirtualRelation::PgIndexes => build_pg_indexes(catalog, resolution)?,
        VirtualRelation::PgType => build_pg_type(),
        VirtualRelation::PgRange => build_pg_range(),
        VirtualRelation::PgProc => build_pg_proc(catalog)?,
        VirtualRelation::PgDatabase => build_pg_database(),
        VirtualRelation::PgAuthMembers => build_pg_auth_members(catalog),
        VirtualRelation::PgRoles => build_pg_roles(catalog),
        VirtualRelation::PgUser => build_pg_user(catalog),
        VirtualRelation::PgSettings => build_pg_settings(session)?,
        VirtualRelation::PgDescription => Vec::new(),
        VirtualRelation::PgMatviews => build_pg_matviews(catalog)?,
        VirtualRelation::PgSequences => build_pg_sequences(catalog)?,
        VirtualRelation::AgGraph => build_ag_graph(catalog)?,
        VirtualRelation::AgLabel => build_ag_label(catalog)?,
    }))
}

mod ag_catalog;
mod builtin_routines;
mod events;
mod expression_text;
mod helpers;
mod information_schema;
mod partitioning;
mod pg_catalog;
mod pg_namespace;
mod pg_proc;
mod pg_settings;
mod regtypes;
mod relation_catalog;
mod schema;

#[derive(Debug, Clone)]
pub(crate) struct RuntimeConstraint {
    pub(crate) identity: ConstraintIdentity,
    pub(crate) deferrable: bool,
}

pub(crate) fn runtime_constraints(engine: &Engine) -> Result<Vec<RuntimeConstraint>, SQLError> {
    let catalog = engine.catalog_read_view();
    let resolution = engine.session_execution_view().relation_name_resolution();
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

pub(crate) fn resolve_age_label_relation_name(
    engine: &Engine,
    name: &str,
) -> Result<Option<String>, SQLError> {
    let catalog = engine.catalog_read_view();
    let resolution = engine.session_execution_view().relation_name_resolution();
    ag_catalog::resolve_age_label_relation_name(&catalog, &resolution, name)
}

use ag_catalog::{build_ag_graph, build_ag_label};
use events::{build_pg_rewrite, build_pg_rules, build_pg_trigger};
pub(in crate::sql) use events::{
    event_relation_oid, pg_get_ruledef_value, pg_get_triggerdef_value,
};
use information_schema::{
    build_info_catalog_name, build_info_columns, build_info_key_column_usage, build_info_routines,
    build_info_schemata, build_info_sequences, build_info_table_constraints, build_info_tables,
    build_info_views,
};
use partitioning::build_pg_partitioned_table;
pub(in crate::sql) use partitioning::{pg_get_expr_value, pg_get_partkeydef_value};
pub(in crate::sql) fn table_relation_oid(engine: &Engine, table: &str) -> Result<i64, SQLError> {
    let catalog = engine.catalog_read_view();
    let resolution = engine.session_execution_view().relation_name_resolution();
    snapshot_table_relation_oid(&catalog, &resolution, table)
}
pub(in crate::sql) fn snapshot_table_relation_oid(
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
pub(crate) use regtypes::{
    resolve_catalog_column_type, resolve_regclass_oid, resolve_regnamespace_oid,
    resolve_regobject_oid, resolve_regprocedure_oid, resolve_regrole_oid, resolve_regtype_output,
    RegtypeOutputCatalog,
};
use relation_catalog::{build_pg_class, build_pg_inherits};
use schema::{resolve_virtual_relation, VirtualRelation};
pub(in crate::sql) use schema::{virtual_relation_accepts_row_lock, virtual_relation_schema};
