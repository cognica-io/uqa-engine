//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Source layouts consume original observations in preorder and assemble their owned topology in postorder.

use super::{AnalysisIdentity, QuerySchemas};
use crate::{
    catalog::work::{inline, Work},
    construction::Stack,
    plan::{AnalyzedSource, SourcePlan},
    RowSchema, SQLError,
};
use uqa_core::memory::{Produced, ProductionControl, TreeRetirement};

enum Frame<'a> {
    Source(&'a SourcePlan),
    Left {
        schema: RowSchema,
        right: &'a SourcePlan,
    },
    Right {
        schema: RowSchema,
        left: Produced<AnalyzedSource>,
    },
}

impl QuerySchemas {
    pub(super) fn take_source_with_control(
        &mut self,
        source: &SourcePlan,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<AnalyzedSource>>, SQLError> {
        inline(self.take_source_from(source, control, &mut ()))
    }

    pub(super) async fn take_source_from<W: Work + Send>(
        &mut self,
        source: &SourcePlan,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<Option<Produced<AnalyzedSource>>, SQLError> {
        let mut frames = Stack::<_, 4>::new(*control);
        frames.push(Frame::Source(source))?;
        let mut output = None;
        while let Some(frame) = frames.pop() {
            control.check()?;
            work.checkpoint().await?;
            control.check()?;
            match frame {
                Frame::Source(source) => {
                    let Some(schema) = self.sources.remove(&AnalysisIdentity::from(source)) else {
                        output = None;
                        continue;
                    };
                    if let SourcePlan::Join { left, right, .. } = source {
                        frames.push(Frame::Left { schema, right })?;
                        frames.push(Frame::Source(left))?;
                    } else {
                        output = Some(control.finish(
                            AnalyzedSource {
                                schema,
                                children: None,
                            },
                            control.empty_reservation(),
                        )?);
                    }
                }
                Frame::Left { schema, right } => {
                    if let Some(left) = output.take() {
                        frames.push(Frame::Right { schema, left })?;
                        frames.push(Frame::Source(right))?;
                    }
                }
                Frame::Right { schema, left } => {
                    if let Some(right) = output.take() {
                        let header =
                            control.reserve(size_of::<(AnalyzedSource, AnalyzedSource)>())?;
                        let (left, left_memory) = left.into_parts();
                        let (right, right_memory) = right.into_parts();
                        let memory =
                            control.combine(control.combine(left_memory, right_memory), header);
                        output = Some(control.finish(
                            AnalyzedSource {
                                schema,
                                children: Some(Box::new((left, right))),
                            },
                            memory,
                        )?);
                    } else {
                        // A missing right descriptor discards the already assembled left topology through the same retirement owner used on abandonment.
                        retire(left, control, work).await?;
                    }
                }
            }
        }
        control.check()?;
        Ok(output)
    }
}

async fn retire<W: Work>(
    value: Produced<AnalyzedSource>,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<(), SQLError> {
    let (mut source, _memory) = value.into_parts();
    let mut children = TreeRetirement::take(&mut source.children);
    loop {
        control.check()?;
        work.checkpoint().await?;
        control.check()?;
        if children.advance() {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests;
