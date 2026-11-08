//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bound SQL routine bodies preserve queries and mutation commands through their native codecs.

use super::{
    variant, DefinitionImageReader, Frame, ImageFrame, Node, Part, SQLError, UNIFIED_MAGIC,
};
use crate::plan::UnifiedPlan;
use uqa_core::{memory::MemoryBudget, CancellationToken};

impl Node for UnifiedPlan {
    fn part(&self, position: usize) -> Result<Option<Part<'_>>, SQLError> {
        Ok(match self {
            Self::Query(query) => variant(position, 0, &[query]),
            Self::Command(command) => variant(position, 1, &[command]),
        })
    }
}

impl UnifiedPlan {
    /// Stream a bound SQL body member without changing its query or mutation representation. Utility commands are outside the stored bound-body carrier and fail during traversal.
    pub fn image_reader<'a>(
        &'a self,
        memory: &'a MemoryBudget,
        cancellation: &'a CancellationToken,
    ) -> Result<DefinitionImageReader<'a>, SQLError> {
        DefinitionImageReader::start(
            ImageFrame::Plan(Frame::Node(self, 0)),
            UNIFIED_MAGIC,
            1,
            memory,
            cancellation,
        )
    }
}
