//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, Input, MemoryReservation, Ordering, RelationIdentity, SQLError};
use crate::{
    ast::InternalRelationId,
    catalog::{
        events::{
            RuleColumnDependency, RuleConditionBinding, RuleDependencies, RuleRoutineDependency,
        },
        image::input::expressions::payload,
    },
};
use uqa_core::memory::OwnedSet;

impl Input<'_> {
    fn rule_texts(&mut self, memory: &mut MemoryReservation) -> Result<Vec<String>, SQLError> {
        let count = self.binary.count()?;
        self.items(count, memory, |input, memory| {
            Ok(payload(input.binary.text()?, memory))
        })
    }

    fn rule_row_identity(&mut self) -> Result<Option<InternalRelationId>, SQLError> {
        self.flag()?
            .then(|| {
                self.binary
                    .array()
                    .map(|bytes| InternalRelationId::from_raw(u64::from_le_bytes(bytes)))
                    .map_err(Into::into)
            })
            .transpose()
    }

    pub(super) fn rule_binding(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<Option<RuleConditionBinding>, SQLError> {
        if !self.flag()? {
            return Ok(None);
        }
        let old = self.rule_row_identity()?;
        let new = self.rule_row_identity()?;
        let columns = self.rule_texts(memory)?;
        Ok(Some(RuleConditionBinding::from_captured_parts(
            old, new, columns,
        )))
    }

    fn rule_relation(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<RelationIdentity, SQLError> {
        let schema = payload(self.binary.text()?, memory);
        let name = payload(self.binary.text()?, memory);
        if schema.is_empty() || name.is_empty() || schema.contains('\0') || name.contains('\0') {
            return Err(invalid("rule dependency relation"));
        }
        Ok(RelationIdentity { schema, name })
    }

    fn rule_set<T: Ord>(
        &mut self,
        memory: &mut MemoryReservation,
        mut read: impl FnMut(&mut Self, &mut MemoryReservation) -> Result<T, SQLError>,
        mut compare: impl FnMut(&Self, &T, &T) -> Result<Ordering, SQLError>,
    ) -> Result<OwnedSet<T>, SQLError> {
        let count = self.binary.count()?;
        let mut result = OwnedSet::new();
        let mut previous = None;
        for _ in 0..count {
            self.binary.cancellation.check()?;
            let value = read(self, memory)?;
            if let Some(previous) = previous.take() {
                if compare(self, &previous, &value)? != Ordering::Less {
                    return Err(invalid("rule dependency order"));
                }
                memory.grow(OwnedSet::<T>::entry_bytes())?;
                result.insert(previous);
            }
            previous = Some(value);
        }
        if let Some(value) = previous {
            memory.grow(OwnedSet::<T>::entry_bytes())?;
            result.insert(value);
        }
        Ok(result)
    }

    fn rule_relation_order(
        &self,
        a: &RelationIdentity,
        b: &RelationIdentity,
    ) -> Result<Ordering, SQLError> {
        let order = self.compare_text(&a.schema, &b.schema)?;
        if order == Ordering::Equal {
            self.compare_text(&a.name, &b.name)
        } else {
            Ok(order)
        }
    }

    fn rule_routine_order(
        &self,
        a: &RuleRoutineDependency,
        b: &RuleRoutineDependency,
    ) -> Result<Ordering, SQLError> {
        let mut order = a.object_id.cmp(&b.object_id);
        if order == Ordering::Equal {
            order = self.compare_text(&a.name, &b.name)?;
        }
        if order != Ordering::Equal {
            return Ok(order);
        }
        for (a, b) in a.argument_types.iter().zip(&b.argument_types) {
            order = self.compare_text(a, b)?;
            if order != Ordering::Equal {
                return Ok(order);
            }
        }
        Ok(a.argument_types.len().cmp(&b.argument_types.len()))
    }

    pub(super) fn rule_dependencies(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<Option<RuleDependencies>, SQLError> {
        if !self.flag()? {
            return Ok(None);
        }
        let relations = self.rule_set(memory, Self::rule_relation, Self::rule_relation_order)?;
        let columns = self.rule_set(
            memory,
            |input, memory| {
                Ok(RuleColumnDependency {
                    relation: input.rule_relation(memory)?,
                    column: payload(input.binary.text()?, memory),
                })
            },
            |input, a, b| {
                let order = input.rule_relation_order(&a.relation, &b.relation)?;
                if order == Ordering::Equal {
                    input.compare_text(&a.column, &b.column)
                } else {
                    Ok(order)
                }
            },
        )?;
        let routines = self.rule_set(
            memory,
            |input, memory| {
                Ok(RuleRoutineDependency {
                    object_id: input.optional_incarnation()?,
                    name: payload(input.binary.text()?, memory),
                    argument_types: input.rule_texts(memory)?,
                })
            },
            Self::rule_routine_order,
        )?;
        Ok(Some(RuleDependencies {
            relations,
            columns,
            routines,
        }))
    }
}
