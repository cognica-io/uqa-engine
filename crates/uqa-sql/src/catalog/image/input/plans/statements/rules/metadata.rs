//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, Fields, Input, MemoryReservation, Ordering, RelationIdentity, SQLError};
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

macro_rules! rule_set {
    ($ty:ty, $read:ident, $owner:expr, $allowance:expr, |$input:ident, $memory:ident| $body:block, |$order_input:ident, $left:ident, $right:ident| $compare:block) => {{
        let count = $read!($owner.binary.count())?;
        let mut result = OwnedSet::new();
        let mut previous: Option<$ty> = None;
        for _ in 0..count {
            $read!($owner.checkpoint())?;
            let $input = &mut *$owner;
            let $memory = &mut *$allowance;
            let value: Result<_, SQLError> = $body;
            let value = value?;
            if let Some(prior) = previous.take() {
                let $order_input = &mut *$owner;
                let $left = &prior;
                let $right = &value;
                let order: Result<Ordering, SQLError> = $compare;
                if order? != Ordering::Less {
                    return Err(invalid("rule dependency order"));
                }
                insert_dependency(&mut result, prior, $allowance)?;
            }
            previous = Some(value);
        }
        if let Some(value) = previous {
            insert_dependency(&mut result, value, $allowance)?;
        }
        Ok::<_, SQLError>(result)
    }};
}

fn insert_dependency<T: Ord>(
    values: &mut OwnedSet<T>,
    value: T,
    memory: &mut MemoryReservation,
) -> Result<(), SQLError> {
    memory.grow(OwnedSet::<T>::entry_bytes())?;
    values.insert(value);
    Ok(())
}

macro_rules! rule_metadata {
    ($input:ident, $read:ident $(, $async:ident)?) => {
        impl $input<'_> {
            $($async)? fn rule_texts(
                &mut self,
                memory: &mut MemoryReservation,
            ) -> Result<Vec<String>, SQLError> {
                input_list!($read, self, memory, |input, memory| {
                    Ok(payload($read!(input.binary.text())?, memory))
                })
            }
            $($async)? fn rule_row_identity(&mut self) -> Result<Option<InternalRelationId>, SQLError> {
                Ok(if $read!(self.flag())? {
                    Some(InternalRelationId::from_raw(u64::from_le_bytes(
                        $read!(self.binary.array())?,
                    )))
                } else {
                    None
                })
            }
            pub(super) $($async)? fn rule_binding(
                &mut self,
                memory: &mut MemoryReservation,
            ) -> Result<Option<RuleConditionBinding>, SQLError> {
                if !$read!(self.flag())? {
                    return Ok(None);
                }
                let old = $read!(self.rule_row_identity())?;
                let new = $read!(self.rule_row_identity())?;
                let columns = $read!(self.rule_texts(memory))?;
                Ok(Some(RuleConditionBinding::from_captured_parts(
                    old, new, columns,
                )))
            }
            $($async)? fn rule_relation(
                &mut self,
                memory: &mut MemoryReservation,
            ) -> Result<RelationIdentity, SQLError> {
                let schema = payload($read!(self.binary.text())?, memory);
                let name = payload($read!(self.binary.text())?, memory);
                if schema.is_empty() || name.is_empty() || schema.contains('\0') || name.contains('\0') {
                    return Err(invalid("rule dependency relation"));
                }
                Ok(RelationIdentity { schema, name })
            }
            $($async)? fn rule_relation_order(
                &mut self,
                a: &RelationIdentity,
                b: &RelationIdentity,
            ) -> Result<Ordering, SQLError> {
                let order = $read!(self.compare_text(&a.schema, &b.schema))?;
                if order == Ordering::Equal {
                    $read!(self.compare_text(&a.name, &b.name))
                } else {
                    Ok(order)
                }
            }
            $($async)? fn rule_routine_order(
                &mut self,
                a: &RuleRoutineDependency,
                b: &RuleRoutineDependency,
            ) -> Result<Ordering, SQLError> {
                let mut order = a.object_id.cmp(&b.object_id);
                if order == Ordering::Equal {
                    order = $read!(self.compare_text(&a.name, &b.name))?;
                }
                if order != Ordering::Equal {
                    return Ok(order);
                }
                for (a, b) in a.argument_types.iter().zip(&b.argument_types) {
                    $read!(self.checkpoint())?;
                    order = $read!(self.compare_text(a, b))?;
                    if order != Ordering::Equal {
                        return Ok(order);
                    }
                }
                Ok(a.argument_types.len().cmp(&b.argument_types.len()))
            }
            pub(super) $($async)? fn rule_dependencies(
                &mut self,
                memory: &mut MemoryReservation,
            ) -> Result<Option<RuleDependencies>, SQLError> {
                if !$read!(self.flag())? {
                    return Ok(None);
                }
                let relations = rule_set!(
                    RelationIdentity,
                    $read,
                    self,
                    memory,
                    |input, memory| { $read!(input.rule_relation(memory)) },
                    |input, a, b| { $read!(input.rule_relation_order(a, b)) }
                )?;
                let columns = rule_set!(
                    RuleColumnDependency,
                    $read,
                    self,
                    memory,
                    |input, memory| {
                        Ok(RuleColumnDependency {
                            relation: $read!(input.rule_relation(memory))?,
                            column: payload($read!(input.binary.text())?, memory),
                        })
                    },
                    |input, a, b| {
                        let order = $read!(input.rule_relation_order(&a.relation, &b.relation))?;
                        if order == Ordering::Equal {
                            $read!(input.compare_text(&a.column, &b.column))
                        } else {
                            Ok(order)
                        }
                    }
                )?;
                let routines = rule_set!(
                    RuleRoutineDependency,
                    $read,
                    self,
                    memory,
                    |input, memory| {
                        Ok(RuleRoutineDependency {
                            object_id: $read!(input.optional_incarnation())?,
                            name: payload($read!(input.binary.text())?, memory),
                            argument_types: $read!(input.rule_texts(memory))?,
                        })
                    },
                    |input, a, b| { $read!(input.rule_routine_order(a, b)) }
                )?;
                Ok(Some(RuleDependencies {
                    relations,
                    columns,
                    routines,
                }))
            }
        }
    };
}
rule_metadata!(Input, read_field);
rule_metadata!(Fields, await_field, async);
