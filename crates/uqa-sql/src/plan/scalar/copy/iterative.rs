//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Suspended scalar constructors retain their exact allocation owners in admitted heap frames.

use super::{Control, Lowering, OwnedScalarExpr, Result, ScalarExpr, ScalarMemory};
use uqa_core::memory::{BudgetedVec, MemoryReservation, Produced, ProductionControl};

mod parts;
mod steps;
use parts::{Parts, Slot};

#[cfg(test)]
mod tests;

enum Memory {
    Ordinary,
    Immutable(MemoryReservation),
    Mutable(ScalarMemory),
}

struct Copied {
    // The copied tree is destroyed before any of its reservations.
    expression: ScalarExpr,
    memory: Memory,
}

struct Frame<'s, 'c> {
    source: &'s ScalarExpr,
    // Partial fields, including completed children, precede their allocation owners.
    parts: Parts,
    lowering: Lowering<'c>,
    stage: usize,
    index: usize,
    pending: Option<Slot>,
}

impl<'s, 'c> Frame<'s, 'c> {
    fn new(source: &'s ScalarExpr, control: ProductionControl<'c>, mutable: bool) -> Result<Self> {
        control.check()?;
        let mut lowering = Lowering {
            control: control.budget().map(|_| Control::from_production(control)),
            owned_children: control.budget().filter(|_| mutable).map(BudgetedVec::new),
        };
        let parts = Parts::new(source, &mut lowering)?;
        Ok(Self {
            source,
            parts,
            lowering,
            stage: 0,
            index: 0,
            pending: None,
        })
    }

    fn next_child(&mut self) -> Result<Option<&'s ScalarExpr>> {
        self.lowering.check()?;
        let next = self.step()?;
        if let Some((source, slot)) = next {
            if let Some(children) = &mut self.lowering.owned_children {
                children.reserve(1)?;
            }
            self.pending = Some(slot);
            Ok(Some(source))
        } else {
            Ok(None)
        }
    }

    fn attach(&mut self, child: Copied) -> Result<()> {
        self.lowering.check()?;
        let slot = self.pending.take().expect("suspended scalar child");
        let Copied { expression, memory } = child;
        // The metadata slot was admitted before descending into this child. Installing its completed fields cannot allocate except for already-admitted parent boxes.
        match memory {
            Memory::Ordinary => assert!(self.lowering.control.is_none()),
            Memory::Immutable(memory) => self
                .lowering
                .control
                .as_mut()
                .expect("controlled copy")
                .memory
                .absorb(memory),
            Memory::Mutable(memory) => self
                .lowering
                .owned_children
                .as_mut()
                .expect("mutable copy")
                .push(Some(memory))
                .expect("admitted child slot"),
        }
        self.parts.attach(slot, expression);
        Ok(())
    }

    fn finish(mut self) -> Result<Copied> {
        self.lowering.check()?;
        let parts = std::mem::replace(&mut self.parts, Parts::Empty);
        let expression = parts.finish(self.source, &mut self.lowering)?;
        self.lowering.check()?;
        let memory = match self.lowering.control.take() {
            None => Memory::Ordinary,
            Some(control) => match self.lowering.owned_children.take() {
                Some(children) => Memory::Mutable(ScalarMemory {
                    direct: control.memory,
                    children,
                }),
                None => Memory::Immutable(control.memory),
            },
        };
        Ok(Copied { expression, memory })
    }

    fn reserve_box(&mut self) -> Result<()> {
        if let Some(control) = &mut self.lowering.control {
            control.memory.grow(size_of::<ScalarExpr>())?;
        }
        Ok(())
    }

    fn advance(&mut self) {
        self.stage += 1;
        self.index = 0;
    }
}

enum Frames<'s, 'c> {
    Ordinary(Vec<Frame<'s, 'c>>),
    Controlled(BudgetedVec<Frame<'s, 'c>>),
}

struct Stack<'s, 'c> {
    // Shallow constructors use a fixed inline buffer; additional ancestors retain admitted heap storage.
    inline: [Option<Frame<'s, 'c>>; 4],
    len: usize,
    frames: Frames<'s, 'c>,
}

impl<'s, 'c> Stack<'s, 'c> {
    fn new(control: ProductionControl<'c>) -> Self {
        Self {
            inline: std::array::from_fn(|_| None),
            len: 0,
            frames: control.budget().map_or_else(
                || Frames::Ordinary(Vec::new()),
                |budget| Frames::Controlled(BudgetedVec::new(budget)),
            ),
        }
    }

    fn push(&mut self, frame: Frame<'s, 'c>) -> Result<()> {
        if self.len < self.inline.len() {
            self.inline[self.len] = Some(frame);
            self.len += 1;
            return Ok(());
        }
        match &mut self.frames {
            Frames::Ordinary(frames) => frames.push(frame),
            Frames::Controlled(frames) => frames.push(frame)?,
        }
        Ok(())
    }

    fn pop(&mut self) -> Option<Frame<'s, 'c>> {
        let frame = match &mut self.frames {
            Frames::Ordinary(frames) => frames.pop(),
            Frames::Controlled(frames) => frames.pop(),
        };
        frame.or_else(|| {
            self.len = self.len.checked_sub(1)?;
            self.inline[self.len].take()
        })
    }
}

fn copy(source: &ScalarExpr, control: ProductionControl<'_>, mutable: bool) -> Result<Copied> {
    let mut current = Frame::new(source, control, mutable)?;
    let mut stack = Stack::new(control);
    loop {
        control.check()?;
        if let Some(child) = current.next_child()? {
            stack.push(current)?;
            current = Frame::new(child, control, mutable)?;
        } else {
            let child = current.finish()?;
            let Some(mut parent) = stack.pop() else {
                return Ok(child);
            };
            parent.attach(child)?;
            current = parent;
        }
    }
}

pub(super) fn mutable(
    source: &ScalarExpr,
    control: ProductionControl<'_>,
) -> Result<OwnedScalarExpr> {
    let Copied { expression, memory } = copy(source, control, true)?;
    Ok(OwnedScalarExpr {
        expression,
        memory: match memory {
            Memory::Ordinary => None,
            Memory::Mutable(memory) => Some(memory),
            Memory::Immutable(_) => unreachable!("mutable copy output"),
        },
    })
}

pub(super) fn immutable(
    source: &ScalarExpr,
    control: ProductionControl<'_>,
) -> Result<Produced<ScalarExpr>> {
    let Copied { expression, memory } = copy(source, control, false)?;
    Ok(control.finish(
        expression,
        match memory {
            Memory::Ordinary => None,
            Memory::Immutable(memory) => Some(memory),
            Memory::Mutable(_) => unreachable!("immutable copy output"),
        },
    )?)
}

fn vector<T>(count: usize, lowering: &mut Lowering<'_>) -> Result<Vec<T>> {
    lowering.check()?;
    let Some(control) = &mut lowering.control else {
        return Ok(Vec::with_capacity(count));
    };
    let mut output = BudgetedVec::new(control.memory.budget());
    output.reserve(count)?;
    let (output, memory) = output.into_parts();
    control.memory.absorb(memory);
    Ok(output)
}
