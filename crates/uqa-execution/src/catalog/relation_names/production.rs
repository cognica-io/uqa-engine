//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relation binding borrows immutable registries and admits candidate names and the selected result.

use super::{RelationLookupMode, RelationNameCatalog, RelationNameResolution, RelationResolution};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    RelationIdentity, RelationName, RelationReference,
};
use uqa_sql::{
    catalog::security::schema::{role_has_schema_privilege_with_control, SchemaAclPrivilege},
    SQLError,
};

impl RelationNameCatalog<'_> {
    pub fn relation_kind_resolution_with_control(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<RelationResolution>, SQLError> {
        control.check()?;
        if resolution.lookup_mode() == RelationLookupMode::Dynamic {
            let reference = RelationIdentity::try_borrowed_reference(name).map_err(|error| {
                formatted(
                    format_args!("resolve catalog relation `{name}`: {error}"),
                    control,
                    SQLError::Internal,
                )
            })?;
            if let Some(schema) = reference.schema {
                let requested = control.format(format_args!("{schema}"))?;
                let resolved = if requested.as_str() == "pg_temp" {
                    resolution.temporary_schema.as_str()
                } else {
                    requested.as_str()
                };
                if !self.namespace_exists(resolution, resolved) {
                    let (schema, memory) = requested.into_parts();
                    return Ok(control.finish(RelationResolution::MissingSchema(schema), memory)?);
                }
            }
        }
        let candidates = self.relation_lookup_candidates_with_control(resolution, name, control)?;
        for relation in candidates.iter() {
            control.check()?;
            let kind = self.kind_at(relation);
            let canonical = RelationReference {
                schema: Some(RelationName::decoded(&relation.schema)),
                name: RelationName::decoded(&relation.name),
            }
            .render_with_control(control)?;
            let kind = kind.or_else(|| {
                self.definitions
                    .composites
                    .contains_key(canonical.as_str())
                    .then_some("composite type")
            });
            if let Some(kind) = kind {
                let (name, memory) = canonical.into_parts();
                return Ok(control.finish(RelationResolution::Found(name, kind), memory)?);
            }
        }
        Ok(control.finish(
            RelationResolution::MissingRelation,
            control.empty_reservation(),
        )?)
    }

    pub(in crate::catalog) fn relation_lookup_candidates_with_control(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Vec<RelationIdentity>>, SQLError> {
        control.check()?;
        let bound = resolution.lookup_mode() == RelationLookupMode::Bound;
        let reference = RelationIdentity::try_borrowed_reference(name).map_err(|error| {
            if bound {
                formatted(
                    format_args!("decode bound relation `{name}`: {error}"),
                    control,
                    SQLError::Internal,
                )
            } else {
                formatted(format_args!("{error}"), control, SQLError::Internal)
            }
        })?;
        let mut output = ProductionVec::new(*control);
        if bound {
            let schema = reference.schema.ok_or_else(|| {
                formatted(
                    format_args!("bound query contains non-canonical relation reference `{name}`"),
                    control,
                    SQLError::Internal,
                )
            })?;
            output.push_produced(identity(schema, reference.name, control)?)?;
        } else if let Some(schema) = reference.schema {
            let schema = if schema == RelationName::decoded("pg_temp") {
                RelationName::decoded(&resolution.temporary_schema)
            } else {
                schema
            };
            let relation = identity(schema, reference.name, control)?;
            if self.permitted(resolution, &relation.schema, true, control)? {
                output.push_produced(relation)?;
            }
        } else {
            let path = resolution.search_path();
            let temporary = (!path
                .iter()
                .any(|schema| schema == "pg_temp" || schema == &resolution.temporary_schema))
            .then_some(resolution.temporary_schema.as_str());
            let catalog =
                (!path.iter().any(|schema| schema == "pg_catalog")).then_some("pg_catalog");
            for schema in temporary
                .into_iter()
                .chain(catalog)
                .chain(path.iter().map(|schema| {
                    if schema == "pg_temp" {
                        resolution.temporary_schema.as_str()
                    } else {
                        schema.as_str()
                    }
                }))
            {
                control.check()?;
                if self.permitted(resolution, schema, false, control)? {
                    output.push_produced(identity(
                        RelationName::decoded(schema),
                        reference.name,
                        control,
                    )?)?;
                }
            }
        }
        Ok(output.finish()?)
    }

    fn kind_at(&self, relation: &RelationIdentity) -> Option<&'static str> {
        if let Some(relation) =
            uqa_sql::catalog::SystemRelation::at(&relation.schema, &relation.name)
        {
            Some(relation.kind())
        } else if self.tables.contains_table(relation) {
            Some("table")
        } else if let Some(view) = self.definitions.views.get(relation) {
            Some(match view.kind {
                crate::catalog::view::StoredViewKind::View => "view",
                crate::catalog::view::StoredViewKind::Materialized => "materialized view",
            })
        } else if self.definitions.sequences.contains_key(relation) {
            Some("sequence")
        } else if self.definitions.foreign_tables.contains_key(relation) {
            Some("foreign table")
        } else if self.definitions.catalog_indexes.contains_key(relation) {
            Some("index")
        } else {
            None
        }
    }

    fn permitted(
        &self,
        resolution: &RelationNameResolution,
        schema: &str,
        qualified: bool,
        control: &ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        let Some(security) = self.definitions.schemas.get(schema) else {
            return Ok(true);
        };
        let allowed = match role_has_schema_privilege_with_control(
            security.into(),
            &resolution.current_user,
            SchemaAclPrivilege::Usage,
            &self.definitions.roles,
            &self.definitions.role_memberships,
            control,
        ) {
            // Name lookup treats invalid persisted ACL references as an unusable namespace.
            Err(SQLError::Internal(_)) => false,
            result => result?,
        };
        if !allowed && qualified {
            return Err(formatted(
                format_args!("permission denied for schema {schema}"),
                control,
                |message| SQLError::Routine {
                    sqlstate: "42501".into(),
                    message,
                },
            ));
        }
        Ok(allowed)
    }
}

fn identity(
    schema: RelationName<'_>,
    name: RelationName<'_>,
    control: &ProductionControl<'_>,
) -> Result<Produced<RelationIdentity>, SQLError> {
    let schema = control.format(format_args!("{schema}"))?;
    let name = control.format(format_args!("{name}"))?;
    let (schema, schema_memory) = schema.into_parts();
    let (name, name_memory) = name.into_parts();
    Ok(control.finish(
        RelationIdentity { schema, name },
        control.combine(schema_memory, name_memory),
    )?)
}

fn formatted(
    message: std::fmt::Arguments<'_>,
    control: &ProductionControl<'_>,
    build: impl FnOnce(String) -> SQLError,
) -> SQLError {
    match control.format(message) {
        Ok(message) => build(message.into_parts().0),
        Err(error) => error.into(),
    }
}
