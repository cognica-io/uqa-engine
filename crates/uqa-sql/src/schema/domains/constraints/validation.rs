//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One constraint per step, sharing declaration and restore validation.

use super::{
    invalid, ConstraintCatalogIdentity, ConstraintMetadataError, ConstraintMetadataResult,
    CreateDomain,
};
use uqa_core::memory::{ProductionControl, ProductionMap};

pub(crate) fn constraint(
    definition: &CreateDomain,
    index: usize,
) -> Option<(Option<&str>, Option<ConstraintCatalogIdentity>)> {
    if let Some(not_null) = &definition.not_null {
        if index == 0 {
            return Some((not_null.name.as_deref(), not_null.catalog_identity));
        }
    }
    definition
        .checks
        .get(index - usize::from(definition.not_null.is_some()))
        .map(|check| (check.name.as_deref(), check.catalog_identity))
}

pub(crate) struct ConstraintValidation<'a> {
    definition: &'a CreateDomain,
    index: usize,
    allow_missing: bool,
    names: ProductionMap<'a, &'a str, ()>,
    objects: ProductionMap<'a, [u8; 16], ()>,
    oids: ProductionMap<'a, i64, ()>,
    control: ProductionControl<'a>,
}

impl<'a> ConstraintValidation<'a> {
    pub(crate) fn new(
        definition: &'a CreateDomain,
        allow_missing: bool,
        control: ProductionControl<'a>,
    ) -> Self {
        Self {
            definition,
            index: 0,
            allow_missing,
            names: ProductionMap::new(control),
            objects: ProductionMap::new(control),
            oids: ProductionMap::new(control),
            control,
        }
    }

    pub(crate) fn advance(&mut self) -> ConstraintMetadataResult<bool> {
        self.control.check().map_err(execution)?;
        let Some((name, identity)) = constraint(self.definition, self.index) else {
            return Ok(true);
        };
        if let Some(name) = name {
            if name.is_empty()
                || self
                    .names
                    .insert_copy(name, ())
                    .map_err(execution)?
                    .is_some()
            {
                return Err(invalid("invalid or duplicate domain constraint name"));
            }
        } else if !self.allow_missing {
            return Err(invalid("domain constraint has no durable name"));
        }
        if let Some(identity) = identity {
            if !identity.is_valid()
                || self
                    .objects
                    .insert_copy(identity.object_id, ())
                    .map_err(execution)?
                    .is_some()
                || self
                    .oids
                    .insert_copy(identity.oid, ())
                    .map_err(execution)?
                    .is_some()
            {
                return Err(invalid(
                    "invalid or duplicate domain constraint catalog identity",
                ));
            }
        } else if !self.allow_missing {
            return Err(invalid("domain constraint has no catalog identity"));
        }
        self.index += 1;
        Ok(false)
    }
}

pub(crate) fn execution(error: uqa_core::ValueRetentionError) -> ConstraintMetadataError {
    ConstraintMetadataError::Execution(Box::new(error.into()))
}
