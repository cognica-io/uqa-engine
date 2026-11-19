//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Trigger reconstruction transfers admitted native declarations without resolving dependencies.

use super::{expressions::payload, invalid, metadata::Identities, Fields, Input};
use crate::{
    ast::{
        CreateTrigger, EventEnableMode, TriggerDeferrability, TriggerEvent, TriggerTiming,
        TriggerTransitionRelation,
    },
    catalog::{
        events::{EventDefinitions, StoredTrigger, TriggerCatalog},
        temporary_namespace::is_temporary_schema_name,
    },
    SQLError,
};
use std::{cmp::Ordering, io::Read};
use uqa_core::{
    image_io::{decode_image, ImageDecoder},
    memory::{Budgeted, MemoryBudget, MemoryReservation},
    CancellationToken, RelationIdentity, RelationName,
};

impl Fields<'_> {
    pub(super) async fn event_relation(
        &mut self,
        previous: Option<&RelationIdentity>,
    ) -> Result<Budgeted<RelationIdentity>, SQLError> {
        let schema = self.binary.text().await?;
        let name = self.binary.text().await?;
        if schema.is_empty()
            || name.is_empty()
            || schema.contains('\0')
            || name.contains('\0')
            || is_temporary_schema_name(&schema)
        {
            return Err(invalid("event relation identity"));
        }
        if let Some(previous) = previous {
            let mut order = self.compare_text(&previous.schema, &schema).await?;
            if order == Ordering::Equal {
                order = self.compare_text(&previous.name, &name).await?;
            }
            if order != Ordering::Less {
                return Err(invalid("event relation order"));
            }
        }
        let (schema, mut memory) = schema.into_parts();
        let name = payload(name, &mut memory);
        Ok(Budgeted::new(RelationIdentity { schema, name }, memory))
    }

    async fn trigger_oid(&mut self, identities: &mut Identities) -> Result<Option<i64>, SQLError> {
        if !self.flag().await? {
            return Ok(None);
        }
        let oid = self.oid().await?;
        identities.oid(oid as u32)?;
        Ok(Some(oid))
    }

    async fn trigger(
        &mut self,
        key: &str,
        relation: &RelationIdentity,
        identities: &mut Identities,
        constraints: &mut Identities,
    ) -> Result<Budgeted<StoredTrigger>, SQLError> {
        let mut memory = self.binary.memory.empty_reservation();
        let function_object_id = self.optional_incarnation().await?;
        let enabled = match self.binary.array::<1>().await?[0] {
            0 => EventEnableMode::Origin,
            1 => EventEnableMode::Disabled,
            2 => EventEnableMode::Replica,
            3 => EventEnableMode::Always,
            _ => return Err(invalid("trigger enable mode")),
        };
        let object_id = self.optional_incarnation().await?;
        if let Some(id) = object_id {
            identities.object(id)?;
        }
        let constraint_name = payload(self.optional_text().await?, &mut memory);
        let catalog_oid = self.trigger_oid(identities).await?;
        let constraint_catalog_oid = self.trigger_oid(constraints).await?;
        let definition = self.trigger_definition(key, relation, &mut memory).await?;
        Ok(Budgeted::new(
            StoredTrigger {
                definition,
                function_object_id,
                enabled,
                object_id,
                constraint_name,
                catalog_oid,
                constraint_catalog_oid,
            },
            memory,
        ))
    }

    async fn trigger_definition(
        &mut self,
        key: &str,
        relation: &RelationIdentity,
        memory: &mut MemoryReservation,
    ) -> Result<CreateTrigger, SQLError> {
        let (mut definition, has_when) = self.trigger_declaration(key, relation, memory).await?;
        if has_when {
            if self.header(super::super::expressions::MAGIC).await? != 1 {
                return Err(invalid("trigger condition count"));
            }
            definition.when = Some(payload(self.expression().await?, memory));
        }
        Ok(definition)
    }

    async fn trigger_declaration(
        &mut self,
        key: &str,
        relation: &RelationIdentity,
        memory: &mut MemoryReservation,
    ) -> Result<(CreateTrigger, bool), SQLError> {
        let name = payload(self.binary.text().await?, memory);
        if name.contains('\0') || self.compare_text(&name, key).await? != Ordering::Equal {
            return Err(invalid("trigger declaration key"));
        }
        let table = payload(self.binary.text().await?, memory);
        let reference = RelationIdentity::borrowed_reference(&table)
            .ok_or_else(|| invalid("trigger declaration relation"))?;
        if reference.schema.unwrap_or(RelationName::decoded("public"))
            != RelationName::decoded(&relation.schema)
            || reference.name != RelationName::decoded(&relation.name)
        {
            return Err(invalid("trigger declaration relation"));
        }
        let function = payload(self.binary.text().await?, memory);
        let constraint = self.flag().await?;
        let referenced_table = payload(self.optional_text().await?, memory);
        let deferrability = match self.binary.array::<1>().await?[0] {
            0 => TriggerDeferrability::NotDeferrable,
            1 => TriggerDeferrability::InitiallyImmediate,
            2 => TriggerDeferrability::InitiallyDeferred,
            _ => return Err(invalid("trigger deferrability")),
        };
        let row = self.flag().await?;
        let timing = match self.binary.array::<1>().await?[0] {
            0 => TriggerTiming::Before,
            1 => TriggerTiming::After,
            2 => TriggerTiming::InsteadOf,
            _ => return Err(invalid("trigger timing")),
        };
        let or_replace = self.flag().await?;
        let has_when = self.flag().await?;
        let arguments = self.binary.count().await?;
        let events = self.binary.count().await?;
        let columns = self.binary.count().await?;
        let transitions = self.binary.count().await?;
        let arguments = payload(self.image_values(arguments).await?, memory);
        let events = payload(self.image_values(events).await?, memory);
        let update_columns = payload(self.image_values(columns).await?, memory);
        let transition_relations = payload(self.image_values(transitions).await?, memory);
        Ok((
            CreateTrigger {
                name,
                table,
                function,
                arguments,
                constraint,
                referenced_table,
                deferrability,
                row,
                timing,
                events,
                update_columns,
                transition_relations,
                when: None,
                or_replace,
            },
            has_when,
        ))
    }
}

impl super::collections::ImageValue for TriggerEvent {
    async fn read(input: &mut Fields<'_>) -> Result<Budgeted<Self>, SQLError> {
        let value = match input.binary.array::<1>().await?[0] {
            0 => Self::Insert,
            1 => Self::Update,
            2 => Self::Delete,
            3 => Self::Truncate,
            _ => return Err(invalid("trigger event")),
        };
        Ok(Budgeted::new(
            value,
            input.binary.memory.empty_reservation(),
        ))
    }
}
impl super::collections::ImageValue for TriggerTransitionRelation {
    async fn read(input: &mut Fields<'_>) -> Result<Budgeted<Self>, SQLError> {
        let mut memory = input.binary.memory.empty_reservation();
        let name = payload(input.binary.text().await?, &mut memory);
        let is_new = input.flag().await?;
        let is_table = input.flag().await?;
        Ok(Budgeted::new(
            Self {
                name,
                is_new,
                is_table,
            },
            memory,
        ))
    }
}

impl Input<'_> {
    pub(super) fn event_relation(
        &mut self,
        previous: Option<&RelationIdentity>,
    ) -> Result<Budgeted<RelationIdentity>, SQLError> {
        self.fields(async |mut input| input.event_relation(previous).await)
    }
}

async fn decode(
    mut input: Fields<'_>,
    mutations: &MemoryBudget,
) -> Result<TriggerCatalog, SQLError> {
    let count = input.header(super::super::triggers::MAGIC).await?;
    let memory = input.binary.memory;
    let mut result = TriggerCatalog::with_budget(memory);
    let mut identities = Identities::new(memory);
    let mut constraints = Identities::new(memory);
    for _ in 0..count {
        input.binary.checkpoint().await?;
        let relation = input.event_relation(result.last_durable_key()).await?;
        let count = input.binary.count().await?;
        let mut entries = EventDefinitions::with_budget(memory);
        for _ in 0..count {
            input.binary.checkpoint().await?;
            let key = input.ordered_name(entries.last_key()).await?;
            let value = input
                .trigger(&key, &relation, &mut identities, &mut constraints)
                .await?;
            entries.insert_image_entry(key, value)?;
        }
        result.insert_image_entry(relation, entries.into_mutation_budget(mutations))?;
    }
    input.binary.end().await?;
    Ok(result.into_mutation_budget(mutations))
}

pub(in super::super) fn triggers(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    mutations: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<TriggerCatalog, SQLError> {
    decode_image(reader, memory, cancellation, async |binary| {
        decode(
            Fields {
                binary,
                retained_children: &mut 0,
            },
            mutations,
        )
        .await
    })
}

impl TriggerCatalog {
    /// Retain ordered relations, original trigger identities and incomplete declarations/conditions across byte and poll windows. Every nested registry transfers future edits to the destination allowance.
    pub fn image_input(
        memory: &MemoryBudget,
        mutations: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<ImageDecoder<Self, SQLError>, SQLError> {
        let mutations = mutations.clone();
        Ok(ImageDecoder::new(
            memory,
            cancellation,
            |mut reader| async move {
                decode(
                    Fields {
                        binary: reader.input(),
                        retained_children: &mut 0,
                    },
                    &mutations,
                )
                .await
            },
        )?)
    }
}
