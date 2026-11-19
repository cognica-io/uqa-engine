//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table, domain and constraint-trigger rows share the constraint catalog namespace.

use super::{invalid, NativeConstraintTable};
use crate::{
    ast::CreateDomain,
    catalog::events::StoredTrigger,
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

    fn insert(&mut self, object_id: Option<[u8; 16]>, oid: Option<i64>) -> Result<(), SQLError> {
        self.control.check()?;
        if object_id.is_some_and(|id| id == [0; 16])
            || oid.is_some_and(|oid| !u32::try_from(oid).is_ok_and(|oid| oid != 0))
        {
            return Err(invalid("invalid constraint catalog identity"));
        }
        if let Some(object_id) = object_id {
            if self.objects.insert_copy(object_id, ())?.is_some() {
                return Err(invalid("duplicate constraint catalog incarnation"));
            }
        }
        if let Some(oid) = oid {
            if self.oids.insert_copy(oid, ())?.is_some() {
                return Err(invalid("duplicate constraint catalog OID"));
            }
        }
        Ok(())
    }

    pub(super) fn table(&mut self, table: NativeConstraintTable<'_>) -> Result<(), SQLError> {
        self.control.check()?;
        for identity in row_identities(table.columns, table.checks, table.keys, table.foreign_keys)
        {
            self.insert(Some(identity.object_id), Some(identity.oid))?;
        }
        Ok(())
    }

    pub(super) fn domain(&mut self, domain: &CreateDomain) -> Result<(), SQLError> {
        self.control.check()?;
        for identity in constraints::identities(domain) {
            self.insert(Some(identity.object_id), Some(identity.oid))?;
        }
        Ok(())
    }

    pub(super) fn trigger(&mut self, trigger: &StoredTrigger) -> Result<(), SQLError> {
        self.control.check()?;
        if trigger.definition.constraint {
            self.insert(trigger.object_id, trigger.constraint_catalog_oid)?;
        }
        Ok(())
    }
}
