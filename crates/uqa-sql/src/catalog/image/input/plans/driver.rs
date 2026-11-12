//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Fields, Frame, Input, Kind, NativeNode, Node, Owner, PlanMemory, SQLError};
use uqa_core::memory::BudgetedVec;

pub(in super::super) enum Step<K = Kind> {
    Advance,
    Child(K),
    Done,
}

impl Input<'_> {
    pub(super) fn native_plan(&mut self, kind: Kind) -> Result<Owner, SQLError> {
        self.native_tree::<Node>(kind)
    }

    pub(super) fn native_tree<N: NativeNode>(
        &mut self,
        kind: N::Kind,
    ) -> Result<Owner<N>, SQLError> {
        let retained_before = self.retained_children;
        let mut owner = Owner::<N>::new(self.binary.memory);
        owner.begin(kind)?;
        loop {
            self.binary.cancellation.check()?;
            let frame = owner.frames.last_mut().expect("active native plan");
            let step = N::read(frame, self, &mut owner.memory)?;
            if matches!(step, Step::Done) {
                N::validate(frame, self)?;
            }
            if owner.apply(step)? {
                owner
                    .memory
                    .retain_child(self.retained_children - retained_before)?;
                return Ok(owner);
            }
        }
    }
}

impl Frame {
    pub(super) fn attach(&mut self, child: Self) {
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

macro_rules! child_fields {
    ($input:ident, $read:ident $(, $async:ident)?) => {
        impl $input<'_> {
            pub(super) $($async)? fn plan_children<T, K>(
                &mut self,
                memory: &mut PlanMemory,
                count: &mut Option<usize>,
                values: &mut Vec<T>,
                kind: K,
            ) -> Result<Step<K>, SQLError> {
                if count.is_none() {
                    let declared = $read!(self.binary.count())?;
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

            pub(super) $($async)? fn plan_optional_child(&mut self, kind: Kind) -> Result<Step, SQLError> {
                Ok(if $read!(self.flag())? {
                    Step::Child(kind)
                } else {
                    Step::Advance
                })
            }

            pub(super) $($async)? fn plan_optional_box<T>(
                &mut self,
                memory: &mut PlanMemory,
                kind: Kind,
            ) -> Result<Step, SQLError> {
                if !$read!(self.flag())? {
                    return Ok(Step::Advance);
                }
                memory.grow(size_of::<T>())?;
                Ok(Step::Child(kind))
            }
        }
    };
}
child_fields!(Input, read_field);
child_fields!(Fields, await_field, async);

impl<N: NativeNode> Owner<N> {
    fn apply(&mut self, step: Step<N::Kind>) -> Result<bool, SQLError> {
        match step {
            Step::Advance => self.frames.last_mut().expect("active native plan").position += 1,
            Step::Child(kind) => self.begin(kind)?,
            Step::Done => {
                let frame = self.frames.pop().expect("completed native plan");
                if let Some(parent) = self.frames.last_mut() {
                    N::attach(parent, frame);
                } else {
                    self.root = frame.node;
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
}

impl Fields<'_> {
    pub(super) async fn native_plan(&mut self, kind: Kind) -> Result<Owner, SQLError> {
        await_field!(self.native_tree::<Node>(kind))
    }

    pub(super) async fn native_tree<N: NativeNode>(
        &mut self,
        kind: N::Kind,
    ) -> Result<Owner<N>, SQLError> {
        let retained_before = *self.retained_children;
        let mut owner = Owner::<N>::new(self.binary.memory);
        owner.begin(kind)?;
        loop {
            self.binary.checkpoint().await?;
            let frame = owner.frames.last_mut().expect("active native plan");
            let step = await_field!(N::read_fields(frame, self, &mut owner.memory))?;
            if matches!(step, Step::Done) {
                let mut position = 0;
                loop {
                    self.binary.checkpoint().await?;
                    if N::validate_step(frame, position)? {
                        break;
                    }
                    position += 1;
                }
            }
            if owner.apply(step)? {
                owner
                    .memory
                    .retain_child(*self.retained_children - retained_before)?;
                return Ok(owner);
            }
        }
    }
}

macro_rules! frame_input {
    ($method:ident, $input:ident, $read:ident $(, $async:ident)?) => {
        impl Frame {
            pub(super) $($async)? fn $method(
                &mut self,
                input: &mut $input<'_>,
                memory: &mut PlanMemory,
            ) -> Result<Step, SQLError> {
                match &mut self.node {
                    Node::Query(value) => $read!(input.plan_query_fields(
                        value,
                        self.position,
                        &mut self.count,
                        &mut self.references,
                        memory,
                    )),
                    Node::Source(value) => {
                        $read!(input.plan_source_fields(
                            value,
                            self.position,
                            &mut self.references,
                            memory
                        ))
                    }
                    Node::Cte(value) => {
                        $read!(input.plan_cte_fields(
                            value,
                            self.position,
                            &mut self.references,
                            memory
                        ))
                    }
                    Node::Command(value) => $read!(input.plan_command_fields(
                        value,
                        self.position,
                        &mut self.count,
                        &mut self.references,
                        &mut self.rule_references,
                        memory,
                    )),
                    Node::Expression(value) => match self.position {
                        0 => {
                            value.scalar =
                                $read!(input.plan_scalar(memory, &mut self.references))?.take();
                            Ok(Step::Advance)
                        }
                        1 => {
                            $read!(input.plan_children(
                                memory,
                                &mut self.count,
                                &mut value.subqueries,
                                Kind::Query
                            ))
                        }
                        _ => Ok(Step::Done),
                    },
                    Node::Empty => unreachable!("native plan constructor"),
                }
            }
        }
    };
}
frame_input!(read, Input, read_field);
frame_input!(read_fields, Fields, await_field, async);
