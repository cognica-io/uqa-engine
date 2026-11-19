//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Suspended scalar constructors retain their exact allocation owners in admitted heap frames.

use super::{Control, Lowering, OwnedScalarExpr, Result, ScalarExpr, ScalarMemory};
use crate::{
    catalog::work::inline,
    construction::Stack,
    plan::{retirement::Pending, scalar::resources::input::Work},
    schema::retention::CatalogRetentionError,
};
use uqa_core::memory::{MemoryReservation, Produced, ProductionControl};

mod parts;
mod steps;
use parts::{Parts, Slot};

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, PartialEq, Eq)]
enum CopyMode {
    Immutable,
    Mutable,
    Capture,
}

impl From<bool> for CopyMode {
    fn from(mutable: bool) -> Self {
        if mutable {
            Self::Mutable
        } else {
            Self::Immutable
        }
    }
}

enum Memory {
    Ordinary,
    Immutable(MemoryReservation),
    Mutable(ScalarMemory),
}

struct Copied {
    // The copied tree is destroyed before any of its reservations.
    expression: Pending<ScalarExpr>,
    memory: Memory,
    retained_children: usize,
}

struct Frame<'s, 'c> {
    source: &'s ScalarExpr,
    // Partial fields, including completed children, precede their allocation owners.
    parts: Pending<Parts>,
    lowering: Lowering<'c>,
    stage: usize,
    index: usize,
    pending: Option<Slot>,
    retained_children: usize,
}

impl<'s, 'c> Frame<'s, 'c> {
    #[cfg(test)]
    fn new(source: &'s ScalarExpr, control: ProductionControl<'c>, mode: CopyMode) -> Result<Self> {
        inline(Self::new_from(source, control, mode, &mut ()))
    }

    async fn new_from<W: Work>(
        source: &'s ScalarExpr,
        control: ProductionControl<'c>,
        mode: CopyMode,
        work: &mut W,
    ) -> std::result::Result<Self, W::Error>
    where
        W::Error: From<CatalogRetentionError>,
    {
        control.check()?;
        let mut lowering = Lowering {
            control: control.budget().map(|_| Control::from_production(control)),
            owned_children: control
                .budget()
                .filter(|_| mode == CopyMode::Mutable)
                .map(|budget| ScalarMemory::new(budget.empty_reservation())),
        };
        lowering.checkpoint(work).await?;
        let (parts, retained_children) = match (mode, source) {
            (CopyMode::Capture, ScalarExpr::RetainedLiteral(literal)) => {
                let (literal, bytes) = literal.capture_with_footprint_from(&control, work).await?;
                (Parts::Leaf(ScalarExpr::RetainedLiteral(literal)), bytes)
            }
            _ => (Parts::new_from(source, &mut lowering, work).await?, 0),
        };
        Ok(Self {
            source,
            parts: Pending::new(parts),
            lowering,
            stage: 0,
            index: 0,
            pending: None,
            retained_children,
        })
    }

    fn next_child(&mut self) -> Result<Option<&'s ScalarExpr>> {
        self.lowering.check()?;
        let next = self.step()?;
        if let Some((source, slot)) = next {
            if let Some(children) = &mut self.lowering.owned_children {
                children.reserve_children(1)?;
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
        self.retained_children = self
            .retained_children
            .checked_add(child.retained_children)
            .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?;
        let Copied {
            expression, memory, ..
        } = child;
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
                .push_child(Some(memory))
                .expect("admitted child slot"),
        }
        self.parts.attach(slot, expression.take());
        Ok(())
    }

    #[cfg(test)]
    fn finish(self) -> Result<Copied> {
        inline(self.finish_from(&mut ()))
    }

    async fn finish_from<W: Work>(mut self, work: &mut W) -> std::result::Result<Copied, W::Error> {
        self.lowering.checkpoint(work).await?;
        let parts = std::mem::replace(&mut self.parts, Pending::new(Parts::Empty));
        let expression = Parts::finish_from(parts, self.source, &mut self.lowering, work).await?;
        self.lowering.checkpoint(work).await?;
        let memory = match self.lowering.control.take() {
            None => Memory::Ordinary,
            Some(control) => match self.lowering.owned_children.take() {
                Some(children) => {
                    Memory::Mutable(ScalarMemory::from_parts(control.memory, children))
                }
                None => Memory::Immutable(control.memory),
            },
        };
        Ok(Copied {
            expression,
            memory,
            retained_children: self.retained_children,
        })
    }

    fn reserve_box(&mut self) -> Result<()> {
        self.lowering.admit_box::<ScalarExpr>()
    }

    fn advance(&mut self) {
        self.stage += 1;
        self.index = 0;
    }
}

fn copy(source: &ScalarExpr, control: ProductionControl<'_>, mode: CopyMode) -> Result<Copied> {
    // Immediate copies keep their existing shallow workspace without another heap reservation.
    inline(copy_from::<_, 4>(source, control, mode, &mut ()))
}

async fn copy_from<W: Work, const N: usize>(
    source: &ScalarExpr,
    control: ProductionControl<'_>,
    mode: CopyMode,
    work: &mut W,
) -> std::result::Result<Copied, W::Error>
where
    W::Error: From<CatalogRetentionError>,
{
    let mut stack = Stack::<_, N>::new(control);
    let mut source = source;
    loop {
        let mut current = Frame::new_from(source, control, mode, work).await?;
        loop {
            current.lowering.checkpoint(work).await?;
            if let Some(child) = current.next_child()? {
                stack.push(current)?;
                source = child;
                break;
            }
            let child = current.finish_from(work).await?;
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
    Ok(copy(source, control, CopyMode::Mutable)?.into_mutable())
}

pub(super) async fn mutable_from<W: Work>(
    source: &ScalarExpr,
    control: ProductionControl<'_>,
    work: &mut W,
) -> std::result::Result<OwnedScalarExpr, W::Error>
where
    W::Error: From<CatalogRetentionError>,
{
    // Suspended copies admit their ancestor frames without enlarging the enclosing poll frame.
    Ok(copy_from::<_, 0>(source, control, CopyMode::Mutable, work)
        .await?
        .into_mutable())
}

impl Copied {
    fn into_mutable(self) -> OwnedScalarExpr {
        OwnedScalarExpr {
            expression: self.expression.take(),
            memory: match self.memory {
                Memory::Ordinary => None,
                Memory::Mutable(memory) => Some(memory),
                Memory::Immutable(_) => unreachable!("mutable copy output"),
            },
        }
    }
}

pub(super) fn immutable(
    source: &ScalarExpr,
    control: ProductionControl<'_>,
) -> Result<Produced<ScalarExpr>> {
    Ok(copy(source, control, CopyMode::Immutable)?
        .into_captured(&control)?
        .0)
}

pub(super) fn capture(
    source: &ScalarExpr,
    control: ProductionControl<'_>,
) -> Result<(Produced<ScalarExpr>, usize)> {
    copy(source, control, CopyMode::Capture)?.into_captured(&control)
}

pub(super) async fn capture_from<W: Work>(
    source: &ScalarExpr,
    control: ProductionControl<'_>,
    work: &mut W,
) -> std::result::Result<(Produced<ScalarExpr>, usize), W::Error>
where
    W::Error: From<CatalogRetentionError>,
{
    Ok(copy_from::<_, 0>(source, control, CopyMode::Capture, work)
        .await?
        .into_captured(&control)?)
}

impl Copied {
    fn into_captured(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<(Produced<ScalarExpr>, usize)> {
        Ok((
            self.expression.finish(
                match self.memory {
                    Memory::Ordinary => None,
                    Memory::Immutable(memory) => Some(memory),
                    Memory::Mutable(_) => unreachable!("immutable copy output"),
                },
                control,
            )?,
            self.retained_children,
        ))
    }
}
