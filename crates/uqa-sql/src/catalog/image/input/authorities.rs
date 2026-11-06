//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native routine and system relation authority is decoded before any catalog publication.

use super::{invalid, Input};
use crate::{
    ast::ObjectAclEntry,
    catalog::{
        security::{
            builtin_routines::{BuiltinRoutineSecurities, BuiltinRoutineSecurity},
            system_relations::{SystemAcl, SystemRelationSecurities, SystemRelationSecurity},
        },
        SystemRelation,
    },
    SQLError,
};
use std::{cmp::Ordering, io::Read, ops::Bound};
use uqa_core::{
    memory::{Budgeted, BudgetedVec, MemoryBudget, OwnedMap, ProductionControl},
    CancellationToken, RelationIdentity,
};

impl Input<'_> {
    fn revision(&mut self) -> Result<[u8; 16], SQLError> {
        let revision = self.binary.array()?;
        if revision == [0; 16] {
            return Err(invalid("authority tuple revision"));
        }
        Ok(revision)
    }

    fn routine_acl(&mut self) -> Result<Budgeted<Vec<ObjectAclEntry>>, SQLError> {
        let count = self.binary.count()?;
        let mut entries = BudgetedVec::new(self.binary.memory);
        entries.reserve(count)?;
        for _ in 0..count {
            self.binary.cancellation.check()?;
            let role = if self.flag()? {
                Some(self.identity()?)
            } else {
                None
            };
            entries.push(ObjectAclEntry {
                role,
                grantor: self.identity()?,
                grant_option: self.flag()?,
            })?;
        }
        let (entries, reservation) = entries.into_parts();
        Ok(Budgeted::new(entries, reservation))
    }

    fn system_acl(&mut self) -> Result<Budgeted<SystemAcl>, SQLError> {
        let revision = self.revision()?;
        let (acl, reservation) = self.acl_entries()?.into_parts();
        Ok(Budgeted::new(SystemAcl { revision, acl }, reservation))
    }

    fn relation_key(
        &mut self,
        previous: Option<&RelationIdentity>,
    ) -> Result<(Budgeted<RelationIdentity>, SystemRelation), SQLError> {
        let schema = self.binary.text()?;
        let name = self.binary.text()?;
        let relation = SystemRelation::at(&schema, &name)
            .ok_or_else(|| invalid("system relation identity"))?;
        if let Some(previous) = previous {
            let mut ordering = self.compare_text(&previous.schema, &schema)?;
            if ordering == Ordering::Equal {
                ordering = self.compare_text(&previous.name, &name)?;
            }
            if ordering != Ordering::Less {
                return Err(invalid("system relation key order"));
            }
        }
        let (schema, mut reservation) = schema.into_parts();
        let (name, name_memory) = name.into_parts();
        reservation.absorb(name_memory);
        Ok((
            Budgeted::new(RelationIdentity { schema, name }, reservation),
            relation,
        ))
    }

    fn relation_authority(
        &mut self,
        relation: SystemRelation,
    ) -> Result<Budgeted<SystemRelationSecurity>, SQLError> {
        let count = self.binary.count()?;
        // Declare the aggregate lease first so partial native values retire before admission.
        let mut reservation = self.binary.memory.empty_reservation();
        let mut value = SystemRelationSecurity::default();
        if self.flag()? {
            let (table, table_memory) = self.system_acl()?.into_parts();
            reservation.absorb(table_memory);
            value.table = Some(table);
        }
        if count != 0 {
            let control = ProductionControl::new(
                self.binary.memory,
                self.binary.cancellation,
                self.binary.cancellation,
            );
            let columns = relation.column_names_with_control(&control)?;
            if count > columns.len() {
                return Err(invalid("system column count"));
            }
            for _ in 0..count {
                self.binary.cancellation.check()?;
                let name = self.binary.text()?;
                if !columns.contains(&name)
                    || value.columns.first_from(Bound::Included(&*name)).is_some()
                {
                    return Err(invalid("system column identity or order"));
                }
                let acl = self.system_acl()?;
                reservation.grow(OwnedMap::<String, SystemAcl>::entry_bytes())?;
                let (name, name_memory) = name.into_parts();
                let (acl, acl_memory) = acl.into_parts();
                reservation.absorb(name_memory);
                reservation.absorb(acl_memory);
                value.columns.insert(name, acl);
            }
        }
        Ok(Budgeted::new(value, reservation))
    }
}

pub(in super::super) fn builtin_routines(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<BuiltinRoutineSecurities, SQLError> {
    let mut input = Input::new(reader, memory, cancellation);
    let count = input.header(super::super::BUILTIN_ROUTINES)?;
    let mut result = BuiltinRoutineSecurities::with_budget(memory);
    for _ in 0..count {
        cancellation.check()?;
        let oid = u32::from_le_bytes(input.binary.array()?);
        if oid == 0
            || result
                .last_durable_key()
                .is_some_and(|previous| *previous >= oid)
        {
            return Err(invalid("builtin routine key order"));
        }
        let revision = input.revision()?;
        let (execute_acl, reservation) = input.routine_acl()?.into_parts();
        result.insert_image_entry(
            Budgeted::new(oid, memory.empty_reservation()),
            Budgeted::new(
                BuiltinRoutineSecurity {
                    revision,
                    execute_acl,
                },
                reservation,
            ),
        )?;
    }
    input.binary.end()?;
    Ok(result)
}

pub(in super::super) fn system_relations(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<SystemRelationSecurities, SQLError> {
    let mut input = Input::new(reader, memory, cancellation);
    let count = input.header(super::super::SYSTEM_RELATIONS)?;
    let mut result = SystemRelationSecurities::with_budget(memory);
    for _ in 0..count {
        cancellation.check()?;
        let (key, relation) = input.relation_key(result.last_durable_key())?;
        let security = input.relation_authority(relation)?;
        result.insert_image_entry(key, security)?;
    }
    input.binary.end()?;
    Ok(result)
}
