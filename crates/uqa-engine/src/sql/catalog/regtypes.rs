//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog-backed `reg*` input/output and type-name resolution.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use uqa_core::Value;
use uqa_sql::ast::ColumnType;
use uqa_sql::{ResultRow, SQLError};

use crate::Engine;

use super::helpers;
use super::pg_catalog::{build_pg_type, catalog_index_relations};
use super::pg_namespace::build_pg_namespace;
use super::pg_proc::build_pg_proc;
use super::relation_catalog::build_pg_class;

mod type_names;

pub(crate) use type_names::resolve_catalog_column_type;

fn cross_database_reference(name: &str) -> SQLError {
    SQLError::Unsupported(format!(
        "cross-database references are not implemented: {name}"
    ))
}

fn qualified_name_list(names: &[String]) -> String {
    names.join(".")
}

enum NumericRegobjectOid {
    NotNumeric,
    Valid(i64),
    InvalidSyntax,
    OutOfRange,
}

fn numeric_regobject_oid(input: &str) -> NumericRegobjectOid {
    if input == "-" {
        return NumericRegobjectOid::Valid(0);
    }
    if input.is_empty() || !input.bytes().all(|byte| byte.is_ascii_digit()) {
        return NumericRegobjectOid::NotNumeric;
    }
    let radix = if input.len() > 1 && input.starts_with('0') {
        8
    } else {
        10
    };
    match u32::from_str_radix(input, radix) {
        Ok(oid) => NumericRegobjectOid::Valid(i64::from(oid)),
        Err(error) if matches!(error.kind(), std::num::IntErrorKind::InvalidDigit) => {
            NumericRegobjectOid::InvalidSyntax
        }
        Err(_) => NumericRegobjectOid::OutOfRange,
    }
}

fn lookup_regclass_oid(engine: &Engine, name: &str) -> Result<Option<i64>, SQLError> {
    match numeric_regobject_oid(name) {
        NumericRegobjectOid::Valid(oid) => return Ok(Some(oid)),
        NumericRegobjectOid::InvalidSyntax | NumericRegobjectOid::OutOfRange => return Ok(None),
        NumericRegobjectOid::NotNumeric => {}
    }
    let Some(names) = uqa_sql::parse_regobject_name(name) else {
        return Ok(None);
    };
    let (schema, local) = relation_name(&names)?;
    if let Some((oid, _, _)) = resolve_virtual_regclass(engine, schema, local)? {
        return Ok(Some(oid));
    }
    let reference = schema.map_or_else(
        || uqa_sql::expr::quote_ident(local),
        |schema| qualified_name(schema, local),
    );
    let Some((canonical, kind)) = engine.try_resolve_visible_relation_kind(&reference)? else {
        return Ok(None);
    };
    if kind == "sequence" {
        let object_id = engine
            .sequence_object_id(&canonical)
            .map_err(|error| SQLError::Internal(error.to_string()))?
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "resolved sequence `{canonical}` has no object identity"
                ))
            })?;
        return Ok(Some(super::sequence_relation_oid(object_id)));
    }
    if kind == "index" {
        let relation =
            crate::RelationIdentity::from_legacy_name(&canonical).map_err(SQLError::Internal)?;
        let catalog = engine.catalog_read_view();
        let mut resolution = engine.session_execution_view().relation_name_resolution();
        resolution.set_lookup_mode(crate::engine_capabilities::RelationLookupMode::Bound);
        let index = catalog_index_relations(&catalog, &resolution)?
            .into_iter()
            .find(|index| index.relation == relation)
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "resolved index `{canonical}` has no catalog relation"
                ))
            })?;
        return Ok(Some(index.oid()));
    }
    let (schema, relation) = helpers::oids::split_schema_name(&canonical)?;
    if kind == "table" {
        return super::table_relation_oid(engine, &canonical)
            .map(Some)
            .map_err(|error| SQLError::Internal(error.to_string()));
    }
    let relkind = match kind {
        "view" => "v",
        "materialized view" => "m",
        "foreign table" => "f",
        other => {
            return Err(SQLError::Internal(format!(
                "unknown relation kind `{other}` for `{canonical}`"
            )));
        }
    };
    Ok(Some(helpers::oids::relation_oid(
        relkind, &schema, &relation,
    )))
}

pub(crate) fn resolve_regclass_oid(engine: &Engine, name: &str) -> Result<Option<i64>, SQLError> {
    lookup_regclass_oid(engine, name)
}

pub(crate) fn resolve_regclass_kind_by_oid(
    engine: &Engine,
    oid: i64,
) -> Result<Option<(String, String)>, SQLError> {
    let catalog = engine.catalog_read_view();
    let resolution = engine.session_execution_view().relation_name_resolution();
    for row in build_pg_class(engine, &catalog, &resolution)? {
        if row.get("oid") != Some(&Value::Int(oid)) {
            continue;
        }
        let name = match row.get("relname") {
            Some(Value::Str(name) | Value::FixedChar(name)) => name.clone(),
            _ => {
                return Err(SQLError::Internal(format!(
                    "pg_class row {oid} has no relname"
                )))
            }
        };
        let kind = match row.get("relkind") {
            Some(Value::Str(kind) | Value::FixedChar(kind)) => kind.clone(),
            _ => {
                return Err(SQLError::Internal(format!(
                    "pg_class row {oid} has no relkind"
                )))
            }
        };
        return Ok(Some((name, kind)));
    }
    Ok(None)
}

fn parse_regprocedure_name(
    name: &str,
) -> Result<Option<uqa_sql::ParsedRegprocedureName>, SQLError> {
    match uqa_sql::parse_regprocedure_name(name) {
        Ok(parsed) => Ok(parsed),
        Err(_) => Ok(None),
    }
}

fn object_name(names: &[String]) -> Result<(Option<&str>, &str), SQLError> {
    match names {
        [local] => Ok((None, local)),
        [schema, local] => Ok((Some(schema), local)),
        [_, _, _] => Err(cross_database_reference(&qualified_name_list(names))),
        _ => Err(SQLError::Parse(format!(
            "improper qualified name (too many dotted names): {}",
            qualified_name_list(names)
        ))),
    }
}

fn relation_name(names: &[String]) -> Result<(Option<&str>, &str), SQLError> {
    match names {
        [local] => Ok((None, local)),
        [schema, local] => Ok((Some(schema), local)),
        [_, _, _] => Err(cross_database_reference(&format!(
            "\"{}\"",
            qualified_name_list(names)
        ))),
        _ => Err(SQLError::Parse(format!(
            "improper relation name (too many dotted names): {}",
            qualified_name_list(names)
        ))),
    }
}

fn type_oid_in_schema(
    catalog: &RegtypeOutputCatalog,
    schema: &str,
    local: &str,
    array_dimensions: usize,
) -> Option<i64> {
    let namespace_oid = catalog
        .namespaces
        .iter()
        .find_map(|(oid, name)| (name == schema).then_some(*oid))?;
    let (oid, entry) = catalog
        .types
        .iter()
        .find(|(_, entry)| entry.namespace_oid == namespace_oid && entry.name == local)?;
    if array_dimensions == 0 || entry.element_oid != 0 {
        return Some(*oid);
    }
    (entry.array_oid != 0).then_some(entry.array_oid)
}

fn parsed_regtype_oid(
    engine: &Engine,
    catalog: &RegtypeOutputCatalog,
    parsed: &uqa_sql::ParsedRegtypeName,
) -> Result<Option<i64>, SQLError> {
    let (schema, local) = object_name(&parsed.names)?;
    if let Some(schema) = schema {
        return Ok(type_oid_in_schema(
            catalog,
            schema,
            local,
            parsed.array_dimensions,
        ));
    }
    for schema in engine
        .current_schema_names(true)
        .map_err(|error| SQLError::Internal(error.to_string()))?
    {
        if let Some(oid) = type_oid_in_schema(catalog, &schema, local, parsed.array_dimensions) {
            return Ok(Some(oid));
        }
    }
    Ok(None)
}

fn lookup_regprocedure_oid(engine: &Engine, name: &str) -> Result<Option<i64>, SQLError> {
    match numeric_regobject_oid(name) {
        NumericRegobjectOid::Valid(oid) => return Ok(Some(oid)),
        NumericRegobjectOid::InvalidSyntax | NumericRegobjectOid::OutOfRange => return Ok(None),
        NumericRegobjectOid::NotNumeric => {}
    }
    let Some(parsed) = parse_regprocedure_name(name)? else {
        return Ok(None);
    };
    let Some(argument_types) = parsed.argument_types.as_ref() else {
        return Ok(None);
    };
    let (schema, local) = object_name(&parsed.names)?;
    let catalog = regtype_output_catalog(engine)?;
    let argument_oids = argument_types
        .iter()
        .map(|type_name| parsed_regtype_oid(engine, &catalog, type_name))
        .collect::<Result<Option<Vec<_>>, _>>()?;
    let Some(argument_oids) = argument_oids else {
        return Ok(None);
    };
    let find_in_schema = |schema: &str| {
        let namespace_oid = catalog
            .namespaces
            .iter()
            .find_map(|(oid, name)| (name == schema).then_some(*oid))?;
        catalog.procs.iter().find_map(|(oid, entry)| {
            (entry.namespace_oid == namespace_oid
                && entry.name == *local
                && entry.argument_types == argument_oids)
                .then_some(*oid)
        })
    };
    if let Some(schema) = schema {
        return Ok(find_in_schema(schema));
    }
    for schema in engine
        .current_schema_names(true)
        .map_err(|error| SQLError::Internal(error.to_string()))?
    {
        if let Some(oid) = find_in_schema(&schema) {
            return Ok(Some(oid));
        }
    }
    Ok(None)
}

pub(crate) fn resolve_regprocedure_oid(engine: &Engine, name: &str) -> Result<Option<i64>, String> {
    lookup_regprocedure_oid(engine, name).map_err(|error| error.to_string())
}

fn lookup_regproc_oid(engine: &Engine, name: &str) -> Result<Option<i64>, SQLError> {
    match numeric_regobject_oid(name) {
        NumericRegobjectOid::Valid(oid) => return Ok(Some(oid)),
        NumericRegobjectOid::InvalidSyntax | NumericRegobjectOid::OutOfRange => return Ok(None),
        NumericRegobjectOid::NotNumeric => {}
    }
    let Some(names) = uqa_sql::parse_regobject_name(name) else {
        return Ok(None);
    };
    let (schema, local) = object_name(&names)?;
    let catalog = regtype_output_catalog(engine)?;
    if let Some(schema) = schema {
        let Some(namespace_oid) = catalog
            .namespaces
            .iter()
            .find_map(|(oid, name)| (name == schema).then_some(*oid))
        else {
            return Ok(None);
        };
        let mut matches = catalog.procs.iter().filter_map(|(oid, entry)| {
            (entry.namespace_oid == namespace_oid && entry.name == *local).then_some(*oid)
        });
        let first = matches.next();
        return Ok(first.filter(|_| matches.next().is_none()));
    }

    let mut visible = BTreeMap::<Vec<i64>, i64>::new();
    for schema in engine
        .current_schema_names(true)
        .map_err(|error| SQLError::Internal(error.to_string()))?
    {
        let Some(namespace_oid) = catalog
            .namespaces
            .iter()
            .find_map(|(oid, name)| (name == &schema).then_some(*oid))
        else {
            continue;
        };
        for (oid, entry) in &catalog.procs {
            if entry.namespace_oid == namespace_oid && entry.name == *local {
                visible.entry(entry.argument_types.clone()).or_insert(*oid);
            }
        }
    }
    let mut matches = visible.into_values();
    let first = matches.next();
    Ok(first.filter(|_| matches.next().is_none()))
}

fn lookup_regnamespace_oid(engine: &Engine, name: &str) -> Result<Option<i64>, SQLError> {
    match numeric_regobject_oid(name) {
        NumericRegobjectOid::Valid(oid) => return Ok(Some(oid)),
        NumericRegobjectOid::InvalidSyntax | NumericRegobjectOid::OutOfRange => return Ok(None),
        NumericRegobjectOid::NotNumeric => {}
    }
    let Some(names) = uqa_sql::parse_regobject_name(name) else {
        return Ok(None);
    };
    let [name] = names.as_slice() else {
        return Ok(None);
    };
    let catalog = regtype_output_catalog(engine)?;
    Ok(catalog
        .namespaces
        .iter()
        .find_map(|(oid, schema)| (schema == name).then_some(*oid)))
}

pub(crate) fn resolve_regnamespace_oid(
    engine: &Engine,
    input: &str,
) -> Result<Option<i64>, SQLError> {
    match numeric_regobject_oid(input) {
        NumericRegobjectOid::Valid(oid) => return Ok(Some(oid)),
        NumericRegobjectOid::InvalidSyntax => {
            return Err(SQLError::Routine {
                sqlstate: "22P02".into(),
                message: format!("invalid input syntax for type oid: \"{input}\""),
            });
        }
        NumericRegobjectOid::OutOfRange => {
            return Err(SQLError::Routine {
                sqlstate: "22003".into(),
                message: format!("value \"{input}\" is out of range for type oid"),
            });
        }
        NumericRegobjectOid::NotNumeric => {}
    }
    let names = uqa_sql::parse_regobject_name(input).ok_or_else(|| SQLError::Routine {
        sqlstate: "42602".into(),
        message: "invalid name syntax".into(),
    })?;
    let [name] = names.as_slice() else {
        return Err(SQLError::Routine {
            sqlstate: "42602".into(),
            message: "invalid name syntax".into(),
        });
    };
    let catalog = regtype_output_catalog(engine)?;
    catalog
        .namespaces
        .iter()
        .find_map(|(oid, schema)| (schema == name).then_some(*oid))
        .map(Some)
        .ok_or_else(|| SQLError::Routine {
            sqlstate: "3F000".into(),
            message: format!("schema \"{}\" does not exist", name.replace('"', "\"\"")),
        })
}

fn lookup_regtype_oid(engine: &Engine, name: &str) -> Result<Option<i64>, SQLError> {
    match numeric_regobject_oid(name) {
        NumericRegobjectOid::Valid(oid) => return Ok(Some(oid)),
        NumericRegobjectOid::InvalidSyntax | NumericRegobjectOid::OutOfRange => return Ok(None),
        NumericRegobjectOid::NotNumeric => {}
    }
    let Some(parsed) = uqa_sql::parse_regtype_name(name)? else {
        return Ok(None);
    };
    let catalog = regtype_output_catalog(engine)?;
    parsed_regtype_oid(engine, &catalog, &parsed)
}

enum ParsedRegroleInput {
    Oid(i64),
    Name(String),
}

fn parse_regrole_input(input: &str) -> Result<ParsedRegroleInput, SQLError> {
    match numeric_regobject_oid(input) {
        NumericRegobjectOid::Valid(oid) => return Ok(ParsedRegroleInput::Oid(oid)),
        NumericRegobjectOid::InvalidSyntax => {
            return Err(SQLError::Routine {
                sqlstate: "22P02".into(),
                message: format!("invalid input syntax for type oid: \"{input}\""),
            });
        }
        NumericRegobjectOid::OutOfRange => {
            return Err(SQLError::Routine {
                sqlstate: "22003".into(),
                message: format!("value \"{input}\" is out of range for type oid"),
            });
        }
        NumericRegobjectOid::NotNumeric => {}
    }
    let names = uqa_sql::parse_regobject_name(input).ok_or_else(|| SQLError::Routine {
        sqlstate: "42602".into(),
        message: "invalid name syntax".into(),
    })?;
    let [name] = names.as_slice() else {
        return Err(SQLError::Routine {
            sqlstate: "42602".into(),
            message: "invalid name syntax".into(),
        });
    };
    Ok(ParsedRegroleInput::Name(name.clone()))
}

pub(crate) fn resolve_regrole_oid(engine: &Engine, input: &str) -> Result<Option<i64>, SQLError> {
    match parse_regrole_input(input)? {
        ParsedRegroleInput::Oid(oid) => Ok(Some(oid)),
        ParsedRegroleInput::Name(name) => {
            let catalog = engine.catalog_read_view();
            let oid = catalog
                .roles()
                .find_map(|role| (role.name == name).then_some(role.oid));
            oid.map(Some).ok_or_else(|| SQLError::Routine {
                sqlstate: "42704".into(),
                message: format!("role \"{}\" does not exist", name.replace('"', "\"\"")),
            })
        }
    }
}

fn lookup_regrole_oid(engine: &Engine, name: &str) -> Result<Option<i64>, SQLError> {
    match resolve_regrole_oid(engine, name) {
        Ok(oid) => Ok(oid),
        Err(SQLError::Routine { .. }) => Ok(None),
        Err(error) => Err(error),
    }
}

pub(crate) fn resolve_regobject_oid(
    engine: &Engine,
    ty: &ColumnType,
    name: &str,
) -> Result<Option<i64>, SQLError> {
    match ty {
        ColumnType::Regproc => lookup_regproc_oid(engine, name),
        ColumnType::Regprocedure => lookup_regprocedure_oid(engine, name),
        ColumnType::Regclass => lookup_regclass_oid(engine, name),
        ColumnType::Regnamespace => lookup_regnamespace_oid(engine, name),
        ColumnType::Regrole => lookup_regrole_oid(engine, name),
        ColumnType::Regtype => lookup_regtype_oid(engine, name),
        _ => Err(SQLError::Internal(format!(
            "unsupported regobject lookup type `{}`",
            ty.sql_name()
        ))),
    }
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
    ("information_schema", "column_privileges", 13371),
    ("information_schema", "columns", 13381),
    ("information_schema", "key_column_usage", 13414),
    ("information_schema", "routines", 13462),
    ("information_schema", "role_column_grants", 13429),
    ("information_schema", "schemata", 13467),
    ("information_schema", "sequences", 13471),
    ("information_schema", "table_constraints", 13496),
    ("information_schema", "tables", 13510),
    ("information_schema", "views", 13568),
];

fn resolve_virtual_regclass(
    engine: &Engine,
    schema: Option<&str>,
    local: &str,
) -> Result<Option<(i64, &'static str, &'static str)>, SQLError> {
    if let Some(schema) = schema {
        return Ok(VIRTUAL_REGCLASSES
            .iter()
            .find(|(candidate_schema, candidate_local, _)| {
                *candidate_schema == schema && *candidate_local == local
            })
            .map(|(schema, local, oid)| (*oid, *schema, *local)));
    }
    let Some(visible_schema) = visible_relation_schema(engine, local)? else {
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
    array_oid: i64,
    element_oid: i64,
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
                        array_oid: 0,
                        element_oid: 0,
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
                    array_oid: 0,
                    element_oid: 0,
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
                        array_oid: catalog_int(&row, "typarray").unwrap_or(0),
                        element_oid: catalog_int(&row, "typelem").unwrap_or(0),
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
    let physical = engine.try_resolve_visible_relation_kind(local)?;
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

fn format_regrole(engine: &Engine, oid: i64) -> Option<String> {
    let catalog = engine.catalog_read_view();
    let name = catalog
        .roles()
        .find_map(|role| (role.oid == oid).then(|| uqa_sql::expr::quote_ident(&role.name)));
    name
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
            | ColumnType::Regrole
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
        ColumnType::Regrole => Ok(format_regrole(engine, oid)),
        ColumnType::Regtype => format_regtype(engine, &catalog, oid),
        _ => unreachable!(),
    };
    output.map_err(|error| error.to_string())
}
