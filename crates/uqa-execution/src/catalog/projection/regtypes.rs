//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog-backed `reg*` input/output and type-name resolution.

mod collations;
mod format_type;
pub use collations::resolve_regcollation_oid;
pub mod relation_oid;
mod row_types;
pub use format_type::{format_type_name, format_type_value};
use relation_oid::lookup_regclass_oid;
pub use relation_oid::resolve_bound_regclass_oid;
pub use row_types::{format_type_object, resolve_type_object_oid, row_type_relation};

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use uqa_core::Value;
use uqa_sql::ast::ColumnType;
use uqa_sql::{ResultRow, SQLError};

use crate::catalog::context::CatalogContext;
use crate::catalog::{CatalogReadView, RelationNameResolution};

use super::pg_catalog::build_pg_type_without_defaults;
use super::pg_namespace::build_pg_namespace;
use super::pg_proc::build_pg_proc_without_defaults;
use super::relation_catalog::relation_catalog_identities;

mod procedures;
mod type_names;
use procedures::{format_regproc, format_regprocedure, lookup_regprocedure_oid};
pub use procedures::{resolve_regprocedure_input_oid, routine_name_parts};

pub use type_names::{
    catalog_routine_type_oid, catalog_type_display_name, catalog_user_type_identity,
    named_type_exists, resolve_catalog_column_type, resolve_catalog_user_type_by_oid,
    CatalogEnumLabels,
};

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

/// `parseDashOrOid`: `-` is the invalid OID and a string of digits is an OID `oidin` reads, which reports `22P02` for digits `strtoul` cannot read as one number and `22003` for a value past `uint32`; any other string is a name.
fn parse_dash_or_oid(input: &str) -> Result<Option<i64>, SQLError> {
    match numeric_regobject_oid(input) {
        NumericRegobjectOid::Valid(oid) => Ok(Some(oid)),
        NumericRegobjectOid::InvalidSyntax => Err(SQLError::Routine {
            sqlstate: "22P02".into(),
            message: format!("invalid input syntax for type oid: \"{input}\""),
        }),
        NumericRegobjectOid::OutOfRange => Err(SQLError::Routine {
            sqlstate: "22003".into(),
            message: format!("value \"{input}\" is out of range for type oid"),
        }),
        NumericRegobjectOid::NotNumeric => Ok(None),
    }
}

/// `regclassin` for a direct cast: the relation's OID, or the error the input function reports.
pub fn resolve_regclass_oid(
    context: &CatalogContext<'_>,
    name: &str,
) -> Result<Option<i64>, SQLError> {
    relation_oid::regclass_input_oid(context, name).map(Some)
}

pub fn resolve_regclass_kind_by_oid(
    context: &CatalogContext<'_>,
    oid: i64,
) -> Result<Option<(String, String)>, SQLError> {
    let catalog = context.catalog_read_view();
    let catalog = catalog.metadata_view();
    let resolution = context.session_execution_view().relation_name_resolution();
    Ok(relation_catalog_identities(&catalog, &resolution)?
        .into_iter()
        .find(|entry| entry.oid == oid)
        .map(|entry| (entry.relation.name, entry.kind.to_string())))
}

fn object_name(names: &[String]) -> Result<(Option<&str>, &str), SQLError> {
    match names {
        [local] => Ok((None, local)),
        [schema, local] => Ok((Some(schema), local)),
        [_, _, _] => Err(cross_database_reference(&qualified_name_list(names))),
        _ => Err(SQLError::Routine {
            sqlstate: "42601".into(),
            message: format!(
                "improper qualified name (too many dotted names): {}",
                qualified_name_list(names)
            ),
        }),
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

fn type_in_schema<'a>(
    catalog: &'a RegtypeOutputCatalog,
    schema: &str,
    local: &str,
) -> Option<(i64, &'a RegtypeCatalogEntry)> {
    let namespace_oid = catalog
        .namespaces
        .iter()
        .find_map(|(oid, name)| (name == schema).then_some(*oid))?;
    let (oid, entry) = catalog
        .types
        .iter()
        .find(|(_, entry)| entry.namespace_oid == namespace_oid && entry.name == local)?;
    Some((*oid, entry))
}

/// Array syntax is applied after selecting the first visible base name, even when that name has no array type.
fn type_oid_for_dimensions(
    oid: i64,
    entry: &RegtypeCatalogEntry,
    array_dimensions: usize,
) -> Option<i64> {
    if array_dimensions == 0 {
        return Some(oid);
    }
    (entry.array_oid != 0).then_some(entry.array_oid)
}

fn parsed_regtype_oid(
    context: &CatalogContext<'_>,
    catalog: &RegtypeOutputCatalog,
    parsed: &uqa_sql::ParsedRegtypeName,
) -> Result<Option<i64>, SQLError> {
    let (schema, local) = object_name(&parsed.names)?;
    if let Some(schema) = schema {
        let temporary_schema;
        let schema = if schema == "pg_temp" {
            temporary_schema = context.session.temporary_schema_name();
            temporary_schema.as_str()
        } else {
            schema
        };
        if context.schema_security_for_privilege(schema).is_none() {
            return Err(SQLError::Routine {
                sqlstate: "3F000".into(),
                message: format!("schema \"{schema}\" does not exist"),
            });
        }
        context.require_schema_privilege(
            schema,
            &context.current_role(),
            crate::catalog::security::schema::SchemaAclPrivilege::Usage,
        )?;
        return Ok(
            type_in_schema(catalog, schema, local).and_then(|(oid, entry)| {
                type_oid_for_dimensions(oid, entry, parsed.array_dimensions)
            }),
        );
    }
    for schema in context
        .current_schema_names(true)
        .map_err(|error| SQLError::Internal(error.to_string()))?
    {
        if let Some((oid, entry)) = type_in_schema(catalog, &schema, local) {
            return Ok(type_oid_for_dimensions(oid, entry, parsed.array_dimensions));
        }
    }
    Ok(None)
}

pub fn resolve_regprocedure_oid(
    context: &CatalogContext<'_>,
    name: &str,
) -> Result<Option<i64>, String> {
    lookup_regprocedure_oid(context, name).map_err(|error| error.to_string())
}

/// `to_regproc`: `NULL` where `regprocin` would reject the name.
fn lookup_regproc_oid(context: &CatalogContext<'_>, name: &str) -> Result<Option<i64>, SQLError> {
    match resolve_regproc_input_oid(context, name) {
        Ok(oid) => Ok(Some(oid)),
        Err(SQLError::Routine { sqlstate, .. })
            if matches!(
                sqlstate.as_str(),
                "22P02" | "22003" | "42602" | "42883" | "42725" | "3F000"
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

/// `regprocin`: a routine name, optionally qualified, that names exactly one routine the search path finds.
pub fn resolve_regproc_input_oid(
    context: &CatalogContext<'_>,
    name: &str,
) -> Result<i64, SQLError> {
    let input_error = |sqlstate: &str, message: String| SQLError::Routine {
        sqlstate: sqlstate.into(),
        message,
    };
    if let Some(oid) = parse_dash_or_oid(name)? {
        return Ok(oid);
    }
    let names = uqa_sql::parse_regobject_name(name)
        .ok_or_else(|| input_error("42602", "invalid name syntax".into()))?;
    let (schema, local) = object_name(&names)?;
    let catalog = regtype_output_catalog(context)?;
    let namespace_oid = |schema: &str| {
        catalog
            .namespaces
            .iter()
            .find_map(|(oid, name)| (name == schema).then_some(*oid))
    };
    let mut visible = BTreeMap::<Vec<i64>, i64>::new();
    if let Some(schema) = schema {
        let namespace = namespace_oid(schema)
            .ok_or_else(|| input_error("3F000", format!("schema \"{schema}\" does not exist")))?;
        for (oid, entry) in &catalog.procs {
            if entry.namespace_oid == namespace && entry.name == *local {
                visible.entry(entry.argument_types.clone()).or_insert(*oid);
            }
        }
    } else {
        // A routine hides one with the same arguments later in the search path.
        for schema in context
            .current_schema_names(true)
            .map_err(|error| SQLError::Internal(error.to_string()))?
        {
            let Some(namespace) = namespace_oid(&schema) else {
                continue;
            };
            for (oid, entry) in &catalog.procs {
                if entry.namespace_oid == namespace && entry.name == *local {
                    visible.entry(entry.argument_types.clone()).or_insert(*oid);
                }
            }
        }
    }
    let mut matches = visible.into_values();
    match (matches.next(), matches.next()) {
        (Some(oid), None) => Ok(oid),
        (None, _) => Err(input_error(
            "42883",
            format!("function \"{name}\" does not exist"),
        )),
        (Some(_), Some(_)) => Err(input_error(
            "42725",
            format!("more than one function named \"{name}\""),
        )),
    }
}

fn lookup_regnamespace_oid(
    context: &CatalogContext<'_>,
    name: &str,
) -> Result<Option<i64>, SQLError> {
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
    let catalog = regtype_output_catalog(context)?;
    Ok(catalog
        .namespaces
        .iter()
        .find_map(|(oid, schema)| (schema == name).then_some(*oid)))
}

pub fn resolve_regnamespace_oid(
    context: &CatalogContext<'_>,
    input: &str,
) -> Result<Option<i64>, SQLError> {
    if let Some(oid) = parse_dash_or_oid(input)? {
        return Ok(Some(oid));
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
    let catalog = regtype_output_catalog(context)?;
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

pub fn resolve_regtype_oid(
    context: &CatalogContext<'_>,
    name: &str,
) -> Result<Option<i64>, SQLError> {
    if let Some(oid) = parse_dash_or_oid(name)? {
        return Ok(Some(oid));
    }
    let Some(parsed) = uqa_sql::parse_regtype_name(name)? else {
        return Ok(None);
    };
    let catalog = regtype_output_catalog(context)?;
    parsed_regtype_oid(context, &catalog, &parsed)
}

enum ParsedRegroleInput {
    Oid(i64),
    Name(String),
}

fn parse_regrole_input(input: &str) -> Result<ParsedRegroleInput, SQLError> {
    if let Some(oid) = parse_dash_or_oid(input)? {
        return Ok(ParsedRegroleInput::Oid(oid));
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

pub fn resolve_regrole_oid(
    context: &CatalogContext<'_>,
    input: &str,
) -> Result<Option<i64>, SQLError> {
    match parse_regrole_input(input)? {
        ParsedRegroleInput::Oid(oid) => Ok(Some(oid)),
        ParsedRegroleInput::Name(name) => {
            let catalog = context.catalog_read_view();
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

fn lookup_regrole_oid(context: &CatalogContext<'_>, name: &str) -> Result<Option<i64>, SQLError> {
    match resolve_regrole_oid(context, name) {
        Ok(oid) => Ok(oid),
        Err(SQLError::Routine { .. }) => Ok(None),
        Err(error) => Err(error),
    }
}

pub fn resolve_regobject_oid(
    context: &CatalogContext<'_>,
    ty: &ColumnType,
    name: &str,
) -> Result<Option<i64>, SQLError> {
    match ty {
        ColumnType::Regproc => lookup_regproc_oid(context, name),
        ColumnType::Regprocedure => lookup_regprocedure_oid(context, name),
        ColumnType::Regclass => lookup_regclass_oid(context, name),
        ColumnType::Regnamespace => lookup_regnamespace_oid(context, name),
        ColumnType::Regcollation => collations::lookup_regcollation_oid(context, name),
        ColumnType::Regrole => lookup_regrole_oid(context, name),
        ColumnType::Regtype => match resolve_regtype_oid(context, name) {
            Err(SQLError::Routine { sqlstate, .. })
                if matches!(sqlstate.as_str(), "3F000" | "22P02" | "22003") =>
            {
                Ok(None)
            }
            result => result,
        },
        _ => Err(SQLError::Internal(format!(
            "unsupported regobject lookup type `{}`",
            ty.sql_name()
        ))),
    }
}

fn catalog_int(row: &ResultRow, column: &str) -> Option<i64> {
    match row.get(column) {
        Some(Value::Int(value)) => Some(*value),
        _ => None,
    }
}

fn catalog_int_list(row: &ResultRow, column: &str) -> Option<Vec<i64>> {
    let Some(Value::LegacyVector(vector)) = row.get(column) else {
        return None;
    };
    vector
        .elements()
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

#[cfg(test)]
mod tests;

/// One immutable catalog snapshot shared by every `reg*` value formatted until catalog state changes.
#[derive(Debug)]
pub struct RegtypeOutputCatalog {
    namespaces: BTreeMap<i64, String>,
    classes: BTreeMap<i64, RegtypeCatalogEntry>,
    procs: BTreeMap<i64, RegtypeCatalogEntry>,
    proc_names_by_namespace: BTreeMap<i64, BTreeSet<String>>,
    types: BTreeMap<i64, RegtypeCatalogEntry>,
    dependencies: super::dependencies::DependencyCatalogCache,
}

impl RegtypeOutputCatalog {
    fn build(context: &CatalogContext<'_>) -> Result<Self, SQLError> {
        let catalog = context.catalog_read_view();
        let catalog = catalog.metadata_view();
        // Output functions read catalog rows, which name relations canonically; they need no privilege on their schemas, as `regclassout` and `regtypeout` do not.
        let mut resolution = context.session_execution_view().relation_name_resolution();
        resolution.set_lookup_mode(crate::catalog::RelationLookupMode::Bound);
        let classes = relation_catalog_identities(&catalog, &resolution)?
            .into_iter()
            .map(|entry| {
                (
                    entry.oid,
                    RegtypeCatalogEntry {
                        name: entry.relation.name,
                        namespace_oid: super::helpers::oids::namespace_oid(
                            &catalog,
                            &entry.relation.schema,
                        ),
                        overloaded: false,
                        argument_types: Vec::new(),
                        array_oid: 0,
                        element_oid: 0,
                    },
                )
            })
            .collect();
        Self::assemble(&catalog, &resolution, classes)
    }

    /// The catalog without its relations, for the output of `regtype`, `regproc`, `regprocedure` and `regnamespace` constants where a catalog view and the session's name resolution are at hand without a catalog context.
    pub(crate) fn without_relations(
        catalog: &CatalogReadView,
        resolution: &RelationNameResolution,
    ) -> Result<Self, SQLError> {
        let catalog = catalog.metadata_view();
        let mut resolution = resolution.clone();
        resolution.set_lookup_mode(crate::catalog::RelationLookupMode::Bound);
        Self::assemble(&catalog, &resolution, BTreeMap::new())
    }

    fn assemble(
        catalog: &CatalogReadView,
        resolution: &RelationNameResolution,
        classes: BTreeMap<i64, RegtypeCatalogEntry>,
    ) -> Result<Self, SQLError> {
        let namespaces = build_pg_namespace(catalog)?
            .into_iter()
            .filter_map(|row| {
                Some((
                    catalog_int(&row, "oid")?,
                    catalog_str(&row, "nspname")?.to_string(),
                ))
            })
            .collect();
        let mut procs = BTreeMap::new();
        let mut proc_name_counts = BTreeMap::new();
        let mut proc_names_by_namespace = BTreeMap::<i64, BTreeSet<String>>::new();
        for row in build_pg_proc_without_defaults(catalog, resolution)? {
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

        let types = build_pg_type_without_defaults(catalog, resolution)?
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
            dependencies: crate::catalog::projection::DependencyCatalogCache::default(),
        })
    }
}

impl RegtypeOutputCatalog {
    /// The OID of the namespace named `name`.
    fn namespace_oid(&self, name: &str) -> Option<i64> {
        self.namespaces
            .iter()
            .find_map(|(oid, schema)| (schema == name).then_some(*oid))
    }
}

/// The namespaces the output functions treat as visible: `current_schemas(true)` for routine names, as `regprocout` and `regprocedureout` test a function's visibility, and the search path for type names, as `format_type` tests a type's.
pub(crate) struct OutputVisibility {
    schemas: Vec<String>,
}

impl OutputVisibility {
    fn from_context(context: &CatalogContext<'_>) -> Result<Self, SQLError> {
        Ok(Self {
            schemas: context.current_schema_names(true)?,
        })
    }

    fn from_resolution(catalog: &CatalogReadView, resolution: &RelationNameResolution) -> Self {
        Self {
            schemas: crate::catalog::namespaces::current_schema_names(
                catalog,
                resolution,
                &resolution.current_user,
                true,
            ),
        }
    }
}

/// The output of `regtype`, `regproc`, `regprocedure` and `regnamespace` constants in reconstructed SQL, built from a catalog view and the session's name resolution when a deparser first prints one.
pub(crate) struct AliasConstantOutput {
    catalog: RegtypeOutputCatalog,
    visibility: OutputVisibility,
}

impl AliasConstantOutput {
    pub(crate) fn build(
        catalog: &CatalogReadView,
        resolution: &RelationNameResolution,
    ) -> Result<Self, SQLError> {
        Ok(Self {
            catalog: RegtypeOutputCatalog::without_relations(catalog, resolution)?,
            visibility: OutputVisibility::from_resolution(catalog, resolution),
        })
    }

    /// Names a selected catalog signature using the same visibility rule as `regprocedure` output.
    pub(crate) fn routine_name_parts(
        &self,
        schema: &str,
        name: &str,
        argument_types: &[i64],
    ) -> Option<Vec<String>> {
        procedures::signature_name_parts(
            &self.visibility,
            &self.catalog,
            schema,
            name,
            argument_types,
        )
    }

    /// The constant's output text, or `None` when no object holds the OID or the type is not one this output prints.
    pub(crate) fn text(&self, ty: &ColumnType, oid: i64) -> Option<String> {
        match ty {
            ColumnType::Regtype => format_regtype(&self.visibility, &self.catalog, oid),
            ColumnType::Regcollation => collations::format_regcollation(&self.visibility, oid),
            ColumnType::Regproc => format_regproc(&self.visibility, &self.catalog, oid),
            ColumnType::Regprocedure => format_regprocedure(&self.visibility, &self.catalog, oid),
            ColumnType::Regnamespace => {
                namespace_name(&self.catalog, oid).map(uqa_sql::expr::quote_ident)
            }
            _ => None,
        }
    }
}

/// Object descriptions share the same retained dependency generation as virtual catalog rows.
pub(crate) fn catalog_dependencies(
    context: &CatalogContext<'_>,
) -> Result<Arc<super::CatalogDependencies>, SQLError> {
    // Preserve output-metadata validation before object-description dependency analysis.
    let output = regtype_output_catalog(context)?;
    output.dependencies.get_or_try_init(|| {
        let view = context.catalog_read_view();
        let resolution = context.session_execution_view().relation_name_resolution();
        super::dependencies::retained_dependencies(context, &view, &resolution)
    })
}

fn regtype_output_catalog(
    context: &CatalogContext<'_>,
) -> Result<Arc<RegtypeOutputCatalog>, SQLError> {
    context
        .cache
        .get_or_try_init(|| RegtypeOutputCatalog::build(context))
}

/// The type whose privileges govern `oid`: a generated array type answers with its element type, as `pg_type_aclmask` does. `None` when no type has the OID.
pub fn type_privilege_oid(context: &CatalogContext<'_>, oid: i64) -> Result<Option<i64>, SQLError> {
    let catalog = regtype_output_catalog(context)?;
    let Some(entry) = catalog.types.get(&oid) else {
        return Ok(None);
    };
    if catalog
        .types
        .get(&entry.element_oid)
        .is_some_and(|element| element.array_oid == oid)
    {
        return Ok(Some(entry.element_oid));
    }
    Ok(Some(oid))
}

pub fn routine_oid_exists(context: &CatalogContext<'_>, oid: i64) -> Result<bool, SQLError> {
    Ok(regtype_output_catalog(context)?.procs.contains_key(&oid))
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

fn visible_relation_schema(
    context: &CatalogContext<'_>,
    local: &str,
) -> Result<Option<String>, SQLError> {
    context
        .try_resolve_visible_relation_kind(&uqa_sql::expr::quote_ident(local))?
        .map(|(canonical, _)| {
            uqa_core::RelationIdentity::from_legacy_name(&canonical)
                .map(|identity| identity.schema)
                .map_err(SQLError::Internal)
        })
        .transpose()
}

fn relation_name_is_visible(
    context: &CatalogContext<'_>,
    schema: &str,
    local: &str,
) -> Result<bool, SQLError> {
    Ok(visible_relation_schema(context, local)?.as_deref() == Some(schema))
}

fn format_regclass(
    context: &CatalogContext<'_>,
    catalog: &RegtypeOutputCatalog,
    oid: i64,
) -> Result<Option<String>, SQLError> {
    if let Some(relation) =
        uqa_sql::catalog::SystemRelation::all().find(|relation| relation.oid() == oid)
    {
        let schema = relation.namespace();
        let local = relation.name();
        return Ok(Some(if relation_name_is_visible(context, schema, local)? {
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
        if relation_name_is_visible(context, schema, &entry.name)? {
            uqa_sql::expr::quote_ident(&entry.name)
        } else {
            qualified_name(schema, &entry.name)
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

fn format_regtype(
    visibility: &OutputVisibility,
    catalog: &RegtypeOutputCatalog,
    oid: i64,
) -> Option<String> {
    let entry = catalog.types.get(&oid)?;
    if catalog
        .types
        .get(&entry.element_oid)
        .is_some_and(|element| element.array_oid == oid)
    {
        return format_regtype(visibility, catalog, entry.element_oid)
            .map(|name| format!("{name}[]"));
    }
    let schema = namespace_name(catalog, entry.namespace_oid)?;
    let local = if schema == "pg_catalog" {
        pg_catalog_type_output(&entry.name)
    } else {
        uqa_sql::expr::quote_ident(&entry.name)
    };
    let visible_schema = visibility.schemas.iter().find(|candidate_schema| {
        catalog
            .namespace_oid(candidate_schema)
            .is_some_and(|namespace| {
                catalog.types.values().any(|candidate| {
                    candidate.namespace_oid == namespace && candidate.name == entry.name
                })
            })
    });
    Some(
        if schema == "pg_catalog" || visible_schema.map(String::as_str) == Some(schema) {
            local
        } else {
            format!("{}.{}", uqa_sql::expr::quote_ident(schema), local)
        },
    )
}

fn format_regrole(context: &CatalogContext<'_>, oid: i64) -> Option<String> {
    let catalog = context.catalog_read_view();
    let name = catalog
        .roles()
        .find_map(|role| (role.oid == oid).then(|| uqa_sql::expr::quote_ident(&role.name)));
    name
}

pub fn resolve_regtype_output(
    context: &CatalogContext<'_>,
    ty: &ColumnType,
    oid: i64,
) -> Result<Option<String>, String> {
    if !matches!(
        ty,
        ColumnType::Regproc
            | ColumnType::Regprocedure
            | ColumnType::Regclass
            | ColumnType::Regcollation
            | ColumnType::Regnamespace
            | ColumnType::Regrole
            | ColumnType::Regtype
    ) {
        return Ok(None);
    }
    let catalog = regtype_output_catalog(context).map_err(|error| error.to_string())?;
    let visibility = OutputVisibility::from_context(context).map_err(|error| error.to_string())?;
    let output = match ty {
        ColumnType::Regproc => Ok(format_regproc(&visibility, &catalog, oid)),
        ColumnType::Regprocedure => Ok(format_regprocedure(&visibility, &catalog, oid)),
        ColumnType::Regclass => format_regclass(context, &catalog, oid),
        ColumnType::Regcollation => Ok(collations::format_regcollation(&visibility, oid)),
        ColumnType::Regnamespace => {
            Ok(namespace_name(&catalog, oid).map(uqa_sql::expr::quote_ident))
        }
        ColumnType::Regrole => Ok(format_regrole(context, oid)),
        ColumnType::Regtype => Ok(format_regtype(&visibility, &catalog, oid)),
        _ => unreachable!(),
    };
    output.map_err(|error| error.to_string())
}
