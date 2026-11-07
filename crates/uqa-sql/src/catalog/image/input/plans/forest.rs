//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One admitted traversal owns native plans and original syntax without inflating either node representation.

use super::{driver::Step, Frame, Input, Kind, Node, PlanMemory, SQLError};

pub(in super::super) trait NativeNode: Sized {
    type Kind;
    fn empty() -> Self;
    fn is_empty(&self) -> bool;
    fn new(kind: Self::Kind, memory: &mut PlanMemory) -> Result<Self, SQLError>;
    fn read(
        frame: &mut Frame<Self>,
        input: &mut Input<'_>,
        memory: &mut PlanMemory,
    ) -> Result<Step<Self::Kind>, SQLError>;
    fn validate(frame: &mut Frame<Self>, input: &Input<'_>) -> Result<(), SQLError>;
    fn attach(frame: &mut Frame<Self>, child: Frame<Self>);
    fn retire_scalars(frame: &mut Frame<Self>);
    fn retire_child(frame: &mut Frame<Self>) -> Option<Self>;
}

impl NativeNode for Node {
    type Kind = Kind;
    fn empty() -> Self {
        Self::Empty
    }
    fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }
    fn new(kind: Kind, memory: &mut PlanMemory) -> Result<Self, SQLError> {
        Self::new(kind, memory)
    }
    fn read(
        frame: &mut Frame,
        input: &mut Input<'_>,
        memory: &mut PlanMemory,
    ) -> Result<Step, SQLError> {
        frame.read(input, memory)
    }
    fn validate(frame: &mut Frame, input: &Input<'_>) -> Result<(), SQLError> {
        frame.validate(input)
    }
    fn attach(frame: &mut Frame, child: Frame) {
        frame.attach(child);
    }
    fn retire_scalars(frame: &mut Frame) {
        frame.retire_scalars();
    }
    fn retire_child(frame: &mut Frame) -> Option<Self> {
        frame.retire_child()
    }
}
