//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact routine lookup and signature-aware catalog output names.

use super::{
    format_regtype, namespace_name, object_name, parse_dash_or_oid, parsed_regtype_oid,
    qualified_name, regtype_output_catalog, OutputVisibility, RegtypeCatalogEntry,
    RegtypeOutputCatalog,
};
use crate::catalog::{context::CatalogContext, security::schema::SchemaAclPrivilege};
use uqa_sql::SQLError;

pub(super) fn lookup_regprocedure_oid(
    context: &CatalogContext<'_>,
    name: &str,
) -> Result<Option<i64>, SQLError> {
    match resolve_regprocedure_input_oid(context, name) {
        Ok(oid) => Ok(Some(oid)),
        Err(SQLError::Parse(_)) => Ok(None),
        Err(SQLError::Routine { sqlstate, .. })
            if matches!(
                sqlstate.as_str(),
                "22P02" | "22003" | "42704" | "42883" | "42602" | "54023"
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

pub fn resolve_regprocedure_input_oid(
    context: &CatalogContext<'_>,
    name: &str,
) -> Result<i64, SQLError> {
    if let Some(oid) = parse_dash_or_oid(name)? {
        return Ok(oid);
    }
    let parsed = uqa_sql::parse_regprocedure_name(name)?
        .ok_or_else(|| input_error("42602", format!("invalid name syntax: \"{name}\"")))?;
    let arguments = parsed
        .argument_types
        .as_ref()
        .ok_or_else(|| input_error("22P02", "expected a left parenthesis".into()))?;
    let names = match parsed.names.as_slice() {
        [database, ..]
            if parsed.names.len() == 3 && database == uqa_sql::catalog::DATABASE_NAME =>
        {
            &parsed.names[1..]
        }
        names => names,
    };
    let (schema, local) = object_name(names)?;
    let catalog = regtype_output_catalog(context)?;
    let argument_oids = arguments
        .iter()
        .map(|argument| {
            parsed_regtype_oid(context, &catalog, argument)?.ok_or_else(|| {
                input_error(
                    "42704",
                    format!("type \"{}\" does not exist", argument.names.join(".")),
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let find_in_schema = |schema: &str| {
        let namespace_oid = catalog
            .namespaces
            .iter()
            .find_map(|(oid, name)| (name == schema).then_some(*oid))?;
        catalog.procs.iter().find_map(|(oid, entry)| {
            (entry.namespace_oid == namespace_oid
                && entry.name == local
                && entry.argument_types == argument_oids)
                .then_some(*oid)
        })
    };
    let oid = if let Some(schema) = schema {
        if context.schema_security_for_privilege(schema).is_some() {
            context.require_schema_privilege(
                schema,
                &context.current_role(),
                SchemaAclPrivilege::Usage,
            )?;
        }
        find_in_schema(schema)
    } else {
        context
            .current_schema_names(true)?
            .iter()
            .find_map(|schema| find_in_schema(schema))
    };
    oid.ok_or_else(|| input_error("42883", format!("function \"{name}\" does not exist")))
}

fn input_error(sqlstate: &str, message: String) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message,
    }
}

pub(super) fn format_regproc(
    visibility: &OutputVisibility,
    catalog: &RegtypeOutputCatalog,
    oid: i64,
) -> Option<String> {
    let entry = catalog.procs.get(&oid)?;
    let schema = namespace_name(catalog, entry.namespace_oid)?;
    let visible_schema = visibility.schemas.iter().find(|candidate_schema| {
        catalog
            .namespace_oid(candidate_schema)
            .and_then(|candidate_oid| catalog.proc_names_by_namespace.get(&candidate_oid))
            .is_some_and(|names| names.contains(entry.name.as_str()))
    });
    Some(
        if !entry.overloaded && visible_schema.map(String::as_str) == Some(schema) {
            uqa_sql::expr::quote_ident(&entry.name)
        } else {
            qualified_name(schema, &entry.name)
        },
    )
}

/// The selected routine's decoded name parts; quoting belongs to the SQL renderer.
pub fn routine_name_parts(
    context: &CatalogContext<'_>,
    oid: i64,
) -> Result<Option<Vec<String>>, SQLError> {
    let catalog = regtype_output_catalog(context)?;
    let visibility = OutputVisibility::from_context(context)?;
    Ok(catalog
        .procs
        .get(&oid)
        .and_then(|entry| entry_name_parts(&visibility, &catalog, entry)))
}

pub(super) fn signature_name_parts(
    visibility: &OutputVisibility,
    catalog: &RegtypeOutputCatalog,
    schema: &str,
    name: &str,
    argument_types: &[i64],
) -> Option<Vec<String>> {
    let namespace = catalog.namespace_oid(schema)?;
    let entry = catalog.procs.values().find(|entry| {
        entry.namespace_oid == namespace
            && entry.name == name
            && entry.argument_types == argument_types
    })?;
    entry_name_parts(visibility, catalog, entry)
}

fn entry_name_parts(
    visibility: &OutputVisibility,
    catalog: &RegtypeOutputCatalog,
    entry: &RegtypeCatalogEntry,
) -> Option<Vec<String>> {
    let schema = namespace_name(catalog, entry.namespace_oid)?;
    let visible_schema = visibility.schemas.iter().find(|candidate_schema| {
        catalog
            .namespace_oid(candidate_schema)
            .is_some_and(|namespace_oid| {
                catalog.procs.values().any(|candidate| {
                    candidate.namespace_oid == namespace_oid
                        && candidate.name == entry.name
                        && candidate.argument_types == entry.argument_types
                })
            })
    });
    Some(if visible_schema.map(String::as_str) == Some(schema) {
        vec![entry.name.clone()]
    } else {
        vec![schema.to_owned(), entry.name.clone()]
    })
}

pub(super) fn format_regprocedure(
    visibility: &OutputVisibility,
    catalog: &RegtypeOutputCatalog,
    oid: i64,
) -> Option<String> {
    let entry = catalog.procs.get(&oid)?;
    let routine_name = entry_name_parts(visibility, catalog, entry)?
        .iter()
        .map(|part| uqa_sql::expr::quote_ident(part))
        .collect::<Vec<_>>()
        .join(".");
    let arguments = entry
        .argument_types
        .iter()
        .map(|oid| format_regtype(visibility, catalog, *oid).unwrap_or_else(|| oid.to_string()))
        .collect::<Vec<_>>();
    Some(format!("{routine_name}({})", arguments.join(",")))
}

#[cfg(test)]
mod tests {
    use super::{
        format_regproc, format_regprocedure, OutputVisibility, RegtypeCatalogEntry,
        RegtypeOutputCatalog,
    };
    use crate::catalog::projection::regtypes::AliasConstantOutput;
    use std::collections::{BTreeMap, BTreeSet};

    fn entry(
        name: &str,
        namespace: i64,
        arguments: &[i64],
        overloaded: bool,
    ) -> RegtypeCatalogEntry {
        RegtypeCatalogEntry {
            name: name.into(),
            namespace_oid: namespace,
            overloaded,
            argument_types: arguments.to_vec(),
            array_oid: 0,
            element_oid: 0,
        }
    }

    fn output() -> AliasConstantOutput {
        AliasConstantOutput {
            catalog: RegtypeOutputCatalog {
                namespaces: BTreeMap::from([
                    (11, "pg_catalog".into()),
                    (20_000, "body_shadow".into()),
                ]),
                classes: BTreeMap::new(),
                procs: BTreeMap::from([
                    (720, entry("octet_length", 11, &[17], true)),
                    (1374, entry("octet_length", 11, &[25], true)),
                    (20_001, entry("octet_length", 20_000, &[25], false)),
                ]),
                proc_names_by_namespace: BTreeMap::from([
                    (11, BTreeSet::from(["octet_length".into()])),
                    (20_000, BTreeSet::from(["octet_length".into()])),
                ]),
                types: BTreeMap::from([
                    (17, entry("bytea", 11, &[], false)),
                    (25, entry("text", 11, &[], false)),
                ]),
                dependencies: crate::catalog::projection::DependencyCatalogCache::default(),
            }
            .into(),
            visibility: OutputVisibility {
                schemas: vec!["body_shadow".into(), "pg_catalog".into()],
            },
        }
    }

    #[test]
    fn selected_names_share_regprocedure_visibility_and_ignore_other_overloads() {
        let mut output = output();
        for (oid, argument, names, procedure) in [
            (
                720,
                17,
                vec!["octet_length".to_owned()],
                "octet_length(bytea)",
            ),
            (
                1374,
                25,
                vec!["pg_catalog".to_owned(), "octet_length".to_owned()],
                "pg_catalog.octet_length(text)",
            ),
        ] {
            assert_eq!(
                output.routine_name_parts("pg_catalog", "octet_length", &[argument]),
                Some(names)
            );
            assert_eq!(
                format_regprocedure(&output.visibility, &output.catalog, oid).as_deref(),
                Some(procedure)
            );
        }
        assert_eq!(
            format_regproc(&output.visibility, &output.catalog, 720).as_deref(),
            Some("pg_catalog.octet_length")
        );
        assert_eq!(
            output.routine_name_parts("pg_catalog", "octet_length", &[23]),
            None
        );
        assert_eq!(
            output.routine_name_parts("missing", "octet_length", &[17]),
            None
        );
        output.visibility.schemas.reverse();
        assert_eq!(
            output.routine_name_parts("pg_catalog", "octet_length", &[25]),
            Some(vec!["octet_length".to_owned()])
        );
        assert_eq!(
            output.routine_name_parts("body_shadow", "octet_length", &[25]),
            Some(vec!["body_shadow".to_owned(), "octet_length".to_owned()])
        );
    }

    #[test]
    fn name_parts_preserve_identifiers_until_the_caller_quotes_them() {
        let mut output = output();
        std::sync::Arc::get_mut(&mut output.catalog)
            .unwrap()
            .procs
            .insert(20_002, entry("a.b \"quoted\"", 20_000, &[], false));
        let names = output
            .routine_name_parts("body_shadow", "a.b \"quoted\"", &[])
            .unwrap();
        assert_eq!(names, vec!["a.b \"quoted\"".to_owned()]);
        assert_eq!(
            format_regprocedure(&output.visibility, &output.catalog, 20_002).as_deref(),
            Some("\"a.b \"\"quoted\"\"\"()")
        );
    }
}
