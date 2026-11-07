//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rewrite-rule images preserve original syntax, bound conditions and dependency membership.

use super::{fields, variant, DefinitionImageReader, Frame, ImageFrame, Node, Part, SQLError};
use crate::{
    ast::{CreateRule, EventEnableMode, RuleEvent},
    catalog::events::{
        CapturedEventCatalog, RuleCatalog, RuleColumnDependency, RuleConditionBinding,
        RuleDependencies, RuleRoutineDependency, StoredRule,
    },
};
use uqa_core::{
    memory::{MemoryBudget, MemoryReservation, OwnedSet, OwnedSetIter},
    CancellationToken, RelationIdentity,
};

impl CapturedEventCatalog<StoredRule> {
    /// Borrow the selected durable generation without resolving, rewriting or executing its statements.
    pub fn image_reader<'a>(
        &'a self,
        memory: &'a MemoryBudget,
        cancellation: &'a CancellationToken,
    ) -> Result<DefinitionImageReader<'a>, SQLError> {
        let mut reader = DefinitionImageReader::start(
            ImageFrame::NextRuleRelation,
            *b"UQARUL01",
            self.len(),
            memory,
            cancellation,
        )?;
        reader.rules = Some(self.iter());
        reader.statement_expressions = true;
        Ok(reader)
    }
}

impl RuleCatalog {
    /// Reconstruct an unpublished immutable catalog without remapping its captured OLD/NEW identities. Cross-catalog validation and process-local activation remain with restore.
    pub fn read_image(
        reader: &mut dyn std::io::Read,
        memory: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<Self, SQLError> {
        super::super::super::input::rules(reader, memory, cancellation)
    }
}

image_struct!(StoredRule {
    definition,
    enabled,
    condition_plan,
    condition_binding,
    dependencies,
    catalog_oid
});
image_struct!(CreateRule {
    name,
    table,
    event,
    instead,
    condition,
    actions,
    or_replace
});
image_struct!(RuleDependencies {
    relations,
    columns,
    routines
});
image_struct!(RuleColumnDependency { relation, column });
image_struct!(RuleRoutineDependency {
    object_id,
    name,
    argument_types
});
image_struct!(RelationIdentity { schema, name });
image_enum!(RuleEvent { 0 => Select, 1 => Insert, 2 => Update, 3 => Delete });
image_enum!(EventEnableMode { 0 => Origin, 1 => Disabled, 2 => Replica, 3 => Always });

impl Node for i64 {
    fn part(&self, position: usize) -> Result<Option<Part<'_>>, SQLError> {
        Ok((position == 0).then_some(Part::U64(*self as u64)))
    }
}

impl Node for RuleConditionBinding {
    fn part(&self, position: usize) -> Result<Option<Part<'_>>, SQLError> {
        let (old, new, columns) = self.captured_parts();
        Ok(fields(position, &[old, new, columns]))
    }
}

pub(in super::super) enum Set<'a> {
    Relations(&'a OwnedSet<RelationIdentity>),
    Columns(&'a OwnedSet<RuleColumnDependency>),
    Routines(&'a OwnedSet<RuleRoutineDependency>),
}
pub(in super::super) enum Cursor<'a> {
    Relations(OwnedSetIter<'a, RelationIdentity>),
    Columns(OwnedSetIter<'a, RuleColumnDependency>),
    Routines(OwnedSetIter<'a, RuleRoutineDependency>),
}

macro_rules! set_node {
    ($ty:ty, $variant:ident) => {
        impl Node for OwnedSet<$ty> {
            fn part(&self, position: usize) -> Result<Option<Part<'_>>, SQLError> {
                Ok(match position {
                    0 => Some(Part::Count(self.len())),
                    1 => Some(Part::RuleSet(Set::$variant(self))),
                    _ => None,
                })
            }
        }
    };
}
set_node!(RelationIdentity, Relations);
set_node!(RuleColumnDependency, Columns);
set_node!(RuleRoutineDependency, Routines);

impl<'a> DefinitionImageReader<'a> {
    pub(in super::super) fn next_rule_relation(&mut self) -> Result<(), SQLError> {
        if let Some((relation, entries)) = self.rules.as_mut().and_then(Iterator::next) {
            self.fields.text(&relation.schema);
            self.fields.text(&relation.name);
            self.fields.count(entries.len());
            self.rule_entries = Some(entries.iter());
            self.push(ImageFrame::NextRuleRelation)?;
            self.push(ImageFrame::NextRule)?;
        }
        Ok(())
    }

    pub(in super::super) fn next_rule(&mut self) -> Result<(), SQLError> {
        if let Some((name, value)) = self.rule_entries.as_mut().and_then(Iterator::next) {
            self.fields.text(name);
            self.push(ImageFrame::NextRule)?;
            self.push(ImageFrame::Plan(Frame::Node(value, 0)))?;
        } else {
            self.rule_entries = None;
        }
        Ok(())
    }

    pub(super) fn start_rule_set(&mut self, set: Set<'a>) -> Result<(), SQLError> {
        let memory = self.memory.reserve(size_of::<Cursor<'_>>())?;
        let cursor = match set {
            Set::Relations(values) => Cursor::Relations(values.iter()),
            Set::Columns(values) => Cursor::Columns(values.iter()),
            Set::Routines(values) => Cursor::Routines(values.iter()),
        };
        self.push(ImageFrame::Plan(Frame::RuleSet(Box::new(cursor), memory)))
    }

    pub(super) fn rule_set(
        &mut self,
        mut cursor: Box<Cursor<'a>>,
        memory: MemoryReservation,
    ) -> Result<(), SQLError> {
        let next: Option<&dyn Node> = match cursor.as_mut() {
            Cursor::Relations(values) => values.next().map(|value| value as &dyn Node),
            Cursor::Columns(values) => values.next().map(|value| value as &dyn Node),
            Cursor::Routines(values) => values.next().map(|value| value as &dyn Node),
        };
        if let Some(value) = next {
            self.push(ImageFrame::Plan(Frame::RuleSet(cursor, memory)))?;
            self.push(ImageFrame::Plan(Frame::Node(value, 0)))?;
        }
        Ok(())
    }
}
