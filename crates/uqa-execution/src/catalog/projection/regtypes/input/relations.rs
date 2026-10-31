//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relation input selects the same namespace candidate order while borrowing durable addresses.

use super::{
    formatted, routine, CatalogContext, CatalogReadView, ProductionControl, QualifiedNames,
    SQLError,
};
use crate::catalog::{namespaces, security::schema::SchemaAclPrivilege};
use uqa_core::{memory::Produced, RelationIdentity, RelationName, RelationReference};
use uqa_sql::catalog::resolution::{RelationLookupMode, RelationNameResolution};

pub(super) fn resolve(
    context: &CatalogContext<'_>,
    input: &str,
    control: &ProductionControl<'_>,
) -> Result<i64, SQLError> {
    let names = uqa_sql::parse_regobject_name_with_control(input, control)?
        .ok_or_else(|| routine("42602", format_args!("invalid name syntax"), control))?;
    let (schema, local) = match names.as_slice() {
        [local] => (None, local.as_str()),
        [schema, local] => (Some(schema.as_str()), local.as_str()),
        [_, _, _] => {
            return Err(formatted(
                format_args!(
                    "cross-database references are not implemented: \"{}\"",
                    QualifiedNames(&names)
                ),
                control,
                SQLError::Unsupported,
            ))
        }
        _ => {
            return Err(formatted(
                format_args!(
                    "improper relation name (too many dotted names): {}",
                    QualifiedNames(&names)
                ),
                control,
                SQLError::Parse,
            ))
        }
    };
    let catalog = context.catalog.refreshed_catalog_snapshot()?;
    let resolution = context
        .session
        .relation_name_resolution_with_control(control)?;
    find(
        &catalog,
        &resolution,
        RelationLookupMode::Dynamic,
        schema,
        local,
        control,
    )?
    .ok_or_else(|| {
        routine(
            "42P01",
            format_args!("relation \"{}\" does not exist", QualifiedNames(&names)),
            control,
        )
    })
}

pub fn resolved_relation_dependency_with_control(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<Option<u32>, SQLError> {
    control.check()?;
    let reference = RelationIdentity::try_borrowed_reference(name).map_err(|error| {
        formatted(
            format_args!("resolve catalog relation `{name}`: {error}"),
            control,
            SQLError::Internal,
        )
    })?;
    let schema = reference
        .schema
        .map(|schema| control.format(format_args!("{schema}")))
        .transpose()?;
    let local = control.format(format_args!("{}", reference.name))?;
    find(
        catalog,
        resolution,
        resolution.lookup_mode,
        schema.as_deref().map(String::as_str),
        &local,
        control,
    )?
    .map(|oid| {
        u32::try_from(oid).map_err(|_| {
            formatted(
                format_args!("invalid relation OID {oid}"),
                control,
                SQLError::Internal,
            )
        })
    })
    .transpose()
}

fn find(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    lookup_mode: RelationLookupMode,
    schema: Option<&str>,
    local: &str,
    control: &ProductionControl<'_>,
) -> Result<Option<i64>, SQLError> {
    if lookup_mode == RelationLookupMode::Bound {
        let schema = schema.ok_or_else(|| {
            let reference = RelationReference {
                schema: None,
                name: RelationName::decoded(local),
            }
            .render_with_control(control);
            match reference {
                Ok(reference) => formatted(
                    format_args!(
                        "bound query contains non-canonical relation reference `{}`",
                        reference.as_str()
                    ),
                    control,
                    SQLError::Internal,
                ),
                Err(error) => error.into(),
            }
        })?;
        if let Some(oid) = at(catalog, schema, local, control)? {
            return Ok(Some(oid));
        }
    } else if let Some(schema) = schema {
        let schema = if schema == "pg_temp" {
            resolution.temporary_schema.as_str()
        } else {
            schema
        };
        let exists = if schema == resolution.temporary_schema {
            resolution.temporary_namespace_allocated
        } else {
            uqa_sql::catalog::is_virtual_system_schema(schema)
                || catalog.snapshot().definitions.schemas.contains_key(schema)
                || catalog.contains_graph(schema)
        };
        if exists && permitted(catalog, resolution, schema, true, control)? {
            if let Some(oid) = at(catalog, schema, local, control)? {
                return Ok(Some(oid));
            }
        }
    } else {
        let path = resolution.search_path();
        let temporary = (!path
            .iter()
            .any(|schema| schema == "pg_temp" || schema == &resolution.temporary_schema))
        .then_some(resolution.temporary_schema.as_str());
        let pg_catalog =
            (!path.iter().any(|schema| schema == "pg_catalog")).then_some("pg_catalog");
        for schema in temporary
            .into_iter()
            .chain(pg_catalog)
            .chain(path.iter().map(|schema| {
                if schema == "pg_temp" {
                    resolution.temporary_schema.as_str()
                } else {
                    schema.as_str()
                }
            }))
        {
            control.check()?;
            if permitted(catalog, resolution, schema, false, control)? {
                if let Some(oid) = at(catalog, schema, local, control)? {
                    return Ok(Some(oid));
                }
            }
        }
    }
    Ok(None)
}

fn permitted(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    schema: &str,
    qualified: bool,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    if catalog.schema_security(schema).is_none() {
        return Ok(true);
    }
    if namespaces::schema_has_privilege_with_control(
        catalog,
        &resolution.temporary_schema,
        schema,
        &resolution.current_user,
        SchemaAclPrivilege::Usage,
        control,
    )? {
        return Ok(true);
    }
    if qualified {
        return Err(routine(
            "42501",
            format_args!("permission denied for schema {schema}"),
            control,
        ));
    }
    Ok(false)
}

fn identity(
    schema: &str,
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<RelationIdentity>, SQLError> {
    let schema = control.copy_text(schema)?;
    let name = control.copy_text(name)?;
    let (schema, schema_memory) = schema.into_parts();
    let (name, name_memory) = name.into_parts();
    Ok(control.finish(
        RelationIdentity { schema, name },
        control.combine(schema_memory, name_memory),
    )?)
}

fn at(
    catalog: &CatalogReadView,
    schema: &str,
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<Option<i64>, SQLError> {
    control.check()?;
    if let Some(relation) = uqa_sql::catalog::SystemRelation::at(schema, name) {
        return Ok(Some(relation.oid()));
    }
    let identity = identity(schema, name, control)?;
    let snapshot = catalog.snapshot();
    if let Some(table) = snapshot.tables.get(&*identity) {
        return Ok(Some(i64::from(table.catalog_oids.relation)));
    }
    if let Some(view) = snapshot.definitions.views.get(&*identity) {
        return Ok(Some(crate::catalog::projection::view_relation_oid(view)));
    }
    if snapshot.definitions.sequences.contains_key(&*identity) {
        return snapshot
            .definitions
            .sequence_object_ids
            .get(&*identity)
            .map(|identity| Some(catalog.sequence_catalog_oid(identity)))
            .ok_or_else(|| {
                formatted(
                    format_args!("resolved sequence `{schema}.{name}` has no object identity"),
                    control,
                    SQLError::Internal,
                )
            });
    }
    if let Some(table) = snapshot.definitions.foreign_tables.get(&*identity) {
        return Ok(Some(
            crate::catalog::projection::foreign_table_relation_oid(table),
        ));
    }
    if let Some(index) = snapshot.definitions.catalog_indexes.get(&*identity) {
        return uqa_sql::catalog::index::stored::catalog_object_identity(
            index.definition_json.as_deref(),
        )
        .map_err(|error| formatted(format_args!("{error}"), control, SQLError::Internal))?
        .map(|identity| Some(identity.oid))
        .ok_or_else(|| {
            formatted(
                format_args!("index `{schema}.{name}` has no catalog identity"),
                control,
                SQLError::Internal,
            )
        });
    }
    let reference = RelationReference {
        schema: Some(RelationName::decoded(schema)),
        name: RelationName::decoded(name),
    }
    .render_with_control(control)?;
    Ok(snapshot
        .definitions
        .composites
        .get(reference.as_str())
        .map(|composite| i64::from(composite.relation_oid)))
}
