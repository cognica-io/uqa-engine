//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A reconstructed view retains its native query owner through immutable catalog publication.

use super::{expressions::payload, invalid, metadata::Identities, plans::Owner, Input};
use crate::{
    ast::RelationPersistence,
    catalog::{
        named_definitions::DefinitionImage,
        relation_oids::{RelationCatalogOids, RelationOidKind},
        security::BoundTableSecurity,
        stored_view::{StoredView, StoredViewDefinition, ViewDefinitions},
        view::StoredViewKind,
    },
    SQLError,
};
use std::{cmp::Ordering, io::Read};
use uqa_core::{
    catalog_acl::RelationAclRevisions,
    memory::{Budgeted, MemoryBudget, MemoryError, MemoryReservation, OwnedMap},
    CancellationToken,
};

struct ViewImage {
    value: StoredView,
    query: Owner,
    memory: MemoryReservation,
}

impl Drop for ViewImage {
    fn drop(&mut self) {
        self.query.exchange_query(&mut self.value.definition.query);
    }
}

impl DefinitionImage<StoredView> for ViewImage {
    fn definition(&self) -> &StoredView {
        &self.value
    }
    fn budget(&self) -> &MemoryBudget {
        self.memory.budget()
    }
    fn retained_bytes(&self) -> Result<usize, MemoryError> {
        self.memory
            .bytes()
            .checked_add(self.query.retained_bytes()?)
            .ok_or(MemoryError::SizeOverflow)
    }
}

impl Input<'_> {
    fn view_vector<T>(
        &mut self,
        memory: &mut MemoryReservation,
        read: impl FnMut(&mut Self, &mut MemoryReservation) -> Result<T, SQLError>,
    ) -> Result<Vec<T>, SQLError> {
        let count = self.binary.count()?;
        self.items(count, memory, read)
    }

    fn view_map<T>(
        &mut self,
        memory: &mut MemoryReservation,
        mut read: impl FnMut(&mut Self, &mut MemoryReservation) -> Result<T, SQLError>,
    ) -> Result<OwnedMap<String, T>, SQLError> {
        let count = self.binary.count()?;
        let mut result = OwnedMap::new();
        let mut previous: Option<(Budgeted<String>, T)> = None;
        for _ in 0..count {
            let key = self.ordered_name(previous.as_ref().map(|(key, _)| &**key))?;
            let value = read(self, memory)?;
            if let Some((key, value)) = previous.take() {
                memory.grow(OwnedMap::<String, T>::entry_bytes())?;
                result.insert(payload(key, memory), value);
            }
            previous = Some((key, value));
        }
        if let Some((key, value)) = previous {
            memory.grow(OwnedMap::<String, T>::entry_bytes())?;
            result.insert(payload(key, memory), value);
        }
        Ok(result)
    }

    fn view_security(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<BoundTableSecurity, SQLError> {
        let role_owner = self.identity()?;
        let acl = payload(self.acl()?, memory);
        let column_acls = self.view_map(memory, |input, memory| {
            Ok(payload(input.acl_entries()?, memory))
        })?;
        let relation = self.optional_incarnation()?;
        let columns = self.view_map(memory, |input, _| input.object_id())?;
        Ok(BoundTableSecurity {
            role_owner,
            acl,
            column_acls,
            acl_revisions: RelationAclRevisions { relation, columns },
        })
    }

    fn view_oids(
        &mut self,
        object_id: &[u8; 16],
        identities: &mut Identities,
    ) -> Result<Option<RelationCatalogOids>, SQLError> {
        if !self.flag()? {
            return Ok(None);
        }
        let oids = RelationCatalogOids {
            relation: self.catalog_oid()?,
            row_type: self.optional_oid()?,
            array_type: self.optional_oid()?,
            rule: self.optional_oid()?,
        };
        if !oids.is_valid_for(RelationOidKind::View, object_id) {
            return Err(invalid("view catalog OIDs"));
        }
        for oid in oids.claimed() {
            identities.oid(oid)?;
        }
        Ok(Some(oids))
    }

    fn view_image(&mut self, identities: &mut Identities) -> Result<ViewImage, SQLError> {
        let mut memory = self.binary.memory.empty_reservation();
        let security = self.view_security(&mut memory)?;
        let object_id = self.object_id()?;
        identities.object(object_id)?;
        let output_columns = if self.flag()? {
            Some(self.view_vector(&mut memory, |input, memory| {
                Ok(payload(input.binary.text()?, memory))
            })?)
        } else {
            None
        };
        let persistence = match self.binary.array::<1>()?[0] {
            0 => RelationPersistence::Permanent,
            1 => RelationPersistence::Unlogged,
            _ => return Err(invalid("durable view persistence")),
        };
        let options = self.view_vector(&mut memory, |input, memory| {
            let key = payload(input.binary.text()?, memory);
            let value = payload(input.binary.text()?, memory);
            Ok((key, value))
        })?;
        let kind = match self.binary.array::<1>()?[0] {
            0 => StoredViewKind::View,
            1 => StoredViewKind::Materialized,
            _ => return Err(invalid("view kind")),
        };
        let materialized_rows = self.view_vector(&mut memory, |input, memory| {
            input.view_map(memory, |input, memory| {
                Ok(payload(input.scalar_value()?, memory))
            })
        })?;
        let materialized_column_types = self.view_vector(&mut memory, |input, memory| {
            if input.flag()? {
                Ok(Some(payload(input.column_type()?, memory)))
            } else {
                Ok(None)
            }
        })?;
        let materialized_column_collations = if self.flag()? {
            Some(self.view_vector(&mut memory, |input, _| {
                Ok(u32::from_le_bytes(input.binary.array()?))
            })?)
        } else {
            None
        };
        let populated = self.flag()?;
        let catalog_oids = self.view_oids(&object_id, identities)?;
        let row_type_array_name = payload(self.optional_text()?, &mut memory);
        let (query, owner) = self.stored_query()?;
        Ok(ViewImage {
            value: StoredView {
                security,
                definition: StoredViewDefinition {
                    object_id,
                    query,
                    output_columns,
                    persistence,
                    options,
                    kind,
                    materialized_rows,
                    materialized_column_types,
                    materialized_column_collations,
                    populated,
                    catalog_oids,
                    row_type_array_name,
                },
            },
            query: owner,
            memory,
        })
    }
}

pub(in super::super) fn views(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<ViewDefinitions, SQLError> {
    let mut input = Input::new(reader, memory, cancellation);
    let count = input.header(*b"UQAVIW01")?;
    let mut result = ViewDefinitions::with_budget(memory);
    let mut identities = Identities::new(memory);
    for _ in 0..count {
        cancellation.check()?;
        let key = input.durable_type_identity()?;
        if let Some(previous) = result.last_durable_key() {
            let order = input.compare_text(&previous.schema, &key.schema)?;
            if order == Ordering::Greater
                || (order == Ordering::Equal
                    && input.compare_text(&previous.name, &key.name)? != Ordering::Less)
            {
                return Err(invalid("view catalog key order"));
            }
        }
        let value = input.view_image(&mut identities)?;
        result.insert_owned_image(key, value)?;
    }
    input.binary.end()?;
    Ok(result)
}

#[cfg(test)]
mod tests;
