//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Analyzed source replacement retains the source generation while its original topology and row fields retire.

use super::{AnalyzedSource, Arc, SharedAnalyzedSource, Storage, TreeRetirement};
use uqa_core::memory::MemoryReservation;

enum Active {
    Children(TreeRetirement<Option<Box<(AnalyzedSource, AnalyzedSource)>>>),
    Schema(crate::schema::retirement::Retirement),
    Empty,
}

pub(crate) struct Retirement {
    active: Active,
    root: Option<AnalyzedSource>,
    memory: Option<MemoryReservation>,
}
impl Retirement {
    fn new(mut root: AnalyzedSource, memory: Option<MemoryReservation>) -> Self {
        Self {
            active: Active::Children(TreeRetirement::take(&mut root.children)),
            root: Some(root),
            memory,
        }
    }

    pub(crate) fn advance(&mut self) -> bool {
        match &mut self.active {
            Active::Children(children) => {
                if children.advance() {
                    self.active = Active::Schema(
                        self.root
                            .as_mut()
                            .expect("owned source root")
                            .schema
                            .take_retirement(),
                    );
                }
                false
            }
            Active::Schema(schema) => {
                if schema.advance() {
                    self.active = Active::Empty;
                    self.root = None;
                    self.memory = None;
                    true
                } else {
                    false
                }
            }
            Active::Empty => true,
        }
    }

    async fn retire_from<W: crate::catalog::work::Work>(
        &mut self,
        control: &uqa_core::memory::ProductionControl<'_>,
        work: &mut W,
    ) -> Result<(), crate::SQLError> {
        loop {
            control.check()?;
            work.checkpoint().await?;
            control.check()?;
            if self.advance() {
                return Ok(());
            }
        }
    }
}
impl SharedAnalyzedSource {
    pub(crate) fn take_retirement(&mut self) -> Retirement {
        let value = self.0.take().and_then(|value| match value {
            Storage::Ordinary(value) => Arc::into_inner(value).map(|value| (value, None)),
            Storage::Admitted(value) => Arc::into_inner(value).map(|value| value.into_parts()),
        });
        match value {
            Some((value, memory)) => Retirement::new(value, memory),
            None => Retirement {
                active: Active::Empty,
                root: None,
                memory: None,
            },
        }
    }
}

impl AnalyzedSource {
    pub(crate) async fn retire_produced_from<W: crate::catalog::work::Work>(
        value: uqa_core::memory::Produced<Self>,
        control: &uqa_core::memory::ProductionControl<'_>,
        work: &mut W,
    ) -> Result<(), crate::SQLError> {
        let (root, memory) = value.into_parts();
        Retirement::new(root, memory)
            .retire_from(control, work)
            .await
    }
}
impl Drop for SharedAnalyzedSource {
    fn drop(&mut self) {
        drop(self.take_retirement());
    }
}
