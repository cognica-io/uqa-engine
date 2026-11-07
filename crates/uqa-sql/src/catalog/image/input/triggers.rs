//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Trigger reconstruction transfers admitted native declarations without resolving dependencies.

use super::{expressions::payload, invalid, metadata::Identities, Input};
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
    memory::{Budgeted, MemoryBudget, MemoryReservation},
    CancellationToken, RelationIdentity, RelationName,
};

impl Input<'_> {
    fn trigger_relation(
        &mut self,
        previous: Option<&RelationIdentity>,
    ) -> Result<Budgeted<RelationIdentity>, SQLError> {
        let schema = self.binary.text()?;
        let name = self.binary.text()?;
        if schema.is_empty()
            || name.is_empty()
            || schema.contains('\0')
            || name.contains('\0')
            || is_temporary_schema_name(&schema)
        {
            return Err(invalid("trigger relation identity"));
        }
        if let Some(previous) = previous {
            let mut order = self.compare_text(&previous.schema, &schema)?;
            if order == Ordering::Equal {
                order = self.compare_text(&previous.name, &name)?;
            }
            if order != Ordering::Less {
                return Err(invalid("trigger relation order"));
            }
        }
        let (schema, mut memory) = schema.into_parts();
        let name = payload(name, &mut memory);
        Ok(Budgeted::new(RelationIdentity { schema, name }, memory))
    }

    fn optional_incarnation(&mut self) -> Result<Option<[u8; 16]>, SQLError> {
        if self.flag()? {
            Ok(Some(self.object_id()?))
        } else {
            Ok(None)
        }
    }

    fn trigger_oid(&mut self, identities: &mut Identities) -> Result<Option<i64>, SQLError> {
        if !self.flag()? {
            return Ok(None);
        }
        let oid = self.oid()?;
        identities.oid(oid as u32)?;
        Ok(Some(oid))
    }

    fn trigger(
        &mut self,
        key: &str,
        relation: &RelationIdentity,
        identities: &mut Identities,
        constraints: &mut Identities,
    ) -> Result<Budgeted<StoredTrigger>, SQLError> {
        let mut memory = self.binary.memory.empty_reservation();
        let function_object_id = self.optional_incarnation()?;
        let enabled = match self.binary.array::<1>()?[0] {
            0 => EventEnableMode::Origin,
            1 => EventEnableMode::Disabled,
            2 => EventEnableMode::Replica,
            3 => EventEnableMode::Always,
            _ => return Err(invalid("trigger enable mode")),
        };
        let object_id = self.optional_incarnation()?;
        if let Some(id) = object_id {
            identities.object(id)?;
        }
        let constraint_name = payload(self.optional_text()?, &mut memory);
        let catalog_oid = self.trigger_oid(identities)?;
        let constraint_catalog_oid = self.trigger_oid(constraints)?;
        let definition = self.trigger_definition(key, relation, &mut memory)?;
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

    fn trigger_definition(
        &mut self,
        key: &str,
        relation: &RelationIdentity,
        memory: &mut MemoryReservation,
    ) -> Result<CreateTrigger, SQLError> {
        let name = payload(self.binary.text()?, memory);
        if name.contains('\0') || self.compare_text(&name, key)? != Ordering::Equal {
            return Err(invalid("trigger declaration key"));
        }
        let table = payload(self.binary.text()?, memory);
        let reference = RelationIdentity::borrowed_reference(&table)
            .ok_or_else(|| invalid("trigger declaration relation"))?;
        if reference.schema.unwrap_or(RelationName::decoded("public"))
            != RelationName::decoded(&relation.schema)
            || reference.name != RelationName::decoded(&relation.name)
        {
            return Err(invalid("trigger declaration relation"));
        }
        let function = payload(self.binary.text()?, memory);
        let constraint = self.flag()?;
        let referenced_table = payload(self.optional_text()?, memory);
        let deferrability = match self.binary.array::<1>()?[0] {
            0 => TriggerDeferrability::NotDeferrable,
            1 => TriggerDeferrability::InitiallyImmediate,
            2 => TriggerDeferrability::InitiallyDeferred,
            _ => return Err(invalid("trigger deferrability")),
        };
        let row = self.flag()?;
        let timing = match self.binary.array::<1>()?[0] {
            0 => TriggerTiming::Before,
            1 => TriggerTiming::After,
            2 => TriggerTiming::InsteadOf,
            _ => return Err(invalid("trigger timing")),
        };
        let or_replace = self.flag()?;
        let has_when = self.flag()?;
        let arguments = self.binary.count()?;
        let events = self.binary.count()?;
        let columns = self.binary.count()?;
        let transitions = self.binary.count()?;
        let arguments = self.items(arguments, memory, |input, memory| {
            Ok(payload(input.binary.text()?, memory))
        })?;
        let events = self.items(events, memory, |input, _| {
            Ok(match input.binary.array::<1>()?[0] {
                0 => TriggerEvent::Insert,
                1 => TriggerEvent::Update,
                2 => TriggerEvent::Delete,
                3 => TriggerEvent::Truncate,
                _ => return Err(invalid("trigger event")),
            })
        })?;
        let update_columns = self.items(columns, memory, |input, memory| {
            Ok(payload(input.binary.text()?, memory))
        })?;
        let transition_relations = self.items(transitions, memory, |input, memory| {
            Ok(TriggerTransitionRelation {
                name: payload(input.binary.text()?, memory),
                is_new: input.flag()?,
                is_table: input.flag()?,
            })
        })?;
        let when = if has_when {
            if self.header(super::super::expressions::MAGIC)? != 1 {
                return Err(invalid("trigger condition count"));
            }
            Some(payload(self.expression()?, memory))
        } else {
            None
        };
        Ok(CreateTrigger {
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
            when,
            or_replace,
        })
    }
}

pub(in super::super) fn triggers(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<TriggerCatalog, SQLError> {
    let mut input = Input::new(reader, memory, cancellation);
    let count = input.header(super::super::triggers::MAGIC)?;
    let mut result = TriggerCatalog::with_budget(memory);
    let mut identities = Identities::new(memory);
    let mut constraints = Identities::new(memory);
    for _ in 0..count {
        cancellation.check()?;
        let relation = input.trigger_relation(result.last_durable_key())?;
        let count = input.binary.count()?;
        let mut entries = EventDefinitions::with_budget(memory);
        for _ in 0..count {
            cancellation.check()?;
            let key = input.ordered_name(entries.last_key())?;
            let value = input.trigger(&key, &relation, &mut identities, &mut constraints)?;
            entries.insert_image_entry(key, value)?;
        }
        result.insert_image_entry(relation, entries)?;
    }
    input.binary.end()?;
    Ok(result)
}
