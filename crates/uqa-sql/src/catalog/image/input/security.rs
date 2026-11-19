//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Schema and database authority preserve optional ACLs, grantor paths and tuple incarnations.

use super::{invalid, Fields};
use crate::{
    catalog::security::{
        database::{BoundDatabaseSecurity, DatabaseAuthority, DatabasePrivileges},
        schema_registry::SchemaRegistry,
        BoundSchemaSecurity,
    },
    SQLError,
};
use std::io::Read;
use uqa_core::{
    catalog_schema::{SchemaPrivileges, SchemaTupleIdentity},
    image_io::{decode_image, ImageDecoder},
    memory::{Budgeted, BudgetedMap, MemoryBudget},
    CancellationToken,
};

pub(in super::super) fn schemas(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<SchemaRegistry, SQLError> {
    super::continuation::read_registry(reader, memory, cancellation)
}

impl SchemaRegistry {
    /// Retain native entries and identity claims between calls; future mutations use the destination allowance.
    pub fn image_input(
        input: &MemoryBudget,
        mutations: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<super::CatalogImageInput<Self>, SQLError> {
        super::continuation::registry::<Self>(input, mutations, cancellation)
    }
}

pub(in super::super) fn database(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<DatabaseAuthority, SQLError> {
    decode_image(reader, memory, cancellation, async |binary| {
        Fields {
            binary,
            retained_children: &mut 0,
        }
        .database()
        .await
    })
}

impl DatabaseAuthority {
    /// Retain the single native authority tuple and its incomplete ACL across byte/poll windows.
    pub fn image_input(
        memory: &MemoryBudget,
        mutations: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<super::CatalogImageInput<Self>, SQLError> {
        let mutations = mutations.clone();
        let decoder = ImageDecoder::new(memory, cancellation, |mut reader| async move {
            let mut input = Fields {
                binary: reader.input(),
                retained_children: &mut 0,
            };
            Ok(input.database().await?.into_mutation_budget(&mutations))
        })?;
        super::CatalogImageInput::single(decoder, memory)
    }
}

impl Fields<'_> {
    async fn database(&mut self) -> Result<DatabaseAuthority, SQLError> {
        if self.header(super::super::DATABASE).await? != 1 {
            return Err(invalid("database authority count"));
        }
        let role_owner = self.identity().await?;
        let (acl, reservation) = self.acl::<DatabasePrivileges>().await?.into_parts();
        let value = Budgeted::new(BoundDatabaseSecurity { role_owner, acl }, reservation);
        self.binary.end().await?;
        DatabaseAuthority::from_image_value(value, self.binary.cancellation)
    }
}

impl super::continuation::CatalogEntries for SchemaRegistry {
    type Scratch = (BudgetedMap<i64, ()>, BudgetedMap<[u8; 16], ()>);
    const MAGIC: [u8; 8] = super::super::SCHEMAS;
    fn create(memory: &MemoryBudget) -> (Self, Self::Scratch) {
        (
            Self::with_budget(memory),
            (BudgetedMap::new(memory), BudgetedMap::new(memory)),
        )
    }
    async fn read_entry(
        input: &mut Fields<'_>,
        result: &mut Self,
        scratch: &mut Self::Scratch,
    ) -> Result<(), SQLError> {
        let (oids, identities) = scratch;
        let name = input.ordered_name(result.last_durable_key()).await?;
        if crate::catalog::temporary_namespace::is_temporary_schema_name(&name) {
            return Err(invalid("temporary schema"));
        }
        let tuple = if input.flag().await? {
            let tuple = SchemaTupleIdentity {
                oid: input.oid().await?,
                object_id: input.binary.array().await?,
                revision: input.binary.array().await?,
            };
            if !tuple.is_valid() {
                return Err(invalid("schema tuple"));
            }
            if oids.insert(tuple.oid, ())?.is_some()
                || identities.insert(tuple.object_id, ())?.is_some()
            {
                return Err(invalid("duplicate schema identity"));
            }
            Some(tuple)
        } else {
            None
        };
        let role_owner = input.identity().await?;
        let (acl, reservation) = input.acl::<SchemaPrivileges>().await?.into_parts();
        result.insert_image_entry(
            name,
            Budgeted::new(
                BoundSchemaSecurity {
                    tuple,
                    role_owner,
                    acl,
                },
                reservation,
            ),
        )?;
        Ok(())
    }
    fn finish(self, mutations: &MemoryBudget) -> Self {
        self.into_mutation_budget(mutations)
    }
}
