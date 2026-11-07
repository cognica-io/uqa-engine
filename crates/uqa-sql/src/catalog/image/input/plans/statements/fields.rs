//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Input, Kind, Node, PlanMemory, SQLError, Step};

/// Empty field contents cannot contain another syntax node or allocate retirement storage.
pub(super) trait Empty {
    fn empty() -> Self;
    fn is_flat(&self) -> bool {
        false
    }
}

pub(super) trait Field {
    fn new(memory: &mut PlanMemory) -> Result<Self, SQLError>
    where
        Self: Sized;
    fn read(&mut self, input: &mut Input<'_>, memory: &mut PlanMemory) -> Result<Step, SQLError>;
    fn attach(&mut self, _child: Node) {
        unreachable!("leaf syntax field")
    }
    fn retire(&mut self) -> Option<Node> {
        None
    }
    fn needs_retirement(&self) -> bool {
        false
    }
}

pub(super) trait Tree: Body + Sized {
    const KIND: Kind;
    fn into_node(self) -> Node;
    fn from_node(node: Node) -> Self;
}

pub(super) trait Body {
    fn new(memory: &mut PlanMemory) -> Result<Self, SQLError>
    where
        Self: Sized;
    fn field(&mut self, _position: usize) -> Option<&mut dyn Field> {
        None
    }
    fn first_field(&self) -> usize {
        0
    }
    fn read(
        &mut self,
        position: usize,
        _count: &mut Option<usize>,
        input: &mut Input<'_>,
        memory: &mut PlanMemory,
    ) -> Result<Step, SQLError> {
        self.field(position)
            .map_or(Ok(Step::Done), |field| field.read(input, memory))
    }
    fn attach(&mut self, position: &mut usize, child: Node) {
        self.field(*position)
            .expect("suspended syntax field")
            .attach(child);
        *position += 1;
    }
    fn retire(&mut self, position: &mut usize) -> Option<Node> {
        *position = (*position).max(self.first_field());
        while let Some(field) = self.field(*position) {
            *position += 1;
            if let Some(child) = field.retire() {
                return Some(child);
            }
        }
        None
    }
}

impl<T> Empty for Vec<T> {
    fn empty() -> Self {
        Self::new()
    }
    fn is_flat(&self) -> bool {
        self.is_empty()
    }
}
impl<T> Empty for Option<T> {
    fn empty() -> Self {
        None
    }
}

impl<T: Field> Field for Option<T> {
    fn new(_: &mut PlanMemory) -> Result<Self, SQLError> {
        Ok(None)
    }
    fn read(&mut self, input: &mut Input<'_>, memory: &mut PlanMemory) -> Result<Step, SQLError> {
        if !input.flag()? {
            return Ok(Step::Advance);
        }
        *self = Some(T::new(memory)?);
        self.as_mut()
            .expect("admitted optional syntax")
            .read(input, memory)
    }
    fn attach(&mut self, child: Node) {
        self.as_mut()
            .expect("suspended optional syntax")
            .attach(child);
    }
    fn retire(&mut self) -> Option<Node> {
        self.take().and_then(|mut value| value.retire())
    }
    fn needs_retirement(&self) -> bool {
        self.as_ref().is_some_and(Field::needs_retirement)
    }
}

impl<T: Field> Field for Box<T> {
    fn new(memory: &mut PlanMemory) -> Result<Self, SQLError> {
        memory.grow(size_of::<T>())?;
        Ok(Box::new(T::new(memory)?))
    }
    fn read(&mut self, input: &mut Input<'_>, memory: &mut PlanMemory) -> Result<Step, SQLError> {
        self.as_mut().read(input, memory)
    }
    fn attach(&mut self, child: Node) {
        self.as_mut().attach(child);
    }
    fn retire(&mut self) -> Option<Node> {
        self.as_mut().retire()
    }
    fn needs_retirement(&self) -> bool {
        true
    }
}

impl<T: Tree> Body for Vec<T> {
    fn new(_: &mut PlanMemory) -> Result<Self, SQLError> {
        Ok(Vec::new())
    }
    fn read(
        &mut self,
        position: usize,
        count: &mut Option<usize>,
        input: &mut Input<'_>,
        memory: &mut PlanMemory,
    ) -> Result<Step, SQLError> {
        if position != 0 {
            return Ok(Step::Done);
        }
        input.plan_children(memory, count, self, T::KIND)
    }
    fn attach(&mut self, _: &mut usize, child: Node) {
        self.push(T::from_node(child));
    }
    fn retire(&mut self, _: &mut usize) -> Option<Node> {
        self.pop().map(T::into_node)
    }
}
