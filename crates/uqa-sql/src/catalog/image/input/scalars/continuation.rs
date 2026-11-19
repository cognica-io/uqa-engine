//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One scalar owner retains original fields, scope references and unfinished parents across input windows.

use super::{CapturedScalar, Fields, Frame, References, SQLError, ScalarExpr};
use uqa_core::memory::{BudgetedVec, MemoryBudget, MemoryReservation};

enum Stage {
    Tag,
    Header(u8),
    Children,
    Complete,
}

pub(super) struct ScalarInput {
    frame: Frame,
    parents: BudgetedVec<Frame>,
    memory: MemoryReservation,
    references: References,
    stage: Stage,
}

impl ScalarInput {
    pub(super) fn new(memory: &MemoryBudget) -> Self {
        Self {
            frame: Frame {
                value: ScalarExpr::Star,
                cursor: 0,
            },
            parents: BudgetedVec::new(memory),
            memory: memory.empty_reservation(),
            references: References::default(),
            stage: Stage::Tag,
        }
    }

    pub(super) async fn advance(&mut self, input: &mut Fields<'_>) -> Result<(), SQLError> {
        match self.stage {
            Stage::Tag => {
                self.stage = Stage::Header(input.binary.array::<1>().await?[0]);
            }
            Stage::Header(tag) => {
                input
                    .native_scalar_tag(tag, &mut self.memory, &mut self.frame.value)
                    .await?;
                self.references.observe(&self.frame.value);
                self.stage = Stage::Children;
            }
            Stage::Children => {
                if self.frame.prepare_child(input).await? {
                    let parent = std::mem::replace(
                        &mut self.frame,
                        Frame {
                            value: ScalarExpr::Star,
                            cursor: 0,
                        },
                    );
                    self.parents.push(parent)?;
                    self.stage = Stage::Tag;
                } else if let Some(mut parent) = self.parents.pop() {
                    *parent.child().expect("suspended native scalar child") =
                        std::mem::replace(&mut self.frame.value, ScalarExpr::Star);
                    parent.cursor += 1;
                    self.frame = parent;
                } else {
                    self.stage = Stage::Complete;
                }
            }
            Stage::Complete => return Err(super::invalid("completed native scalar input")),
        }
        Ok(())
    }

    pub(super) fn complete(&self) -> bool {
        matches!(self.stage, Stage::Complete)
    }

    pub(super) fn finish(mut self) -> Result<CapturedScalar, SQLError> {
        if !self.complete() {
            return Err(super::invalid("incomplete native scalar input"));
        }
        Ok(CapturedScalar {
            expression: std::mem::replace(&mut self.frame.value, ScalarExpr::Star),
            memory: self.memory,
            references: self.references,
        })
    }
}
