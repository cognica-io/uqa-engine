//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Existing field slots retain ancestor links while constructor records retire. Immediate and suspended folds share each transition without allocation or recursive cleanup.

use super::{footprint::FieldPath, MemoryReservation, RewriteField, RewriteMemory};
use crate::{catalog::work::Work, plan::scalar::ScalarMemoryFolding, SQLError};
use uqa_core::memory::ProductionControl;

impl Drop for RewriteField {
    fn drop(&mut self) {
        if let Some(memory) = self.memory.take() {
            drop(fold(memory));
        }
    }
}

pub(super) fn fold(memory: RewriteMemory) -> Option<MemoryReservation> {
    let mut folding = Folding::new(memory);
    folding.drain();
    folding.finish()
}

enum Step {
    Descend,
    Release,
    Ascend,
}

pub(in crate::plan::mutable) struct Folding {
    current: Option<RewriteMemory>,
    path: FieldPath,
    scalar: Option<ScalarMemoryFolding>,
    retained: Option<MemoryReservation>,
    step: Step,
}

impl Folding {
    pub(in crate::plan::mutable) fn new(memory: RewriteMemory) -> Self {
        Self {
            current: Some(memory),
            path: FieldPath::default(),
            scalar: None,
            retained: None,
            step: Step::Descend,
        }
    }

    pub(in crate::plan::mutable) async fn retire_from<W: Work>(
        mut self,
        control: Option<&ProductionControl<'_>>,
        work: &mut W,
    ) -> Result<(), SQLError> {
        self.advance_from(control, work).await
    }

    pub(in crate::plan::mutable) async fn advance_from<W: Work>(
        &mut self,
        control: Option<&ProductionControl<'_>>,
        work: &mut W,
    ) -> Result<(), SQLError> {
        while !self.is_complete() {
            if let Some(control) = control {
                control.check()?;
            }
            work.checkpoint().await?;
            if let Some(control) = control {
                control.check()?;
            }
            self.advance();
        }
        Ok(())
    }

    pub(in crate::plan::mutable) fn drain(&mut self) {
        while !self.advance() {}
    }

    /// Detach one constructor field or scalar owner, advance one scalar-fold edge, or release one completed record.
    fn advance(&mut self) -> bool {
        if let Some(scalar) = &mut self.scalar {
            if scalar.advance() {
                let memory = self.scalar.take().expect("complete scalar fold").finish();
                self.absorb(Some(memory));
            }
            return self.is_complete();
        }
        match self.step {
            Step::Descend => {
                if let Some(RewriteMemory::Fields { fields, .. }) = &mut self.current {
                    if !fields.is_empty() {
                        self.path.descend(&mut self.current, 0);
                        return false;
                    }
                }
                self.step = Step::Release;
            }
            Step::Release => {
                let scalar = match &mut self.current {
                    Some(RewriteMemory::Scalar(memory)) if memory.is_some() => Some(memory.take()),
                    Some(RewriteMemory::Scalars { items, .. }) => items.pop(),
                    _ => None,
                };
                if let Some(scalar) = scalar {
                    self.scalar = scalar.map(ScalarMemoryFolding::new);
                    return false;
                }
                let memory = self.current.take();
                self.absorb(memory.and_then(|memory| match memory {
                    RewriteMemory::Scalar(None) => None,
                    RewriteMemory::Scalar(Some(_)) => unreachable!("scalar fold still pending"),
                    RewriteMemory::Scalars { direct, items } => {
                        debug_assert!(items.is_empty());
                        drop(items);
                        Some(direct)
                    }
                    RewriteMemory::Fields { mut direct, fields } => {
                        let fields = fields.into_budgeted(&mut direct);
                        debug_assert!(
                            fields.len() <= 1 && fields.iter().all(|field| field.memory.is_none())
                        );
                        drop(fields);
                        Some(direct)
                    }
                }));
                self.step = Step::Ascend;
            }
            Step::Ascend => {
                if let Some(RewriteMemory::Fields { fields, .. }) = &mut self.path.parent {
                    if fields.len() > 1 {
                        self.current = fields
                            .pop()
                            .expect("remaining constructor field")
                            .memory
                            .take();
                        self.step = Step::Descend;
                    } else {
                        self.path.ascend(&mut self.current, 0);
                        self.step = Step::Release;
                    }
                } else {
                    debug_assert!(self.path.parent.is_none());
                }
            }
        }
        self.is_complete()
    }

    fn absorb(&mut self, memory: Option<MemoryReservation>) {
        if let Some(memory) = memory {
            if let Some(retained) = &mut self.retained {
                retained.absorb(memory);
            } else {
                self.retained = Some(memory);
            }
        }
    }

    fn is_complete(&self) -> bool {
        self.current.is_none() && self.path.parent.is_none() && self.scalar.is_none()
    }

    pub(in crate::plan::mutable) fn finish(mut self) -> Option<MemoryReservation> {
        assert!(self.is_complete());
        self.retained.take()
    }
}

impl Drop for Folding {
    fn drop(&mut self) {
        self.drain();
    }
}

#[cfg(test)]
mod tests;
