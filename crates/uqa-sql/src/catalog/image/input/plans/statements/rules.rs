//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rule declarations and bound conditions keep independent iterative destruction forests.

use super::{
    super::{Kind as PlanKind, Node as PlanNode, Owner},
    invalid, Body, Empty, Field, Input, Kind, Node, PlanMemory, SQLError, Step,
};
use crate::{
    ast::{CreateRule, EventEnableMode, RuleEvent},
    catalog::{
        events::{EventDefinitions, RuleCatalog, StoredRule},
        image::input::metadata::Identities,
        named_definitions::DefinitionImage,
    },
};
use std::{cmp::Ordering, io::Read};
use uqa_core::{
    memory::{MemoryBudget, MemoryError, MemoryReservation},
    CancellationToken, RelationIdentity, RelationName,
};

mod metadata;
#[cfg(test)]
mod tests;

empty! { RuleEvent => RuleEvent::Select }
leaf!(RuleEvent, |input, _memory| Ok::<_, SQLError>(
    match input.binary.array::<1>()?[0] {
        0 => Self::Select,
        1 => Self::Insert,
        2 => Self::Update,
        3 => Self::Delete,
        _ => return Err(invalid("rule event")),
    }
));
syntax_struct!(CreateRule {
    name,
    table,
    event,
    instead,
    condition,
    actions,
    or_replace
});

struct RuleImage {
    value: StoredRule,
    definition: Owner<Node>,
    condition: Option<Owner>,
    memory: MemoryReservation,
}

impl Drop for RuleImage {
    fn drop(&mut self) {
        let Node::Rule(definition) = &mut self.definition.root else {
            unreachable!("native rule definition owner")
        };
        std::mem::swap(definition, &mut self.value.definition);
        if let Some(owner) = &mut self.condition {
            let PlanNode::Expression(condition) = &mut owner.root else {
                unreachable!("native rule condition owner")
            };
            std::mem::swap(
                condition,
                self.value
                    .condition_plan
                    .as_mut()
                    .expect("paired native rule condition"),
            );
        }
    }
}

impl DefinitionImage<StoredRule> for RuleImage {
    fn definition(&self) -> &StoredRule {
        &self.value
    }
    fn budget(&self) -> &MemoryBudget {
        self.memory.budget()
    }
    fn retained_bytes(&self) -> Result<usize, MemoryError> {
        let condition = self
            .condition
            .as_ref()
            .map_or(Ok(0), Owner::retained_bytes)?;
        self.memory
            .bytes()
            .checked_add(self.definition.retained_bytes()?)
            .and_then(|bytes| bytes.checked_add(condition))
            .ok_or(MemoryError::SizeOverflow)
    }
}

impl Input<'_> {
    fn rule(
        &mut self,
        key: &str,
        relation: &RelationIdentity,
        identities: &mut Identities,
    ) -> Result<RuleImage, SQLError> {
        let mut definition = self.native_tree::<Node>(Kind::Rule)?;
        let Node::Rule(value) = &mut definition.root else {
            unreachable!("native rule declaration")
        };
        let value = std::mem::replace(value, <CreateRule as Empty>::empty());
        let mut image = RuleImage {
            value: StoredRule {
                definition: value,
                enabled: EventEnableMode::Origin,
                condition_plan: None,
                condition_binding: None,
                dependencies: None,
                catalog_oid: None,
            },
            definition,
            condition: None,
            memory: self.binary.memory.empty_reservation(),
        };
        if image.value.definition.name.contains('\0')
            || self.compare_text(&image.value.definition.name, key)? != Ordering::Equal
        {
            return Err(invalid("rule declaration key"));
        }
        let reference = RelationIdentity::borrowed_reference(&image.value.definition.table)
            .ok_or_else(|| invalid("rule declaration relation"))?;
        if reference.schema.unwrap_or(RelationName::decoded("public"))
            != RelationName::decoded(&relation.schema)
            || reference.name != RelationName::decoded(&relation.name)
        {
            return Err(invalid("rule declaration relation"));
        }
        image.value.enabled = match self.binary.array::<1>()?[0] {
            0 => EventEnableMode::Origin,
            1 => EventEnableMode::Disabled,
            2 => EventEnableMode::Replica,
            3 => EventEnableMode::Always,
            _ => return Err(invalid("rule enable mode")),
        };
        if self.flag()? {
            let mut owner = self.native_plan(PlanKind::Expression)?;
            let PlanNode::Expression(condition) = &mut owner.root else {
                unreachable!("native rule condition")
            };
            let value = std::mem::replace(
                condition,
                crate::plan::ExpressionPlan {
                    scalar: crate::ScalarExpr::Star,
                    subqueries: Vec::new(),
                },
            );
            image.value.condition_plan = Some(value);
            image.condition = Some(owner);
        }
        image.value.condition_binding = self.rule_binding(&mut image.memory)?;
        image.value.dependencies = self.rule_dependencies(&mut image.memory)?;
        image.value.catalog_oid = if self.flag()? {
            let oid = self.oid()?;
            identities.oid(oid as u32)?;
            Some(oid)
        } else {
            None
        };
        Ok(image)
    }
}

pub(in crate::catalog::image) fn read(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<RuleCatalog, SQLError> {
    let mut input = Input::new(reader, memory, cancellation);
    let count = input.header(*b"UQARUL01")?;
    let mut result = RuleCatalog::with_budget(memory);
    let mut identities = Identities::new(memory);
    for _ in 0..count {
        cancellation.check()?;
        let relation = input.event_relation(result.last_durable_key())?;
        let count = input.binary.count()?;
        let mut entries = EventDefinitions::with_budget(memory);
        for _ in 0..count {
            cancellation.check()?;
            let key = input.ordered_name(entries.last_key())?;
            let value = input.rule(&key, &relation, &mut identities)?;
            entries.insert_owned_image(key, value)?;
        }
        result.insert_image_entry(relation, entries)?;
    }
    input.binary.end()?;
    Ok(result)
}
