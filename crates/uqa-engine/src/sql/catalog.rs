//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `information_schema` and `pg_catalog` virtual row synthesis.

use std::sync::LazyLock;

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
        return ag_catalog::build_age_label_relation_rows(engine, name);
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
        VirtualRelation::PgInherits => build_pg_inherits(engine)?,
        VirtualRelation::PgPartitionedTable => build_pg_partitioned_table(engine)?,
        VirtualRelation::PgAttribute => build_pg_attribute(engine)?,
        VirtualRelation::PgAttrdef => build_pg_attrdef(engine)?,
        VirtualRelation::PgConstraint => build_pg_constraint(engine)?,
        VirtualRelation::PgIndex => build_pg_index(engine)?,
        VirtualRelation::PgTrigger => build_pg_trigger(engine)?,
        VirtualRelation::PgRewrite => build_pg_rewrite(engine)?,
        VirtualRelation::PgRules => build_pg_rules(engine)?,
        VirtualRelation::PgTables => build_pg_tables(engine)?,
        VirtualRelation::PgViews => build_pg_views(engine)?,
        VirtualRelation::PgIndexes => build_pg_indexes(engine)?,
        VirtualRelation::PgType => build_pg_type(),
        VirtualRelation::PgRange => build_pg_range(),
        VirtualRelation::PgProc => build_pg_proc(engine)?,
        VirtualRelation::PgDatabase => build_pg_database(),
        VirtualRelation::PgRoles => build_pg_roles(engine),
        VirtualRelation::PgUser => build_pg_user(engine),
        VirtualRelation::PgSettings => build_pg_settings(engine)?,
        VirtualRelation::PgDescription => Vec::new(),
        VirtualRelation::PgMatviews => build_pg_matviews(engine)?,
        VirtualRelation::PgSequences => build_pg_sequences(engine)?,
        VirtualRelation::AgGraph => build_ag_graph(engine)?,
        VirtualRelation::AgLabel => build_ag_label(engine)?,
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
mod pg_proc;
mod relation_catalog;
mod schema;

pub(crate) use ag_catalog::resolve_age_label_relation_name;
use ag_catalog::{build_ag_graph, build_ag_label};
use events::{build_pg_rewrite, build_pg_rules, build_pg_trigger};
pub(in crate::sql) use events::{pg_get_ruledef_value, pg_get_triggerdef_value};
use information_schema::{
    build_info_catalog_name, build_info_columns, build_info_key_column_usage, build_info_routines,
    build_info_schemata, build_info_sequences, build_info_table_constraints, build_info_tables,
    build_info_views,
};
use partitioning::build_pg_partitioned_table;
pub(in crate::sql) use partitioning::{pg_get_expr_value, pg_get_partkeydef_value};
pub(in crate::sql) use pg_catalog::table_relation_oid;
use pg_catalog::{
    build_pg_attrdef, build_pg_attribute, build_pg_constraint, build_pg_database, build_pg_index,
    build_pg_indexes, build_pg_matviews, build_pg_namespace, build_pg_range, build_pg_roles,
    build_pg_sequences, build_pg_settings, build_pg_tables, build_pg_type, build_pg_user,
    build_pg_views,
};
use pg_proc::build_pg_proc;
use relation_catalog::{build_pg_class, build_pg_inherits};
use schema::{resolve_virtual_relation, VirtualRelation};
pub(in crate::sql) use schema::{virtual_relation_accepts_row_lock, virtual_relation_schema};

static CATALOG_DOMAIN_TYPES: LazyLock<Vec<ColumnType>> = LazyLock::new(|| {
    let mut domains = schema::information_schema_domains();
    domains.extend(schema::ag_catalog_domains());
    domains
});

pub(crate) fn resolve_catalog_column_type(engine: &Engine, type_name: &str) -> Option<ColumnType> {
    if let Ok(ty) = ColumnType::from_sql_name(type_name) {
        return Some(ty);
    }
    let mut base_name = type_name.trim();
    let mut array_dimensions = 0usize;
    while let Some(element) = base_name.strip_suffix("[]") {
        base_name = element.trim_end();
        array_dimensions += 1;
    }
    let (schema, local_name) = base_name
        .rsplit_once('.')
        .map_or((None, base_name), |(schema, local_name)| {
            (Some(schema.trim_matches('"')), local_name)
        });
    let local_name = local_name.trim_matches('"');
    let mut resolved = CATALOG_DOMAIN_TYPES
        .iter()
        .find(|domain| match domain {
            ColumnType::Domain {
                schema: domain_schema,
                name: domain_name,
                ..
            } => {
                domain_name == local_name
                    && schema.map_or_else(
                        || engine.search_path_contains(domain_schema),
                        |schema| domain_schema == schema,
                    )
            }
            _ => false,
        })
        .cloned();
    if let Some(ty) = resolved.as_mut() {
        for _ in 0..array_dimensions {
            *ty = ColumnType::Array(Box::new(ty.clone()));
        }
    }
    resolved
}

pub(crate) fn resolve_regclass_oid(engine: &Engine, name: &str) -> Result<Option<i64>, String> {
    if let Some((oid, _, _)) = resolve_virtual_regclass(engine, name)? {
        return Ok(Some(oid));
    }
    let Some((canonical, kind)) = engine
        .try_resolve_relation_kind(name)
        .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    let (schema, relation) =
        helpers::split_schema_name(&canonical).map_err(|error| error.to_string())?;
    if kind == "table" {
        return pg_catalog::table_relation_oid(engine, &canonical)
            .map(Some)
            .map_err(|error| error.to_string());
    }
    let relkind = match kind {
        "view" => "v",
        "materialized view" => "m",
        "sequence" => "S",
        "foreign table" => "f",
        other => return Err(format!("unknown relation kind `{other}` for `{canonical}`")),
    };
    Ok(Some(helpers::relation_oid(relkind, &schema, &relation)))
}

const VIRTUAL_REGCLASSES: &[(&str, &str, i64)] = &[
    ("pg_catalog", "pg_namespace", 2615),
    ("pg_catalog", "pg_class", 1259),
    ("pg_catalog", "pg_inherits", 2611),
    ("pg_catalog", "pg_partitioned_table", 3350),
    ("pg_catalog", "pg_attribute", 1249),
    ("pg_catalog", "pg_attrdef", 2604),
    ("pg_catalog", "pg_constraint", 2606),
    ("pg_catalog", "pg_index", 2610),
    ("pg_catalog", "pg_trigger", 2620),
    ("pg_catalog", "pg_rewrite", 2618),
    ("pg_catalog", "pg_rules", 12023),
    ("pg_catalog", "pg_tables", 12033),
    ("pg_catalog", "pg_views", 12028),
    ("pg_catalog", "pg_indexes", 12043),
    ("pg_catalog", "pg_type", 1247),
    ("pg_catalog", "pg_range", 3541),
    ("pg_catalog", "pg_proc", 1255),
    ("pg_catalog", "pg_database", 1262),
    ("pg_catalog", "pg_roles", 12000),
    ("pg_catalog", "pg_user", 12014),
    ("pg_catalog", "pg_settings", 12104),
    ("pg_catalog", "pg_description", 2609),
    ("pg_catalog", "pg_matviews", 12038),
    ("pg_catalog", "pg_sequences", 12048),
    (
        "information_schema",
        "information_schema_catalog_name",
        13313,
    ),
    ("information_schema", "columns", 13381),
    ("information_schema", "key_column_usage", 13414),
    ("information_schema", "routines", 13462),
    ("information_schema", "schemata", 13467),
    ("information_schema", "sequences", 13471),
    ("information_schema", "table_constraints", 13496),
    ("information_schema", "tables", 13510),
    ("information_schema", "views", 13568),
];

fn resolve_virtual_regclass(
    engine: &Engine,
    name: &str,
) -> Result<Option<(i64, &'static str, &'static str)>, String> {
    let normalized = name.trim().to_ascii_lowercase();
    if let Some((schema, local)) = normalized.rsplit_once('.') {
        return Ok(VIRTUAL_REGCLASSES
            .iter()
            .find(|(candidate_schema, candidate_local, _)| {
                *candidate_schema == schema.trim_matches('"')
                    && *candidate_local == local.trim_matches('"')
            })
            .map(|(schema, local, oid)| (*oid, *schema, *local)));
    }
    let local = normalized.trim_matches('"');
    let Some(visible_schema) =
        visible_relation_schema(engine, local).map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    Ok(VIRTUAL_REGCLASSES
        .iter()
        .find(|(schema, candidate_local, _)| *schema == visible_schema && *candidate_local == local)
        .map(|(schema, local, oid)| (*oid, *schema, *local)))
}

fn catalog_int(row: &ResultRow, column: &str) -> Option<i64> {
    match row.get(column) {
        Some(Value::Int(value)) => Some(*value),
        _ => None,
    }
}

fn catalog_str<'a>(row: &'a ResultRow, column: &str) -> Option<&'a str> {
    match row.get(column) {
        Some(Value::Str(value) | Value::FixedChar(value)) => Some(value),
        _ => None,
    }
}

fn namespace_name(engine: &Engine, oid: i64) -> Result<Option<String>, SQLError> {
    Ok(build_pg_namespace(engine)?
        .into_iter()
        .find(|row| catalog_int(row, "oid") == Some(oid))
        .and_then(|row| catalog_str(&row, "nspname").map(str::to_string)))
}

fn qualified_name(schema: &str, local: &str) -> String {
    format!(
        "{}.{}",
        uqa_sql::expr::quote_ident(schema),
        uqa_sql::expr::quote_ident(local)
    )
}

fn visible_relation_schema(engine: &Engine, local: &str) -> Result<Option<String>, SQLError> {
    let physical = engine
        .try_resolve_relation_kind(local)
        .map_err(|error| SQLError::Internal(error.to_string()))?;
    if let Some((canonical, _)) = physical.as_ref() {
        if let Some((schema, _)) = canonical.rsplit_once('.') {
            if schema.starts_with("pg_temp_") {
                return Ok(Some(schema.to_string()));
            }
        }
    }
    for schema in engine
        .current_schema_names(true)
        .map_err(|error| SQLError::Internal(error.to_string()))?
    {
        if VIRTUAL_REGCLASSES
            .iter()
            .any(|(candidate_schema, candidate_local, _)| {
                *candidate_schema == schema && *candidate_local == local
            })
        {
            return Ok(Some(schema));
        }
        if engine
            .relation_kind_at(&format!("{schema}.{local}"))
            .map_err(|error| SQLError::Internal(error.to_string()))?
            .is_some()
        {
            return Ok(Some(schema));
        }
    }
    Ok(physical.and_then(|(canonical, _)| {
        canonical
            .rsplit_once('.')
            .map(|(schema, _)| schema.to_string())
    }))
}

fn relation_name_is_visible(engine: &Engine, schema: &str, local: &str) -> Result<bool, SQLError> {
    Ok(visible_relation_schema(engine, local)?.as_deref() == Some(schema))
}

fn format_regclass(engine: &Engine, oid: i64) -> Result<Option<String>, SQLError> {
    if let Some((schema, local, _)) = VIRTUAL_REGCLASSES
        .iter()
        .find(|(_, _, candidate_oid)| *candidate_oid == oid)
    {
        return Ok(Some(if relation_name_is_visible(engine, schema, local)? {
            uqa_sql::expr::quote_ident(local)
        } else {
            qualified_name(schema, local)
        }));
    }
    let Some(row) = build_pg_class(engine)?
        .into_iter()
        .find(|row| catalog_int(row, "oid") == Some(oid))
    else {
        return Ok(None);
    };
    let Some(local) = catalog_str(&row, "relname") else {
        return Ok(None);
    };
    let Some(namespace_oid) = catalog_int(&row, "relnamespace") else {
        return Ok(None);
    };
    let Some(schema) = namespace_name(engine, namespace_oid)? else {
        return Ok(None);
    };
    Ok(Some(if relation_name_is_visible(engine, &schema, local)? {
        uqa_sql::expr::quote_ident(local)
    } else {
        qualified_name(&schema, local)
    }))
}

fn format_regproc(engine: &Engine, oid: i64) -> Result<Option<String>, SQLError> {
    let rows = build_pg_proc(engine)?;
    let Some(row) = rows.iter().find(|row| catalog_int(row, "oid") == Some(oid)) else {
        return Ok(None);
    };
    let Some(local) = catalog_str(row, "proname") else {
        return Ok(None);
    };
    let Some(namespace_oid) = catalog_int(row, "pronamespace") else {
        return Ok(None);
    };
    let Some(schema) = namespace_name(engine, namespace_oid)? else {
        return Ok(None);
    };
    let overloaded = rows
        .iter()
        .filter(|candidate| {
            catalog_str(candidate, "proname") == Some(local)
                && catalog_int(candidate, "pronamespace") == Some(namespace_oid)
        })
        .count()
        > 1;
    let schemas = engine
        .current_schema_names(true)
        .map_err(|error| SQLError::Internal(error.to_string()))?;
    let visible_schema = schemas.into_iter().find(|candidate_schema| {
        let candidate_oid = helpers::schema_oid(candidate_schema);
        rows.iter().any(|candidate| {
            catalog_str(candidate, "proname") == Some(local)
                && catalog_int(candidate, "pronamespace") == Some(candidate_oid)
        })
    });
    Ok(Some(
        if !overloaded && visible_schema.as_deref() == Some(schema.as_str()) {
            uqa_sql::expr::quote_ident(local)
        } else {
            qualified_name(&schema, local)
        },
    ))
}

fn pg_catalog_type_output(typname: &str) -> String {
    if let Some(element) = typname.strip_prefix('_') {
        return format!("{}[]", pg_catalog_type_output(element));
    }
    match typname {
        "char" => "\"char\"".into(),
        "bpchar" => "character".into(),
        other => ColumnType::from_sql_name(other)
            .map_or_else(|_| uqa_sql::expr::quote_ident(other), |ty| ty.sql_name()),
    }
}

fn format_regtype(engine: &Engine, oid: i64) -> Result<Option<String>, SQLError> {
    let Some(row) = build_pg_type()
        .into_iter()
        .find(|row| catalog_int(row, "oid") == Some(oid))
    else {
        return Ok(None);
    };
    let Some(typname) = catalog_str(&row, "typname") else {
        return Ok(None);
    };
    let Some(namespace_oid) = catalog_int(&row, "typnamespace") else {
        return Ok(None);
    };
    let Some(schema) = namespace_name(engine, namespace_oid)? else {
        return Ok(None);
    };
    let local = if schema == "pg_catalog" {
        pg_catalog_type_output(typname)
    } else {
        uqa_sql::expr::quote_ident(typname)
    };
    Ok(Some(
        if schema == "pg_catalog" || engine.search_path_contains(&schema) {
            local
        } else {
            format!("{}.{}", uqa_sql::expr::quote_ident(&schema), local)
        },
    ))
}

pub(crate) fn resolve_regtype_output(
    engine: &Engine,
    ty: &ColumnType,
    oid: i64,
) -> Result<Option<String>, String> {
    let output = match ty {
        ColumnType::Regproc => format_regproc(engine, oid),
        ColumnType::Regclass => format_regclass(engine, oid),
        ColumnType::Regnamespace => namespace_name(engine, oid)
            .map(|name| name.map(|name| uqa_sql::expr::quote_ident(&name))),
        ColumnType::Regtype => format_regtype(engine, oid),
        _ => return Ok(None),
    };
    output.map_err(|error| error.to_string())
}
