//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native trigger images borrow ordered relation roots and preserve bound declarations.

use super::{
    fields::{Field, Fields},
    invalid, DefinitionImageReader,
};
use crate::{
    ast::{CreateTrigger, TriggerEvent},
    catalog::{
        events::{
            CapturedEventCatalog, EventDefinitions, EventIter, StoredTrigger, TriggerCatalog,
        },
        named_definitions::CapturedDefinitionIter,
    },
    SQLError,
};
use uqa_core::{
    memory::{MemoryBudget, MemoryReservation},
    CancellationToken, RelationIdentity,
};

mod fields;
#[cfg(test)]
mod tests;

pub(super) const MAGIC: [u8; 8] = *b"UQATRG01";

enum Step<'a> {
    Relation,
    Trigger,
    Declaration(&'a CreateTrigger),
    Arguments(&'a CreateTrigger, usize),
    Events(&'a CreateTrigger, usize),
    Columns(&'a CreateTrigger, usize),
    Transitions(&'a CreateTrigger, usize),
    Condition(&'a CreateTrigger),
    Complete,
    Failed,
}

struct Condition<'a> {
    reader: Box<DefinitionImageReader<'a>>,
    _memory: MemoryReservation,
}

/// Stream a captured native trigger catalog without visiting declarations during preparation. Each read emits at most 16 KiB; a WHEN expression uses the admitted scalar-image owner.
pub struct TriggerImageReader<'a> {
    relations:
        Option<CapturedDefinitionIter<'a, RelationIdentity, EventDefinitions<StoredTrigger>>>,
    entries: Option<EventIter<'a, StoredTrigger>>,
    step: Step<'a>,
    fields: Fields<'a>,
    field: Field<'a>,
    offset: usize,
    condition: Option<Condition<'a>>,
    memory: &'a MemoryBudget,
    cancellation: &'a CancellationToken,
}

impl CapturedEventCatalog<StoredTrigger> {
    /// Borrow original identities, declaration order, enabled states and bound WHEN values. No trigger or function is executed and temporary relation roots are excluded by native capture.
    pub fn image_reader<'a>(
        &'a self,
        memory: &'a MemoryBudget,
        cancellation: &'a CancellationToken,
    ) -> Result<TriggerImageReader<'a>, SQLError> {
        cancellation.check()?;
        let mut fields = Fields::new();
        fields.scalar(MAGIC);
        fields.count(self.len());
        Ok(TriggerImageReader {
            relations: Some(self.iter()),
            entries: None,
            step: Step::Relation,
            fields,
            field: Field::Bytes(&[]),
            offset: 0,
            condition: None,
            memory,
            cancellation,
        })
    }
}

impl TriggerCatalog {
    /// Decode into an unpublished native catalog with admitted payloads. Cross-catalog function, relation and constraint validation remains with the enclosing restore owner.
    pub fn read_image(
        reader: &mut dyn std::io::Read,
        memory: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<Self, SQLError> {
        super::input::triggers(reader, memory, cancellation)
    }
}

impl TriggerImageReader<'_> {
    pub fn read(&mut self, output: &mut [u8]) -> Result<usize, SQLError> {
        match self.step {
            Step::Complete => return Ok(0),
            Step::Failed => return Err(invalid("failed trigger image reader")),
            _ => {}
        }
        let mut advance = Advance {
            reader: self,
            succeeded: false,
        };
        let count = advance.reader.read_active(output)?;
        advance.succeeded = true;
        Ok(count)
    }

    fn read_active(&mut self, output: &mut [u8]) -> Result<usize, SQLError> {
        self.cancellation.check()?;
        let limit = output.len().min(16384);
        let mut written = 0;
        while written < limit {
            self.cancellation.check()?;
            let bytes = self.field.bytes();
            if self.offset < bytes.len() {
                let count = (limit - written).min(bytes.len() - self.offset).min(4096);
                output[written..written + count]
                    .copy_from_slice(&bytes[self.offset..self.offset + count]);
                self.offset += count;
                written += count;
            } else if let Some(field) = self.fields.next() {
                self.field = field;
                self.offset = 0;
            } else if let Some(condition) = self.condition.as_mut() {
                let count = condition.reader.read(&mut output[written..limit])?;
                written += count;
                if count == 0 {
                    self.condition = None;
                }
            } else {
                self.fields = Fields::new();
                self.advance()?;
                if matches!(self.step, Step::Complete) {
                    break;
                }
            }
        }
        self.cancellation.check()?;
        Ok(written)
    }

    fn advance(&mut self) -> Result<(), SQLError> {
        self.step = match self.step {
            Step::Relation => {
                if let Some((relation, entries)) = self.relations.as_mut().and_then(Iterator::next)
                {
                    self.fields.text(&relation.schema);
                    self.fields.text(&relation.name);
                    self.fields.count(entries.len());
                    self.entries = Some(entries.iter());
                    Step::Trigger
                } else {
                    self.relations = None;
                    Step::Complete
                }
            }
            Step::Trigger => {
                if let Some((name, value)) = self.entries.as_mut().and_then(Iterator::next) {
                    self.fields.text(name);
                    self.fields.trigger(value);
                    Step::Declaration(&value.definition)
                } else {
                    self.entries = None;
                    Step::Relation
                }
            }
            Step::Declaration(value) => {
                self.fields.trigger_definition(value);
                Step::Arguments(value, 0)
            }
            Step::Arguments(value, position) => {
                if let Some(text) = value.arguments.get(position) {
                    self.fields.text(text);
                    Step::Arguments(value, position + 1)
                } else {
                    Step::Events(value, 0)
                }
            }
            Step::Events(value, position) => {
                if let Some(event) = value.events.get(position) {
                    self.fields.scalar([match event {
                        TriggerEvent::Insert => 0,
                        TriggerEvent::Update => 1,
                        TriggerEvent::Delete => 2,
                        TriggerEvent::Truncate => 3,
                    }]);
                    Step::Events(value, position + 1)
                } else {
                    Step::Columns(value, 0)
                }
            }
            Step::Columns(value, position) => {
                if let Some(text) = value.update_columns.get(position) {
                    self.fields.text(text);
                    Step::Columns(value, position + 1)
                } else {
                    Step::Transitions(value, 0)
                }
            }
            Step::Transitions(value, position) => {
                if let Some(relation) = value.transition_relations.get(position) {
                    let crate::ast::TriggerTransitionRelation {
                        name,
                        is_new,
                        is_table,
                    } = relation;
                    self.fields.text(name);
                    self.fields.flag(*is_new);
                    self.fields.flag(*is_table);
                    Step::Transitions(value, position + 1)
                } else {
                    Step::Condition(value)
                }
            }
            Step::Condition(value) => {
                if let Some(value) = &value.when {
                    let memory = self
                        .memory
                        .reserve(size_of::<DefinitionImageReader<'_>>())?;
                    let reader = Box::new(value.image_reader(self.memory, self.cancellation)?);
                    self.condition = Some(Condition {
                        reader,
                        _memory: memory,
                    });
                }
                Step::Trigger
            }
            Step::Complete | Step::Failed => unreachable!("terminal trigger reader"),
        };
        Ok(())
    }

    fn fail(&mut self) {
        self.step = Step::Failed;
        self.relations = None;
        self.entries = None;
        self.condition = None;
        self.fields = Fields::new();
        self.field = Field::Bytes(&[]);
        self.offset = 0;
    }
}

struct Advance<'reader, 'source> {
    reader: &'reader mut TriggerImageReader<'source>,
    succeeded: bool,
}
impl Drop for Advance<'_, '_> {
    fn drop(&mut self) {
        if !self.succeeded {
            self.reader.fail();
        }
    }
}
