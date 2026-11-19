//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Paired foreign definitions and authority preserve original identities without contacting a source.

use super::{CapturedForeignCatalog, ForeignCatalog, StoredForeignTable};
use std::{cmp::Ordering, io::Read, ops::Bound, sync::Arc};
use uqa_core::{
    image_io::{decode_image, AsyncImageInput, ImageDecoder},
    memory::{Budgeted, BudgetedMap, BudgetedVec, MemoryBudget, MemoryReservation, OwnedMap},
    CancellationToken, RelationIdentity,
};
use uqa_sql::{
    ast::RelationPersistence,
    catalog::{
        image,
        relation_oids::{RelationCatalogOids, RelationOidKind},
        security::BoundTableSecurity,
    },
    SQLError,
};
use uqa_storage::{image_io::read_framed_image_from, read_control::StorageReadControl};

mod output;

const MAGIC: [u8; 8] = *b"UQAFCT01";

fn invalid(message: &'static str) -> SQLError {
    SQLError::Routine {
        sqlstate: "XX001".into(),
        message: format!("invalid foreign catalog image: {message}"),
    }
}

fn take<T>(value: Budgeted<T>, memory: &mut MemoryReservation) -> T {
    let (value, lease) = value.into_parts();
    memory.absorb(lease);
    value
}

struct Decoder<'a>(AsyncImageInput<'a>);
impl Decoder<'_> {
    async fn compare(&mut self, left: &str, right: &str) -> Result<Ordering, SQLError> {
        for (left, right) in left
            .as_bytes()
            .chunks(4096)
            .zip(right.as_bytes().chunks(4096))
        {
            self.0.checkpoint().await?;
            let order = left.cmp(right);
            if order != Ordering::Equal {
                return Ok(order);
            }
        }
        self.0.checkpoint().await?;
        Ok(left.len().cmp(&right.len()))
    }

    async fn flag(&mut self) -> Result<bool, SQLError> {
        match self.0.array::<1>().await?[0] {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(invalid("presence tag")),
        }
    }
    async fn text(
        &mut self,
        memory: &mut MemoryReservation,
        name: bool,
    ) -> Result<String, SQLError> {
        let text = self.0.text().await?;
        if name {
            if text.is_empty() {
                return Err(invalid("empty name"));
            }
            for chunk in text.as_bytes().chunks(4096) {
                self.0.checkpoint().await?;
                if chunk.contains(&0) {
                    return Err(invalid("name contains NUL"));
                }
            }
        }
        Ok(take(text, memory))
    }
    async fn optional_text(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<Option<String>, SQLError> {
        if self.flag().await? {
            Ok(Some(self.text(memory, true).await?))
        } else {
            Ok(None)
        }
    }
    async fn oid(&mut self) -> Result<u32, SQLError> {
        let value = u32::from_le_bytes(self.0.array().await?);
        if value == 0 {
            return Err(invalid("zero OID"));
        }
        Ok(value)
    }
    async fn optional_oid(&mut self) -> Result<Option<u32>, SQLError> {
        if self.flag().await? {
            Ok(Some(self.oid().await?))
        } else {
            Ok(None)
        }
    }
    async fn options(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<OwnedMap<String, String>, SQLError> {
        let count = self.0.count().await?;
        let mut values = OwnedMap::new();
        let mut pending: Option<(String, String)> = None;
        for _ in 0..count {
            let name = self.text(memory, true).await?;
            if let Some((previous, _)) = &pending {
                if self.compare(previous, &name).await? != Ordering::Less {
                    return Err(invalid("option order"));
                }
            }
            let value = self.text(memory, false).await?;
            memory.grow(OwnedMap::<String, String>::entry_bytes())?;
            if let Some((name, value)) = pending.replace((name, value)) {
                values.insert(name, value);
            }
        }
        if let Some((name, value)) = pending {
            values.insert(name, value);
        }
        Ok(values)
    }
    async fn names(&mut self, memory: &mut MemoryReservation) -> Result<Vec<String>, SQLError> {
        let count = self.0.count().await?;
        let mut values = BudgetedVec::new(self.0.memory);
        values.reserve(count)?;
        for _ in 0..count {
            values.push(self.text(memory, true).await?)?;
        }
        let (values, lease) = values.into_parts();
        memory.absorb(lease);
        Ok(values)
    }
    async fn table(&mut self) -> Result<(Budgeted<StoredForeignTable>, usize), SQLError> {
        let mut memory = self.0.memory.empty_reservation();
        let name = self.text(&mut memory, true).await?;
        let persistence = match self.0.array::<1>().await?[0] {
            0 => RelationPersistence::Permanent,
            1 => RelationPersistence::Unlogged,
            _ => return Err(invalid("durable persistence")),
        };
        let object_id = self.0.array::<16>().await?;
        if object_id == [0; 16] {
            return Err(invalid("zero object identity"));
        }
        let catalog_oids = if self.flag().await? {
            let oids = RelationCatalogOids {
                relation: self.oid().await?,
                row_type: self.optional_oid().await?,
                array_type: self.optional_oid().await?,
                rule: self.optional_oid().await?,
            };
            if !oids.is_valid_for(RelationOidKind::ForeignTable, &object_id) {
                return Err(invalid("relation OIDs"));
            }
            Some(oids)
        } else {
            None
        };
        let row_type_array_name = self.optional_text(&mut memory).await?;
        let server_name = self.text(&mut memory, true).await?;
        let server_reference = if self.flag().await? {
            let reference = super::super::ForeignServerReference {
                oid: self.oid().await?,
                object_id: self.0.array().await?,
            };
            if reference.oid < 16384 || reference.object_id == [0; 16] {
                return Err(invalid("server identity"));
            }
            Some(reference)
        } else {
            None
        };
        let options = self.options(&mut memory).await?;
        let option_order = self.names(&mut memory).await?;
        if option_order.len() != options.len() {
            return Err(invalid("option membership"));
        }
        let mut seen = BudgetedMap::new(self.0.memory);
        for name in &option_order {
            self.0.checkpoint().await?;
            if !options.contains_key(name) || seen.insert(name.as_str(), ())?.is_some() {
                return Err(invalid("option membership"));
            }
        }
        drop(seen);
        let (columns, column_children) = read_framed_image_from(&mut self.0, async |input| {
            image::read_columns_image_from(input).await
        })
        .await?;
        let columns = take(columns, &mut memory);
        let dropped_attributes = take(
            read_framed_image_from(&mut self.0, async |input| {
                image::read_dropped_attributes_image_from(input).await
            })
            .await?,
            &mut memory,
        );
        let (checks, check_children) = read_framed_image_from(&mut self.0, async |input| {
            image::read_table_checks_image_from(input).await
        })
        .await?;
        let checks = take(checks, &mut memory);
        let children = column_children
            .checked_add(check_children)
            .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?;
        Ok((
            Budgeted::new(
                StoredForeignTable {
                    name,
                    persistence,
                    object_id,
                    catalog_oids,
                    row_type_array_name,
                    server_name,
                    server_reference,
                    columns,
                    dropped_attributes,
                    checks,
                    options,
                    option_order,
                },
                memory,
            ),
            children,
        ))
    }
}

impl ForeignCatalog {
    /// Reconstruct both private native roots from one ordered stream. Original server bindings and declarations remain unevaluated; complete restore owns cross-catalog validation and activation.
    pub fn read_image(
        reader: &mut dyn Read,
        control: &StorageReadControl,
    ) -> Result<Self, SQLError> {
        Self::read_image_with_mutation_budget(reader, control, control.memory())
    }

    /// Transfer complete paired definitions and authority to the destination's mutation allowance. Decoded keys, declarations and ACLs retain the original input allowance; no catalog binding or callback is rerun.
    pub fn read_image_with_mutation_budget(
        reader: &mut dyn Read,
        control: &StorageReadControl,
        mutations: &MemoryBudget,
    ) -> Result<Self, SQLError> {
        decode_image(
            reader,
            control.memory(),
            control.cancellation(),
            async |input| read_catalog(input, mutations).await,
        )
    }

    /// Retain native catalog and framed SQL field continuations within each byte/poll window. Both published roots preserve their original input ownership and use the independent destination allowance for later mutations.
    pub fn image_input(
        memory: &MemoryBudget,
        mutations: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<ImageDecoder<Self, SQLError>, SQLError> {
        let mutations = mutations.clone();
        Ok(ImageDecoder::new(
            memory,
            cancellation,
            |mut reader| async move { read_catalog(reader.input(), &mutations).await },
        )?)
    }
}

async fn read_catalog(
    binary: AsyncImageInput<'_>,
    mutations: &MemoryBudget,
) -> Result<ForeignCatalog, SQLError> {
    let memory = binary.memory;
    let mut input = Decoder(binary);
    if input.0.array::<8>().await? != MAGIC {
        return Err(invalid("version header"));
    }
    let count = input.0.count().await?;
    let mut tables = super::ForeignTables::with_budget(memory);
    let mut securities =
        uqa_sql::catalog::security::foreign_tables::ForeignTableSecurities::with_budget(memory);
    for _ in 0..count {
        let mut key_memory = memory.empty_reservation();
        let key = RelationIdentity {
            schema: input.text(&mut key_memory, true).await?,
            name: input.text(&mut key_memory, true).await?,
        };
        if let Some((previous, _)) = tables.durable.last_before(Bound::Unbounded) {
            let mut order = input.compare(&previous.schema, &key.schema).await?;
            if order == Ordering::Equal {
                order = input.compare(&previous.name, &key.name).await?;
            }
            if order != Ordering::Less {
                return Err(invalid("relation order"));
            }
        }
        let (table, children) = input.table().await?;
        let security = read_framed_image_from(&mut input.0, async |input| {
            BoundTableSecurity::read_image_from(input).await
        })
        .await?;
        let (schema, mut security_memory) = input.0.copy_text(&key.schema).await?.into_parts();
        let (name, name_memory) = input.0.copy_text(&key.name).await?.into_parts();
        security_memory.absorb(name_memory);
        securities.insert_image_entry(
            Budgeted::new(RelationIdentity { schema, name }, security_memory),
            table.persistence,
            security,
        )?;
        let (table, mut memory) = table.into_parts();
        memory.absorb(key_memory);
        let bytes = memory
            .bytes()
            .checked_add(children)
            .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?;
        tables
            .durable
            .insert(key, Budgeted::new(table, memory), bytes)?;
    }
    input.0.end().await?;
    tables.durable = tables.durable.into_mutation_budget(mutations);
    tables.temporary = tables.temporary.into_mutation_budget(mutations);
    Ok(ForeignCatalog {
        tables: Arc::new(tables),
        security: Arc::new(securities.into_mutation_budget(mutations)),
    })
}

#[cfg(test)]
mod tests;
