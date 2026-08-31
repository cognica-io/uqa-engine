//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog-backed `reg*` input/output and type-name resolution.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, LazyLock};

use uqa_core::Value;
use uqa_sql::ast::{ColumnType, Statement};
use uqa_sql::{ResultRow, SQLError};

use crate::{Engine, RelationIdentity};

use super::pg_catalog::build_pg_type;
use super::pg_namespace::build_pg_namespace;
use super::pg_proc::build_pg_proc;
use super::relation_catalog::build_pg_class;
use super::{helpers, schema};

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
        helpers::oids::split_schema_name(&canonical).map_err(|error| error.to_string())?;
    if kind == "table" {
        return super::table_relation_oid(engine, &canonical)
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
    Ok(Some(helpers::oids::relation_oid(
        relkind, &schema, &relation,
    )))
}

pub(crate) fn resolve_regprocedure_oid(engine: &Engine, name: &str) -> Result<Option<i64>, String> {
    let Ok(mut statements) = uqa_sql::compile(&format!("DROP FUNCTION {name}")) else {
        return Ok(None);
    };
    if statements.len() != 1 {
        return Ok(None);
    }
    let Statement::DropFunction(statement) = statements.remove(0) else {
        return Ok(None);
    };
    let [item] = statement.items.as_slice() else {
        return Ok(None);
    };
    let Some(argument_types) = item.arg_types.as_ref() else {
        return Ok(None);
    };
    let Ok((schema, local)) = RelationIdentity::parse_reference(&item.name) else {
        return Ok(None);
    };
    let argument_oids = argument_types
        .iter()
        .map(|type_name| {
            resolve_catalog_column_type(engine, type_name).map_or_else(
                || helpers::type_metadata::routine_type_oid(type_name),
                |ty| helpers::type_metadata::pg_type_oid(&ty),
            )
        })
        .collect::<Vec<_>>();
    let catalog = regtype_output_catalog(engine).map_err(|error| error.to_string())?;
    let find_in_schema = |schema: &str| {
        let namespace_oid = helpers::oids::schema_oid(schema);
        catalog.procs.iter().find_map(|(oid, entry)| {
            (entry.namespace_oid == namespace_oid
                && entry.name == local
                && entry.argument_types == argument_oids)
                .then_some(*oid)
        })
    };
    if let Some(schema) = schema {
        return Ok(find_in_schema(&schema));
    }
    for schema in engine
        .current_schema_names(true)
        .map_err(|error| error.to_string())?
    {
        if let Some(oid) = find_in_schema(&schema) {
            return Ok(Some(oid));
        }
    }
    Ok(None)
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

fn catalog_int_list(row: &ResultRow, column: &str) -> Option<Vec<i64>> {
    let Some(Value::List(values)) = row.get(column) else {
        return None;
    };
    values
        .iter()
        .map(|value| match value {
            Value::Int(value) => Some(*value),
            _ => None,
        })
        .collect()
}

fn catalog_str<'a>(row: &'a ResultRow, column: &str) -> Option<&'a str> {
    match row.get(column) {
        Some(Value::Str(value) | Value::FixedChar(value)) => Some(value),
        _ => None,
    }
}

#[derive(Debug)]
struct RegtypeCatalogEntry {
    name: String,
    namespace_oid: i64,
    overloaded: bool,
    argument_types: Vec<i64>,
}

/// One immutable catalog snapshot shared by every `reg*` value formatted until catalog state changes.
#[derive(Debug)]
pub(crate) struct RegtypeOutputCatalog {
    namespaces: BTreeMap<i64, String>,
    classes: BTreeMap<i64, RegtypeCatalogEntry>,
    procs: BTreeMap<i64, RegtypeCatalogEntry>,
    proc_names_by_namespace: BTreeMap<i64, BTreeSet<String>>,
    types: BTreeMap<i64, RegtypeCatalogEntry>,
}

impl RegtypeOutputCatalog {
    fn build(engine: &Engine) -> Result<Self, SQLError> {
        let catalog = engine.catalog_read_view();
        let resolution = engine.session_execution_view().relation_name_resolution();
        let namespaces = build_pg_namespace(&catalog, &resolution)?
            .into_iter()
            .filter_map(|row| {
                Some((
                    catalog_int(&row, "oid")?,
                    catalog_str(&row, "nspname")?.to_string(),
                ))
            })
            .collect();
        let classes = build_pg_class(engine, &catalog, &resolution)?
            .into_iter()
            .filter_map(|row| {
                Some((
                    catalog_int(&row, "oid")?,
                    RegtypeCatalogEntry {
                        name: catalog_str(&row, "relname")?.to_string(),
                        namespace_oid: catalog_int(&row, "relnamespace")?,
                        overloaded: false,
                        argument_types: Vec::new(),
                    },
                ))
            })
            .collect();

        let mut procs = BTreeMap::new();
        let mut proc_name_counts = BTreeMap::new();
        let mut proc_names_by_namespace = BTreeMap::<i64, BTreeSet<String>>::new();
        for row in build_pg_proc(&catalog)? {
            let Some(oid) = catalog_int(&row, "oid") else {
                continue;
            };
            let Some(name) = catalog_str(&row, "proname").map(str::to_string) else {
                continue;
            };
            let Some(namespace_oid) = catalog_int(&row, "pronamespace") else {
                continue;
            };
            *proc_name_counts
                .entry((namespace_oid, name.clone()))
                .or_insert(0_usize) += 1;
            proc_names_by_namespace
                .entry(namespace_oid)
                .or_default()
                .insert(name.clone());
            procs.insert(
                oid,
                RegtypeCatalogEntry {
                    name,
                    namespace_oid,
                    overloaded: false,
                    argument_types: catalog_int_list(&row, "proargtypes").ok_or_else(|| {
                        SQLError::Internal(format!(
                            "pg_proc row {oid} has a malformed proargtypes value"
                        ))
                    })?,
                },
            );
        }
        for entry in procs.values_mut() {
            entry.overloaded = proc_name_counts
                .get(&(entry.namespace_oid, entry.name.clone()))
                .is_some_and(|count| *count > 1);
        }

        let types = build_pg_type()
            .into_iter()
            .filter_map(|row| {
                Some((
                    catalog_int(&row, "oid")?,
                    RegtypeCatalogEntry {
                        name: catalog_str(&row, "typname")?.to_string(),
                        namespace_oid: catalog_int(&row, "typnamespace")?,
                        overloaded: false,
                        argument_types: Vec::new(),
                    },
                ))
            })
            .collect();
        Ok(Self {
            namespaces,
            classes,
            procs,
            proc_names_by_namespace,
            types,
        })
    }
}

fn regtype_output_catalog(engine: &Engine) -> Result<Arc<RegtypeOutputCatalog>, SQLError> {
    loop {
        if let Some(catalog) = engine.runtime.regtype_output_cache.lock().clone() {
            return Ok(catalog);
        }
        let revision = engine
            .runtime
            .regtype_output_cache_revision
            .load(std::sync::atomic::Ordering::Acquire);
        let built = Arc::new(RegtypeOutputCatalog::build(engine)?);
        let mut cache = engine.runtime.regtype_output_cache.lock();
        if engine
            .runtime
            .regtype_output_cache_revision
            .load(std::sync::atomic::Ordering::Acquire)
            != revision
        {
            drop(cache);
            continue;
        }
        return Ok(cache.get_or_insert(built).clone());
    }
}

impl Engine {
    pub(crate) fn clear_regtype_output_cache(&self) {
        self.runtime
            .regtype_output_cache_revision
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        self.runtime.regtype_output_cache.lock().take();
    }
}

fn namespace_name(catalog: &RegtypeOutputCatalog, oid: i64) -> Option<&str> {
    catalog.namespaces.get(&oid).map(String::as_str)
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

fn format_regclass(
    engine: &Engine,
    catalog: &RegtypeOutputCatalog,
    oid: i64,
) -> Result<Option<String>, SQLError> {
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
    let Some(entry) = catalog.classes.get(&oid) else {
        return Ok(None);
    };
    let Some(schema) = namespace_name(catalog, entry.namespace_oid) else {
        return Ok(None);
    };
    Ok(Some(
        if relation_name_is_visible(engine, schema, &entry.name)? {
            uqa_sql::expr::quote_ident(&entry.name)
        } else {
            qualified_name(schema, &entry.name)
        },
    ))
}

fn format_regproc(
    engine: &Engine,
    catalog: &RegtypeOutputCatalog,
    oid: i64,
) -> Result<Option<String>, SQLError> {
    let Some(entry) = catalog.procs.get(&oid) else {
        return Ok(None);
    };
    let Some(schema) = namespace_name(catalog, entry.namespace_oid) else {
        return Ok(None);
    };
    let schemas = engine
        .current_schema_names(true)
        .map_err(|error| SQLError::Internal(error.to_string()))?;
    let visible_schema = schemas.into_iter().find(|candidate_schema| {
        let candidate_oid = helpers::oids::schema_oid(candidate_schema);
        catalog
            .proc_names_by_namespace
            .get(&candidate_oid)
            .is_some_and(|names| names.contains(entry.name.as_str()))
    });
    Ok(Some(
        if !entry.overloaded && visible_schema.as_deref() == Some(schema) {
            uqa_sql::expr::quote_ident(&entry.name)
        } else {
            qualified_name(schema, &entry.name)
        },
    ))
}

fn format_regprocedure(
    engine: &Engine,
    catalog: &RegtypeOutputCatalog,
    oid: i64,
) -> Result<Option<String>, SQLError> {
    let Some(entry) = catalog.procs.get(&oid) else {
        return Ok(None);
    };
    let Some(schema) = namespace_name(catalog, entry.namespace_oid) else {
        return Ok(None);
    };
    let visible_schema = engine
        .current_schema_names(true)
        .map_err(|error| SQLError::Internal(error.to_string()))?
        .into_iter()
        .find(|candidate_schema| {
            let namespace_oid = helpers::oids::schema_oid(candidate_schema);
            catalog.procs.values().any(|candidate| {
                candidate.namespace_oid == namespace_oid
                    && candidate.name == entry.name
                    && candidate.argument_types == entry.argument_types
            })
        });
    let routine_name = if visible_schema.as_deref() == Some(schema) {
        uqa_sql::expr::quote_ident(&entry.name)
    } else {
        qualified_name(schema, &entry.name)
    };
    let arguments = entry
        .argument_types
        .iter()
        .map(|oid| {
            format_regtype(engine, catalog, *oid)
                .map(|name| name.unwrap_or_else(|| oid.to_string()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(format!("{routine_name}({})", arguments.join(","))))
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

fn format_regtype(
    engine: &Engine,
    catalog: &RegtypeOutputCatalog,
    oid: i64,
) -> Result<Option<String>, SQLError> {
    let Some(entry) = catalog.types.get(&oid) else {
        return Ok(None);
    };
    let Some(schema) = namespace_name(catalog, entry.namespace_oid) else {
        return Ok(None);
    };
    let local = if schema == "pg_catalog" {
        pg_catalog_type_output(&entry.name)
    } else {
        uqa_sql::expr::quote_ident(&entry.name)
    };
    Ok(Some(
        if schema == "pg_catalog" || engine.search_path_contains(schema) {
            local
        } else {
            format!("{}.{}", uqa_sql::expr::quote_ident(schema), local)
        },
    ))
}

pub(crate) fn resolve_regtype_output(
    engine: &Engine,
    ty: &ColumnType,
    oid: i64,
) -> Result<Option<String>, String> {
    if !matches!(
        ty,
        ColumnType::Regproc
            | ColumnType::Regprocedure
            | ColumnType::Regclass
            | ColumnType::Regnamespace
            | ColumnType::Regtype
    ) {
        return Ok(None);
    }
    let catalog = regtype_output_catalog(engine).map_err(|error| error.to_string())?;
    let output = match ty {
        ColumnType::Regproc => format_regproc(engine, &catalog, oid),
        ColumnType::Regprocedure => format_regprocedure(engine, &catalog, oid),
        ColumnType::Regclass => format_regclass(engine, &catalog, oid),
        ColumnType::Regnamespace => {
            Ok(namespace_name(&catalog, oid).map(uqa_sql::expr::quote_ident))
        }
        ColumnType::Regtype => format_regtype(engine, &catalog, oid),
        _ => unreachable!(),
    };
    output.map_err(|error| error.to_string())
}
