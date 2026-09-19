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

mod relations;
pub(crate) use relations::relation_claims;

pub fn catalog_oid_in_use(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    class: CatalogOidClass,
    oid: i64,
) -> Result<bool, SQLError> {
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
            trigger_address_in_use(catalog, resolution, oid)
        }
    }
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
