//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A reconstructed view retains its native query owner through immutable catalog publication.

use super::{expressions::payload, invalid, metadata::Identities, plans::Owner, Fields, Input};
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

macro_rules! view_map {
    ($read:ident, $owner:expr, $allowance:expr, |$input:pat_param, $memory:pat_param| $body:block) => {{
        let count = $read!($owner.binary.count())?;
        let mut result = OwnedMap::new();
        let mut previous = None;
        for _ in 0..count {
            $read!($owner.checkpoint())?;
            let key = $read!($owner.ordered_name(
                previous
                    .as_ref()
                    .map(|(key, _): &(Budgeted<String>, _)| &**key)
            ))?;
            let $input = &mut *$owner;
            let $memory = &mut *$allowance;
            let value: Result<_, SQLError> = $body;
            let value = value?;
            if let Some((key, old)) = previous.take() {
                insert_map_entry(&mut result, key, old, $allowance)?;
            }
            previous = Some((key, value));
        }
        if let Some((key, value)) = previous {
            insert_map_entry(&mut result, key, value, $allowance)?;
        }
        Ok::<_, SQLError>(result)
    }};
}

fn insert_map_entry<T>(
    result: &mut OwnedMap<String, T>,
    key: Budgeted<String>,
    value: T,
    memory: &mut MemoryReservation,
) -> Result<(), SQLError> {
    memory.grow(OwnedMap::<String, T>::entry_bytes())?;
    result.insert(payload(key, memory), value);
    Ok(())
}

macro_rules! view_fields {
    ($input:ident, $read:ident $(, $async:ident)?) => {
        impl $input<'_> {
            $($async)? fn view_security(
                &mut self,
                memory: &mut MemoryReservation,
            ) -> Result<BoundTableSecurity, SQLError> {
                let role_owner = $read!(self.identity())?;
                let acl = payload($read!(self.acl())?, memory);
                let column_acls = view_map!($read, self, memory, |input, memory| {
                    Ok(payload($read!(input.acl_entries())?, memory))
                })?;
                let relation = $read!(self.optional_incarnation())?;
                let columns = view_map!($read, self, memory, |input, _| {
                    $read!(input.object_id())
                })?;
                Ok(BoundTableSecurity {
                    role_owner,
                    acl,
                    column_acls,
                    acl_revisions: RelationAclRevisions { relation, columns },
                })
            }

            $($async)? fn view_oids(
                &mut self,
                object_id: &[u8; 16],
                identities: &mut Identities,
            ) -> Result<Option<RelationCatalogOids>, SQLError> {
                if !$read!(self.flag())? {
                    return Ok(None);
                }
                let oids = RelationCatalogOids {
                    relation: $read!(self.catalog_oid())?,
                    row_type: $read!(self.optional_oid())?,
                    array_type: $read!(self.optional_oid())?,
                    rule: $read!(self.optional_oid())?,
                };
                if !oids.is_valid_for(RelationOidKind::View, object_id) {
                    return Err(invalid("view catalog OIDs"));
                }
                for oid in oids.claimed() {
                    identities.oid(oid)?;
                }
                Ok(Some(oids))
            }

            $($async)? fn view_header(
                &mut self,
                memory: &mut MemoryReservation,
                identities: &mut Identities,
            ) -> Result<StoredView, SQLError> {
                let security = $read!(self.view_security(memory))?;
                let object_id = $read!(self.object_id())?;
                identities.object(object_id)?;
                let output_columns = if $read!(self.flag())? {
                    Some(input_list!(
                        $read,
                        self,
                        memory,
                        |input, memory| { Ok(payload($read!(input.binary.text())?, memory)) }
                    )?)
                } else {
                    None
                };
                let persistence = match $read!(self.binary.array::<1>())?[0] {
                    0 => RelationPersistence::Permanent,
                    1 => RelationPersistence::Unlogged,
                    _ => return Err(invalid("durable view persistence")),
                };
                let options = input_list!($read, self, memory, |input, memory| {
                    let key = payload($read!(input.binary.text())?, memory);
                    let value = payload($read!(input.binary.text())?, memory);
                    Ok((key, value))
                })?;
                let kind = match $read!(self.binary.array::<1>())?[0] {
                    0 => StoredViewKind::View,
                    1 => StoredViewKind::Materialized,
                    _ => return Err(invalid("view kind")),
                };
                let materialized_rows = input_list!($read, self, memory, |input, memory| {
                    view_map!($read, input, memory, |input, memory| {
                        Ok(payload($read!(input.scalar_value())?, memory))
                    })
                })?;
                let materialized_column_types =
                    input_list!($read, self, memory, |input, memory| {
                        if $read!(input.flag())? {
                            Ok(Some(payload($read!(input.column_type())?, memory)))
                        } else {
                            Ok(None)
                        }
                    })?;
                let materialized_column_collations = if $read!(self.flag())? {
                    Some(input_list!($read, self, memory, |input, _| {
                        Ok(u32::from_le_bytes($read!(input.binary.array())?))
                    })?)
                } else {
                    None
                };
                let populated = $read!(self.flag())?;
                let catalog_oids = $read!(self.view_oids(&object_id, identities))?;
                let row_type_array_name = payload($read!(self.optional_text())?, memory);
                Ok(StoredView {
                    security,
                    definition: StoredViewDefinition {
                        object_id,
                        query: super::plans::empty_query(),
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
                })
            }

            $($async)? fn view_image(&mut self, identities: &mut Identities) -> Result<ViewImage, SQLError> {
                let mut memory = self.binary.memory.empty_reservation();
                // Header fields own no query tree. Only the completed native plan transfers its query and retirement owner into this image.
                let mut value = $read!(self.view_header(&mut memory, identities))?;
                let (query, owner) = $read!(self.stored_query())?;
                value.definition.query = query;
                Ok(ViewImage {
                    value,
                    query: owner,
                    memory,
                })
            }

            $($async)? fn view_entry(
                &mut self,
                result: &mut ViewDefinitions,
                identities: &mut Identities,
            ) -> Result<(), SQLError> {
                let key = $read!(self.durable_type_identity())?;
                if let Some(previous) = result.last_durable_key() {
                    let order = $read!(self.compare_text(&previous.schema, &key.schema))?;
                    if order == Ordering::Greater
                        || (order == Ordering::Equal
                            && $read!(self.compare_text(&previous.name, &key.name))?
                                != Ordering::Less)
                    {
                        return Err(invalid("view catalog key order"));
                    }
                }
                let value = $read!(self.view_image(identities))?;
                result.insert_owned_image(key, value)
            }
        }
    };
}
view_fields!(Input, read_field);
view_fields!(Fields, await_field, async);

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
        input.view_entry(&mut result, &mut identities)?;
    }
    input.binary.end()?;
    Ok(result)
}

#[cfg(test)]
mod tests;

impl ViewDefinitions {
    /// Retain native view fields, materialized values and query owners across independent entry, byte and poll windows. Future edits use the destination allowance.
    pub fn image_input(
        memory: &MemoryBudget,
        mutations: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<super::CatalogImageInput<Self>, SQLError> {
        super::continuation::registry(memory, mutations, cancellation)
    }
}

impl super::continuation::CatalogEntries for ViewDefinitions {
    type Scratch = Identities;
    const MAGIC: [u8; 8] = *b"UQAVIW01";
    fn create(memory: &MemoryBudget) -> (Self, Self::Scratch) {
        (Self::with_budget(memory), Identities::new(memory))
    }
    async fn read_entry(
        input: &mut Fields<'_>,
        result: &mut Self,
        identities: &mut Identities,
    ) -> Result<(), SQLError> {
        await_field!(input.view_entry(result, identities))
    }
    fn finish(self, mutations: &MemoryBudget) -> Self {
        self.into_mutation_budget(mutations)
    }
}
