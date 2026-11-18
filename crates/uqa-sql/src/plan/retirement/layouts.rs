//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Layout replacement and native tree retirement share the original descriptor owners and field order.

use crate::{
    catalog::work::Work,
    plan::{analyzed, QueryPlan, SharedAnalyzedSource},
    RowSchema, SQLError,
};
use uqa_core::memory::ProductionControl;

enum Active {
    Empty,
    Output(crate::schema::retirement::Retirement),
    Source(analyzed::retirement::Retirement),
}

pub(in crate::plan) struct Layouts {
    active: Active,
    output: Option<RowSchema>,
    source: Option<SharedAnalyzedSource>,
}

impl Layouts {
    pub(in crate::plan) fn replace(
        query: &mut QueryPlan,
        output: Option<RowSchema>,
        source: Option<SharedAnalyzedSource>,
    ) -> Self {
        Self {
            active: Active::Empty,
            output: std::mem::replace(&mut query.analyzed_output, output),
            source: std::mem::replace(&mut query.analyzed_source, source),
        }
    }

    pub(in crate::plan) fn advance(&mut self) -> bool {
        match &mut self.active {
            Active::Output(output) => {
                if output.advance() {
                    self.active = Active::Empty;
                }
            }
            Active::Source(source) => {
                if source.advance() {
                    self.active = Active::Empty;
                }
            }
            Active::Empty => {
                if let Some(mut output) = self.output.take() {
                    self.active = Active::Output(output.take_retirement());
                } else if let Some(mut source) = self.source.take() {
                    self.active = Active::Source(source.take_retirement());
                } else {
                    return true;
                }
            }
        }
        false
    }

    pub(in crate::plan) async fn retire_from<W: Work>(
        &mut self,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<(), SQLError> {
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

impl Drop for Layouts {
    fn drop(&mut self) {
        while !self.advance() {}
    }
}
