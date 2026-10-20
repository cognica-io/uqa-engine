//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compact metadata for sealed runs; their shared arena owns the authenticated byte file.

use super::{spill_error, ArenaSegment, RowSchema, SpillBuffer, SpillFileArena};
use crate::ExecResult;

#[derive(Clone, Copy)]
pub(crate) struct SpillSegmentDescriptor(pub [u64; 6]);

impl SpillBuffer {
    pub(crate) fn into_segment_descriptor(mut self) -> ExecResult<SpillSegmentDescriptor> {
        self.spill_pending()?;
        let range = self
            .spill_range()
            .ok_or_else(|| spill_error("sort run has no sealed arena range"))?;
        let to_word =
            |n: usize| u64::try_from(n).map_err(|_| spill_error("sort run count exceeds u64"));
        Ok(SpillSegmentDescriptor([
            range.start,
            range.end,
            to_word(self.rows)?,
            to_word(self.spilled_batches)?,
            to_word(self.spilled_bytes)?,
            to_word(self.max_spilled_record_bytes)?,
        ]))
    }

    pub(crate) fn from_segment_descriptor(
        arena: &SpillFileArena,
        schema: &RowSchema,
        descriptor: SpillSegmentDescriptor,
    ) -> ExecResult<Self> {
        let [start, end, rows, batches, bytes, maximum] = descriptor.0;
        let file = arena.reopen()?;
        if start >= end
            || end
                > file
                    .metadata()
                    .map_err(|error| spill_error(error.to_string()))?
                    .len()
        {
            return Err(spill_error("sort run has an invalid sealed range"));
        }
        let count = |n| usize::try_from(n).map_err(|_| spill_error("sort run count exceeds usize"));
        let mut buffer = Self::new(0);
        buffer.schema = Some(schema.clone());
        buffer.rows = count(rows)?;
        buffer.spilled_rows = buffer.rows;
        buffer.spilled_batches = count(batches)?;
        buffer.spilled_bytes = count(bytes)?;
        buffer.max_spilled_record_bytes = count(maximum)?;
        buffer.spill_file = Some(file);
        buffer.spill_segment = Some(Box::new(ArenaSegment {
            arena: arena.clone(),
            range: Some(start..end),
        }));
        Ok(buffer)
    }
}
