//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Frame, Input, Kind, MemoryReservation, Node, Owner, SQLError};
use uqa_core::memory::BudgetedVec;

pub(super) enum Step {
    Advance,
    Child(Kind),
    Done,
}

impl Input<'_> {
    pub(super) fn native_plan(&mut self, kind: Kind) -> Result<Owner, SQLError> {
        let mut owner = Owner::new(self.binary.memory);
        owner.begin(kind)?;
        loop {
            self.binary.cancellation.check()?;
            let frame = owner.frames.last_mut().expect("active native plan");
            match frame.read(self, &mut owner.memory)? {
                Step::Advance => frame.position += 1,
                Step::Child(kind) => owner.begin(kind)?,
                Step::Done => {
                    frame.validate(self)?;
                    let frame = owner.frames.pop().expect("completed native plan");
                    if let Some(parent) = owner.frames.last_mut() {
                        parent.attach(frame);
                    } else {
                        owner.root = frame.node;
                        return Ok(owner);
                    }
                }
            }
        }
    }

    pub(super) fn plan_children<T>(
        &mut self,
        memory: &mut MemoryReservation,
        count: &mut Option<usize>,
        values: &mut Vec<T>,
        kind: Kind,
    ) -> Result<Step, SQLError> {
        if count.is_none() {
            let declared = self.binary.count()?;
            let mut buffer = BudgetedVec::new(self.binary.memory);
            buffer.reserve(declared)?;
            let (buffer, lease) = buffer.into_parts();
            *values = buffer;
            memory.absorb(lease);
            *count = Some(declared);
        }
        if values.len() < count.expect("declared native child count") {
            Ok(Step::Child(kind))
        } else {
            *count = None;
            Ok(Step::Advance)
        }
    }

    pub(super) fn plan_optional_child(&mut self, kind: Kind) -> Result<Step, SQLError> {
        Ok(if self.flag()? {
            Step::Child(kind)
        } else {
            Step::Advance
        })
    }

    pub(super) fn plan_optional_box<T>(
        &mut self,
        memory: &mut MemoryReservation,
        kind: Kind,
    ) -> Result<Step, SQLError> {
        if !self.flag()? {
            return Ok(Step::Advance);
        }
        memory.grow(size_of::<T>())?;
        Ok(Step::Child(kind))
    }
}

impl Frame {
    fn read(
        &mut self,
        input: &mut Input<'_>,
        memory: &mut MemoryReservation,
    ) -> Result<Step, SQLError> {
        match &mut self.node {
            Node::Query(value) => super::queries::read(
                value,
                self.position,
                &mut self.count,
                &mut self.references,
                input,
                memory,
            ),
            Node::Source(value) => {
                super::sources::read(value, self.position, &mut self.references, input, memory)
            }
            Node::Cte(value) => {
                super::ctes::read(value, self.position, &mut self.references, input, memory)
            }
            Node::Command(value) => super::mutations::read(
                value,
                self.position,
                &mut self.count,
                &mut self.references,
                &mut self.rule_references,
                input,
                memory,
            ),
            Node::Expression(value) => match self.position {
                0 => {
                    value.scalar = input.plan_scalar(memory, &mut self.references)?.take();
                    Ok(Step::Advance)
                }
                1 => {
                    input.plan_children(memory, &mut self.count, &mut value.subqueries, Kind::Query)
                }
                _ => Ok(Step::Done),
            },
            Node::Empty => unreachable!("native plan constructor"),
        }
    }

    fn attach(&mut self, child: Self) {
        // Child storage and destination boxes were admitted before descent. Moving into the native parent cannot fail or release its payload lease.
        if matches!(child.node, Node::Source(_)) {
            self.references.absorb(child.references);
        }
        match &mut self.node {
            Node::Query(value) => super::queries::attach(value, &mut self.position, child.node),
            Node::Source(value) => super::sources::attach(value, &mut self.position, child.node),
            Node::Cte(value) => super::ctes::attach(value, &mut self.position, child.node),
            Node::Command(value) => super::mutations::attach(value, &mut self.position, child.node),
            Node::Expression(value) => {
                let Node::Query(query) = child.node else {
                    unreachable!("expression query arena")
                };
                value.subqueries.push(query);
            }
            Node::Empty => unreachable!("native plan parent"),
        }
    }
}
