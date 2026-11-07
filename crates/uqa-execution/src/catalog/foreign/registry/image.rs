//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Paired foreign definitions and authority preserve original identities without contacting a source.

use super::{CapturedForeignCatalog, ForeignCatalog, StoredForeignTable};
use std::{cmp::Ordering, io::Read, ops::Bound, sync::Arc};
use uqa_core::{
    image_io::ImageInput,
    memory::{Budgeted, BudgetedMap, BudgetedVec, MemoryReservation, OwnedMap, ProductionControl},
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
use uqa_storage::{image_io::FramedImageInput, read_control::StorageReadControl};

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

fn compare(left: &str, right: &str, token: &CancellationToken) -> Result<Ordering, SQLError> {
    for (left, right) in left
        .as_bytes()
        .chunks(4096)
        .zip(right.as_bytes().chunks(4096))
    {
        token.check()?;
        let order = left.cmp(right);
        if order != Ordering::Equal {
            return Ok(order);
        }
    }
    token.check()?;
    Ok(left.len().cmp(&right.len()))
}

struct Decoder<'a>(ImageInput<'a>);
impl Decoder<'_> {
    fn flag(&mut self) -> Result<bool, SQLError> {
        match self.0.array::<1>()?[0] {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(invalid("presence tag")),
        }
    }
    fn text(&mut self, memory: &mut MemoryReservation, name: bool) -> Result<String, SQLError> {
        let text = self.0.text()?;
        if name {
            if text.is_empty() {
                return Err(invalid("empty name"));
            }
            for chunk in text.as_bytes().chunks(4096) {
                self.0.cancellation.check()?;
                if chunk.contains(&0) {
                    return Err(invalid("name contains NUL"));
                }
            }
        }
        Ok(take(text, memory))
    }
    fn optional_text(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<Option<String>, SQLError> {
        if self.flag()? {
            Ok(Some(self.text(memory, true)?))
        } else {
            Ok(None)
        }
    }
    fn oid(&mut self) -> Result<u32, SQLError> {
        let value = u32::from_le_bytes(self.0.array()?);
        if value == 0 {
            return Err(invalid("zero OID"));
        }
        Ok(value)
    }
    fn optional_oid(&mut self) -> Result<Option<u32>, SQLError> {
        if self.flag()? {
            Ok(Some(self.oid()?))
        } else {
            Ok(None)
        }
    }
    fn child<T>(
        &mut self,
        decode: impl FnOnce(
            &mut dyn Read,
            &uqa_core::memory::MemoryBudget,
            &CancellationToken,
        ) -> Result<T, SQLError>,
    ) -> Result<T, SQLError> {
        decode(
            &mut FramedImageInput::new(self.0.reader),
            self.0.memory,
            self.0.cancellation,
        )
    }
    fn options(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<OwnedMap<String, String>, SQLError> {
        let count = self.0.count()?;
        let mut values = OwnedMap::new();
        let mut pending: Option<(String, String)> = None;
        for _ in 0..count {
            let name = self.text(memory, true)?;
            if let Some((previous, _)) = &pending {
                if compare(previous, &name, self.0.cancellation)? != Ordering::Less {
                    return Err(invalid("option order"));
                }
            }
            let value = self.text(memory, false)?;
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
    fn names(&mut self, memory: &mut MemoryReservation) -> Result<Vec<String>, SQLError> {
        let count = self.0.count()?;
        let mut values = BudgetedVec::new(self.0.memory);
        values.reserve(count)?;
        for _ in 0..count {
            values.push(self.text(memory, true)?)?;
        }
        let (values, lease) = values.into_parts();
        memory.absorb(lease);
        Ok(values)
    }
    fn table(&mut self) -> Result<(Budgeted<StoredForeignTable>, usize), SQLError> {
        let mut memory = self.0.memory.empty_reservation();
        let name = self.text(&mut memory, true)?;
        let persistence = match self.0.array::<1>()?[0] {
            0 => RelationPersistence::Permanent,
            1 => RelationPersistence::Unlogged,
            _ => return Err(invalid("durable persistence")),
        };
        let object_id = self.0.array::<16>()?;
        if object_id == [0; 16] {
            return Err(invalid("zero object identity"));
        }
        let catalog_oids = if self.flag()? {
            let oids = RelationCatalogOids {
                relation: self.oid()?,
                row_type: self.optional_oid()?,
                array_type: self.optional_oid()?,
                rule: self.optional_oid()?,
            };
            if !oids.is_valid_for(RelationOidKind::ForeignTable, &object_id) {
                return Err(invalid("relation OIDs"));
            }
            Some(oids)
        } else {
            None
        };
        let row_type_array_name = self.optional_text(&mut memory)?;
        let server_name = self.text(&mut memory, true)?;
        let server_reference = if self.flag()? {
            let reference = super::super::ForeignServerReference {
                oid: self.oid()?,
                object_id: self.0.array()?,
            };
            if reference.oid < 16384 || reference.object_id == [0; 16] {
                return Err(invalid("server identity"));
            }
            Some(reference)
        } else {
            None
        };
        let options = self.options(&mut memory)?;
        let option_order = self.names(&mut memory)?;
        if option_order.len() != options.len() {
            return Err(invalid("option membership"));
        }
        let mut seen = BudgetedMap::new(self.0.memory);
        for name in &option_order {
            self.0.cancellation.check()?;
            if !options.contains_key(name) || seen.insert(name.as_str(), ())?.is_some() {
                return Err(invalid("option membership"));
            }
        }
        drop(seen);
        let (columns, column_children) = self.child(image::read_columns_image_with_footprint)?;
        let columns = take(columns, &mut memory);
        let dropped_attributes = take(
            self.child(image::read_dropped_attributes_image)?,
            &mut memory,
        );
        let (checks, check_children) = self.child(image::read_table_checks_image_with_footprint)?;
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
        let mut input = Decoder(ImageInput {
            reader,
            memory: control.memory(),
            cancellation: control.cancellation(),
        });
        if input.0.array::<8>()? != MAGIC {
            return Err(invalid("version header"));
        }
        let count = input.0.count()?;
        let mut result = Self::with_budget(control.memory());
        let production = ProductionControl::new(
            control.memory(),
            control.cancellation(),
            control.cancellation(),
        );
        for _ in 0..count {
            let mut key_memory = control.memory().empty_reservation();
            let key = RelationIdentity {
                schema: input.text(&mut key_memory, true)?,
                name: input.text(&mut key_memory, true)?,
            };
            if let Some((previous, _)) = result.tables.durable.last_before(Bound::Unbounded) {
                let mut order = compare(&previous.schema, &key.schema, control.cancellation())?;
                if order == Ordering::Equal {
                    order = compare(&previous.name, &key.name, control.cancellation())?;
                }
                if order != Ordering::Less {
                    return Err(invalid("relation order"));
                }
            }
            let (table, children) = input.table()?;
            let security = input.child(BoundTableSecurity::read_image)?;
            let (schema, mut security_memory) = production.copy_text(&key.schema)?.into_parts();
            let (name, name_memory) = production.copy_text(&key.name)?.into_parts();
            security_memory = production.combine(security_memory, name_memory);
            Arc::make_mut(&mut result.security).insert_image_entry(
                Budgeted::new(
                    RelationIdentity { schema, name },
                    security_memory.expect("admitted foreign key"),
                ),
                table.persistence,
                security,
            )?;
            let (table, mut memory) = table.into_parts();
            memory.absorb(key_memory);
            let bytes = memory
                .bytes()
                .checked_add(children)
                .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?;
            Arc::make_mut(&mut result.tables).durable.insert(
                key,
                Budgeted::new(table, memory),
                bytes,
            )?;
        }
        input.0.end()?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests;
