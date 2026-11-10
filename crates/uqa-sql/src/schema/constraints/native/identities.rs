//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog row identities share one namespace across ordinary tables, foreign tables and domains.

use super::{invalid, NativeConstraintTable};
use crate::{
    ast::{ConstraintCatalogIdentity, CreateDomain},
    schema::{constraint_metadata::identity::claims::row_identities, domains::constraints},
    SQLError,
};
use uqa_core::memory::{ProductionControl, ProductionMap};

pub(super) struct Identities<'a> {
    objects: ProductionMap<'a, [u8; 16], ()>,
    oids: ProductionMap<'a, i64, ()>,
    control: ProductionControl<'a>,
}

impl<'a> Identities<'a> {
    pub(super) fn new(control: ProductionControl<'a>) -> Self {
        Self {
            objects: ProductionMap::new(control),
            oids: ProductionMap::new(control),
            control,
        }
    }

    fn insert(&mut self, identity: ConstraintCatalogIdentity) -> Result<(), SQLError> {
        self.control.check()?;
        if !identity.is_valid() {
            return Err(invalid("invalid constraint catalog identity"));
        }
        if self.objects.insert_copy(identity.object_id, ())?.is_some() {
            return Err(invalid("duplicate constraint catalog incarnation"));
        }
        if self.oids.insert_copy(identity.oid, ())?.is_some() {
            return Err(invalid("duplicate constraint catalog OID"));
        }
        Ok(())
    }

    pub(super) fn table(&mut self, table: NativeConstraintTable<'_>) -> Result<(), SQLError> {
        self.control.check()?;
        for identity in row_identities(table.columns, table.checks, table.keys, table.foreign_keys)
        {
            self.insert(identity)?;
        }
        Ok(())
    }

    pub(super) fn domain(&mut self, domain: &CreateDomain) -> Result<(), SQLError> {
        self.control.check()?;
        for identity in constraints::identities(domain) {
            self.insert(identity)?;
        }
        Ok(())
    }
}
