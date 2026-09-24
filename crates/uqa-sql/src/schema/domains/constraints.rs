//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable names and individual catalog identities for domain constraints.

use crate::ast::{ConstraintCatalogIdentity, CreateDomain};
use crate::schema::constraint_metadata::{
    assign_constraint_name, CatalogIdentityAllocator, CatalogOidClass, ConstraintMetadataError,
    ConstraintMetadataResult,
};
use crate::SQLError;
use std::collections::BTreeSet;
use uqa_core::RelationIdentity;

pub fn assign_names(
    definition: &mut CreateDomain,
    schema_names: &BTreeSet<String>,
) -> Result<(), SQLError> {
    let identity =
        RelationIdentity::from_legacy_name(&definition.name).map_err(SQLError::Internal)?;
    let mut names = BTreeSet::new();
    let mut automatic = schema_names.clone();
    if let Some(not_null) = &mut definition.not_null {
        assign_constraint_name(
            &mut not_null.name,
            (&identity.name, "", "not_null"),
            &mut automatic,
        )
        .map_err(|error| crate::catalog::errors::storage_error("domain constraint name", &error))?;
        let name = not_null.name.as_ref().expect("assigned NOT NULL name");
        names.insert(name.clone());
        automatic.insert(name.clone());
    }
    for check in &mut definition.checks {
        if let Some(name) = &check.name {
            if !names.insert(name.clone()) {
                return Err(crate::assignment::domain::domain_error(
                    "42710",
                    format!(
                        "constraint \"{name}\" for domain \"{}\" already exists",
                        identity.name
                    ),
                ));
            }
        } else {
            assign_constraint_name(
                &mut check.name,
                (&identity.name, "", "check"),
                &mut automatic,
            )
            .map_err(|error| {
                crate::catalog::errors::storage_error("domain constraint name", &error)
            })?;
            names.insert(check.name.clone().expect("assigned CHECK name"));
        }
        automatic.extend(check.name.iter().cloned());
    }
    Ok(())
}

pub fn identities(
    definition: &CreateDomain,
) -> impl Iterator<Item = ConstraintCatalogIdentity> + '_ {
    definition
        .not_null
        .iter()
        .filter_map(|constraint| constraint.catalog_identity)
        .chain(
            definition
                .checks
                .iter()
                .filter_map(|constraint| constraint.catalog_identity),
        )
}

/// Unmaterialized declarations and initial legacy conversion may omit metadata. Supplied corrupt metadata is never replaced.
pub fn validate(definition: &CreateDomain, allow_missing: bool) -> ConstraintMetadataResult<()> {
    let mut names = BTreeSet::new();
    let mut objects = BTreeSet::new();
    let mut oids = BTreeSet::new();
    let rows = definition
        .not_null
        .iter()
        .map(|constraint| (&constraint.name, constraint.catalog_identity))
        .chain(
            definition
                .checks
                .iter()
                .map(|constraint| (&constraint.name, constraint.catalog_identity)),
        );
    for (name, identity) in rows {
        if let Some(name) = name {
            if name.is_empty() || !names.insert(name) {
                return Err(invalid("invalid or duplicate domain constraint name"));
            }
        } else if !allow_missing {
            return Err(invalid("domain constraint has no durable name"));
        }
        if let Some(identity) = identity {
            if !identity.is_valid()
                || !objects.insert(identity.object_id)
                || !oids.insert(identity.oid)
            {
                return Err(invalid(
                    "invalid or duplicate domain constraint catalog identity",
                ));
            }
        } else if !allow_missing {
            return Err(invalid("domain constraint has no catalog identity"));
        }
    }
    Ok(())
}

pub fn materialize(
    definition: &mut CreateDomain,
    allocate: &mut CatalogIdentityAllocator<'_>,
) -> ConstraintMetadataResult<bool> {
    validate(definition, true)?;
    let relation = RelationIdentity::from_legacy_name(&definition.name).map_err(invalid)?;
    for identity in identities(definition) {
        allocate.include_catalog_identity(&relation, CatalogOidClass::Constraint, identity)?;
    }
    let mut changed = false;
    let rows = definition
        .not_null
        .iter_mut()
        .map(|constraint| &mut constraint.catalog_identity)
        .chain(
            definition
                .checks
                .iter_mut()
                .map(|constraint| &mut constraint.catalog_identity),
        );
    for target in rows {
        if target.is_some() {
            continue;
        }
        let object_id = allocate.allocate_object_id("domain constraint")?;
        *target = Some(ConstraintCatalogIdentity {
            object_id,
            oid: allocate.allocate_catalog_oid(CatalogOidClass::Constraint, &object_id)?,
        });
        changed = true;
    }
    validate(definition, false)?;
    Ok(changed)
}

fn invalid(message: impl Into<String>) -> ConstraintMetadataError {
    ConstraintMetadataError::Invalid(message.into())
}

#[cfg(test)]
mod tests;
