//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Admission and identity checks shared by native metadata decoders.

use super::{invalid, Fields, Input};
use crate::{ast::ObjectAclEntry, catalog::array_type_names::valid_type_name, SQLError};
use uqa_core::{
    memory::{Budgeted, BudgetedMap, MemoryBudget, ProductionControl},
    RelationIdentity,
};

pub(super) struct Identities {
    oids: BudgetedMap<u32, ()>,
    objects: BudgetedMap<[u8; 16], ()>,
}
impl Identities {
    pub(super) fn new(memory: &MemoryBudget) -> Self {
        Self {
            oids: BudgetedMap::new(memory),
            objects: BudgetedMap::new(memory),
        }
    }
    pub(super) fn oid(&mut self, oid: u32) -> Result<(), SQLError> {
        if self.oids.insert(oid, ())?.is_some() {
            return Err(invalid("duplicate catalog OID"));
        }
        Ok(())
    }
    pub(super) fn object(&mut self, object: [u8; 16]) -> Result<(), SQLError> {
        if self.objects.insert(object, ())?.is_some() {
            return Err(invalid("duplicate catalog incarnation"));
        }
        Ok(())
    }
}

impl Fields<'_> {
    pub(super) async fn optional_incarnation(&mut self) -> Result<Option<[u8; 16]>, SQLError> {
        if self.flag().await? {
            Ok(Some(self.object_id().await?))
        } else {
            Ok(None)
        }
    }
    pub(super) async fn optional_catalog_identity(
        &mut self,
    ) -> Result<Option<crate::ast::ConstraintCatalogIdentity>, SQLError> {
        if self.flag().await? {
            Ok(Some(crate::ast::ConstraintCatalogIdentity {
                object_id: self.object_id().await?,
                oid: self.oid().await?,
            }))
        } else {
            Ok(None)
        }
    }
    pub(super) async fn object_id(&mut self) -> Result<[u8; 16], SQLError> {
        let id = self.binary.array().await?;
        if id == [0; 16] {
            return Err(invalid("catalog incarnation"));
        }
        Ok(id)
    }
    pub(super) async fn catalog_oid(&mut self) -> Result<u32, SQLError> {
        let oid = u32::from_le_bytes(self.binary.array().await?);
        if oid == 0 {
            return Err(invalid("catalog OID"));
        }
        Ok(oid)
    }
    pub(super) async fn user_oid(&mut self) -> Result<u32, SQLError> {
        let oid = self.catalog_oid().await?;
        if oid < crate::catalog::oids::FIRST_NORMAL_OBJECT_ID {
            return Err(invalid("user catalog OID"));
        }
        Ok(oid)
    }
    pub(super) async fn optional_oid(&mut self) -> Result<Option<u32>, SQLError> {
        if self.flag().await? {
            Ok(Some(self.catalog_oid().await?))
        } else {
            Ok(None)
        }
    }
    pub(super) async fn optional_text(&mut self) -> Result<Budgeted<Option<String>>, SQLError> {
        if self.flag().await? {
            let (value, reservation) = self.binary.text().await?.into_parts();
            Ok(Budgeted::new(Some(value), reservation))
        } else {
            Ok(Budgeted::new(None, self.binary.memory.empty_reservation()))
        }
    }
    pub(super) async fn optional_object_acl(
        &mut self,
    ) -> Result<Budgeted<Option<Vec<ObjectAclEntry>>>, SQLError> {
        if self.flag().await? {
            let (value, reservation) = self.object_acl().await?.into_parts();
            Ok(Budgeted::new(Some(value), reservation))
        } else {
            Ok(Budgeted::new(None, self.binary.memory.empty_reservation()))
        }
    }
    pub(super) async fn type_identity(
        &mut self,
        key: &str,
    ) -> Result<Budgeted<RelationIdentity>, SQLError> {
        let identity = self.durable_type_identity().await?;
        let control = ProductionControl::new(
            self.binary.memory,
            self.binary.cancellation,
            self.binary.cancellation,
        );
        let canonical = identity.qualified_name_with_control(&control)?;
        if self.compare_text(&canonical, key).await? != std::cmp::Ordering::Equal {
            return Err(invalid("type catalog key"));
        }
        Ok(identity)
    }
    pub(super) async fn durable_type_identity(
        &mut self,
    ) -> Result<Budgeted<RelationIdentity>, SQLError> {
        let schema = self.binary.text().await?;
        let name = self.binary.text().await?;
        if schema.is_empty()
            || !valid_type_name(&name)
            || crate::catalog::temporary_namespace::is_temporary_schema_name(&schema)
        {
            return Err(invalid("durable type identity"));
        }
        let (schema, mut reservation) = schema.into_parts();
        let (name, name_memory) = name.into_parts();
        reservation.absorb(name_memory);
        Ok(Budgeted::new(
            RelationIdentity { schema, name },
            reservation,
        ))
    }
}

impl Input<'_> {
    pub(super) fn optional_incarnation(&mut self) -> Result<Option<[u8; 16]>, SQLError> {
        self.fields(async |mut input| input.optional_incarnation().await)
    }
    pub(super) fn object_id(&mut self) -> Result<[u8; 16], SQLError> {
        self.fields(async |mut input| input.object_id().await)
    }
    pub(super) fn catalog_oid(&mut self) -> Result<u32, SQLError> {
        self.fields(async |mut input| input.catalog_oid().await)
    }
    pub(super) fn optional_oid(&mut self) -> Result<Option<u32>, SQLError> {
        self.fields(async |mut input| input.optional_oid().await)
    }
    pub(super) fn optional_text(&mut self) -> Result<Budgeted<Option<String>>, SQLError> {
        self.fields(async |mut input| input.optional_text().await)
    }
    pub(super) fn durable_type_identity(&mut self) -> Result<Budgeted<RelationIdentity>, SQLError> {
        self.fields(async |mut input| input.durable_type_identity().await)
    }
}
