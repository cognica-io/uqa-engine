//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog address occupancy without row scans or authority-filtered catalog projections.

use crate::catalog::{CatalogReadView, RelationNameResolution};
use uqa_core::RelationIdentity;
use uqa_sql::ast::ConstraintCatalogIdentity;
use uqa_sql::{
    schema::constraint_metadata::{identity::claims, CatalogOidClass},
    SQLError,
};

mod graphs;
mod relations;
pub(crate) use relations::{legacy_relation_claims, relation_claims};

pub fn catalog_oid_in_use(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    class: CatalogOidClass,
    oid: i64,
) -> Result<bool, SQLError> {
    if graphs::graph_oid_in_use(catalog, class, oid) {
        return Ok(true);
    }
    match class {
        CatalogOidClass::Relation => Ok(relation_claims(catalog, resolution)?
            .iter()
            .any(|claim| claim.oid == oid)),
        CatalogOidClass::Constraint => {
            let snapshot = catalog.snapshot();
            for (relation, table) in &snapshot.tables {
                if claims::has_constraint_oid(
                    relation,
                    &table.columns,
                    &table.checks,
                    &table.keys,
                    &table.foreign_keys,
                    oid,
                ) {
                    return Ok(true);
                }
            }
            for (relation, table) in snapshot.definitions.foreign_tables.iter() {
                if claims::has_constraint_oid(
                    relation,
                    &table.columns,
                    &table.checks,
                    &[],
                    &[],
                    oid,
                ) {
                    return Ok(true);
                }
            }
            if snapshot.definitions.domains.values().any(|domain| {
                uqa_sql::schema::domains::constraints::identities(&domain.definition)
                    .any(|identity| identity.oid == oid)
            }) {
                return Ok(true);
            }
            trigger_address_in_use(catalog, resolution, oid)
        }
        CatalogOidClass::Type => Ok(type_oid_in_use(catalog, oid)),
        CatalogOidClass::EnumLabel => Ok(catalog
            .enums()
            .any(|definition| definition.label_oids().any(|label| i64::from(label) == oid))),
        CatalogOidClass::Rewrite => rewrite_oid_in_use(catalog, oid),
        CatalogOidClass::Procedure => {
            super::routine_oid_in_use(&catalog.snapshot().definitions.sql_user_functions, oid)
        }
        CatalogOidClass::AttributeDefault => {
            Ok(super::pg_catalog::attribute_default_oids(catalog).contains(&oid))
        }
        CatalogOidClass::Trigger => {
            for (trigger, _) in super::events::catalog_triggers(catalog, resolution)? {
                if super::events::trigger_catalog_oid(catalog, resolution, &trigger)? == oid {
                    return Ok(true);
                }
            }
            Ok(false)
        }
    }
}

/// The largest OID a user object holds, past which a database's OID counter starts. Objects created before OIDs were recorded hold the OIDs their identities or names derive, which the catalog projections report.
pub fn largest_catalog_oid(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<Option<u32>, SQLError> {
    let row_oids = |rows: Vec<uqa_sql::ResultRow>| {
        rows.into_iter()
            .filter_map(|row| match row.get("oid") {
                Some(uqa_core::Value::Int(oid)) => Some(*oid),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let snapshot = catalog.snapshot();
    let mut oids = relation_claims(catalog, resolution)?
        .into_iter()
        .filter(|claim| {
            uqa_sql::catalog::SystemRelation::at(&claim.relation.schema, &claim.relation.name)
                .is_none()
        })
        .map(|claim| claim.oid)
        .collect::<Vec<_>>();
    for relation_oids in snapshot
        .tables
        .values()
        .map(|table| table.catalog_oids)
        .chain(
            snapshot
                .definitions
                .views
                .values()
                .map(|view| view.relation_oids()),
        )
        .chain(
            snapshot
                .definitions
                .foreign_tables
                .values()
                .map(crate::catalog::foreign::StoredForeignTable::relation_oids),
        )
    {
        oids.extend(relation_oids.claimed().map(i64::from));
    }
    // Built-in, registered and extension catalog objects are not user objects: only user-defined types, routines and schemas are counted.
    for definition in catalog.enums() {
        oids.extend(
            [definition.oid, definition.array_oid]
                .into_iter()
                .chain(definition.label_oids())
                .map(i64::from),
        );
    }
    for domain in catalog.domains() {
        let ty = domain.column_type();
        oids.push(uqa_sql::catalog::type_metadata::pg_type_oid(&ty));
        oids.push(uqa_sql::catalog::type_metadata::pg_type_array_oid(&ty));
    }
    oids.extend(foreign_object_oids(&snapshot.definitions));
    for function in snapshot.definitions.sql_user_functions.values().flatten() {
        oids.push(super::user_routine_catalog_oid(function)?);
    }
    for graph in snapshot.definitions.graph_catalog_oids.values() {
        oids.extend(graph.claimed().map(i64::from));
    }
    // Bootstrap namespaces have reserved identities, including the extension schema's derived OID; they do not advance the user allocation sequence.
    oids.extend(
        snapshot
            .definitions
            .schemas
            .iter()
            .filter(|(name, _)| {
                !uqa_sql::catalog::security::BoundSchemaSecurity::BUILTIN_NAMES
                    .contains(&name.as_str())
            })
            .filter_map(|(_, security)| security.tuple.map(|tuple| tuple.oid)),
    );
    oids.extend(row_oids(super::pg_catalog::build_pg_constraint(
        catalog, resolution,
    )?));
    oids.extend(super::pg_catalog::attribute_default_oids(catalog));
    oids.extend(row_oids(super::pg_catalog::build_pg_enum(catalog)));
    oids.extend(row_oids(super::pg_catalog::build_pg_authid(catalog)));
    oids.extend(row_oids(super::pg_catalog::build_pg_auth_members(catalog)?));
    oids.extend(super::events::rewrite_catalog_oids(catalog));
    for (trigger, _) in super::events::catalog_triggers(catalog, resolution)? {
        oids.push(super::events::trigger_catalog_oid(
            catalog, resolution, &trigger,
        )?);
        oids.push(super::events::trigger_constraint_catalog_oid(
            catalog, resolution, &trigger,
        )?);
    }
    Ok(oids
        .into_iter()
        .filter_map(|oid| u32::try_from(oid).ok())
        .filter(|oid| *oid >= uqa_sql::catalog::oids::FIRST_NORMAL_OBJECT_ID)
        .max())
}

fn foreign_object_oids(
    definitions: &crate::catalog::CatalogDefinitionSnapshot,
) -> impl Iterator<Item = i64> + '_ {
    definitions
        .foreign_servers
        .values()
        .map(|server| i64::from(server.metadata.oid))
        .chain(
            definitions
                .foreign_wrappers
                .values()
                .map(|wrapper| i64::from(wrapper.identity.oid)),
        )
}

/// User rules and the `_RETURN` rules of views and materialized views occupy `pg_rewrite` OIDs.
fn rewrite_oid_in_use(catalog: &CatalogReadView, oid: i64) -> Result<bool, SQLError> {
    let snapshot = catalog.snapshot();
    Ok(snapshot
        .definitions
        .views
        .values()
        .any(|view| view.relation_oids().rule.map(i64::from) == Some(oid))
        || snapshot
            .definitions
            .rules
            .values()
            .flat_map(|rules| rules.values())
            .any(|rule| super::events::rule_catalog_oid(rule) == oid))
}

/// Every projected `pg_type` row and every relation row type occupies the type OID space.
fn type_oid_in_use(catalog: &CatalogReadView, oid: i64) -> bool {
    if super::pg_catalog::builtin_type_oid_in_use(oid)
        || catalog.domains().any(|domain| {
            i64::from(domain.oid) == oid
                || uqa_sql::catalog::type_metadata::pg_domain_array_oid(
                    domain.oid,
                    domain.array_oid,
                ) == oid
        })
        || catalog.enums().any(|definition| {
            [definition.oid, definition.array_oid]
                .into_iter()
                .any(|claimed| i64::from(claimed) == oid)
        })
        || catalog.composites().any(|definition| {
            [definition.oid, definition.array_oid]
                .into_iter()
                .any(|claimed| i64::from(claimed) == oid)
        })
    {
        return true;
    }
    // Relation row types and their array types occupy `pg_type` OIDs too.
    let claims = |oids: uqa_sql::catalog::relation_oids::RelationCatalogOids| {
        [oids.row_type, oids.array_type]
            .into_iter()
            .flatten()
            .any(|claimed| i64::from(claimed) == oid)
    };
    let snapshot = catalog.snapshot();
    snapshot
        .tables
        .values()
        .any(|table| claims(table.catalog_oids))
        || snapshot
            .definitions
            .views
            .values()
            .any(|view| claims(view.relation_oids()))
        || snapshot
            .definitions
            .foreign_tables
            .values()
            .any(|table| claims(table.relation_oids()))
}

/// Return whether this exact row already belongs to the target, rejecting any other claim on its OID or incarnation.
pub fn validate_catalog_identity_claim(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    target: &RelationIdentity,
    class: CatalogOidClass,
    identity: ConstraintCatalogIdentity,
) -> Result<bool, SQLError> {
    match class {
        // Type, label, rule, routine and column default addresses are always generated by their creating statement.
        CatalogOidClass::Type
        | CatalogOidClass::EnumLabel
        | CatalogOidClass::Rewrite
        | CatalogOidClass::Procedure
        | CatalogOidClass::AttributeDefault
        | CatalogOidClass::Trigger => Err(SQLError::Internal(format!(
            "supplied {} catalog identities are not accepted",
            class.label()
        ))),
        CatalogOidClass::Relation => {
            let mut found = false;
            for claim in relation_claims(catalog, resolution)? {
                if claim.oid == identity.oid || claim.object_id == Some(identity.object_id) {
                    if &claim.relation != target
                        || claim.oid != identity.oid
                        || claim.object_id != Some(identity.object_id)
                        || found
                    {
                        return Err(SQLError::Internal(
                            "supplied relation catalog identity conflicts with an existing row"
                                .into(),
                        ));
                    }
                    found = true;
                }
            }
            Ok(found)
        }
        CatalogOidClass::Constraint => {
            let snapshot = catalog.snapshot();
            let mut found = false;
            for (relation, table) in &snapshot.tables {
                let local = validate_rows(
                    relation,
                    target,
                    identity,
                    claims::row_identities(
                        &table.columns,
                        &table.checks,
                        &table.keys,
                        &table.foreign_keys,
                    ),
                    &mut found,
                )?;
                if !local
                    && claims::has_constraint_oid(
                        relation,
                        &table.columns,
                        &table.checks,
                        &table.keys,
                        &table.foreign_keys,
                        identity.oid,
                    )
                {
                    return Err(conflict());
                }
            }
            for (relation, table) in snapshot.definitions.foreign_tables.iter() {
                let local = validate_rows(
                    relation,
                    target,
                    identity,
                    claims::row_identities(&table.columns, &table.checks, &[], &[]),
                    &mut found,
                )?;
                if !local
                    && claims::has_constraint_oid(
                        relation,
                        &table.columns,
                        &table.checks,
                        &[],
                        &[],
                        identity.oid,
                    )
                {
                    return Err(conflict());
                }
            }
            for domain in snapshot.definitions.domains.values() {
                validate_rows(
                    &domain.identity,
                    target,
                    identity,
                    uqa_sql::schema::domains::constraints::identities(&domain.definition),
                    &mut found,
                )?;
            }
            if trigger_address_in_use(catalog, resolution, identity.oid)? {
                return Err(conflict());
            }
            Ok(found)
        }
    }
}

fn validate_rows(
    relation: &RelationIdentity,
    target: &RelationIdentity,
    identity: ConstraintCatalogIdentity,
    rows: impl Iterator<Item = ConstraintCatalogIdentity>,
    found: &mut bool,
) -> Result<bool, SQLError> {
    let mut local = false;
    for row in rows {
        if row.oid == identity.oid || row.object_id == identity.object_id {
            if relation != target || row != identity || *found {
                return Err(conflict());
            }
            *found = true;
            local = true;
        }
    }
    Ok(local)
}

fn conflict() -> SQLError {
    SQLError::Internal("supplied constraint catalog identity conflicts with an existing row".into())
}

fn trigger_address_in_use(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    oid: i64,
) -> Result<bool, SQLError> {
    for (trigger, _) in super::events::catalog_triggers(catalog, resolution)? {
        if super::events::trigger_constraint_catalog_oid(catalog, resolution, &trigger)? == oid {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests;
