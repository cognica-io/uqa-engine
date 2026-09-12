//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Java simple lowercase reads scalar pairs but preserves unpaired UTF-16 units.

use super::Work;
use crate::nori::error::invalid;
use crate::nori::NoriDictionary;
use crate::AnalysisResult;

pub(super) fn apply(
    input: &mut [u16],
    model: &NoriDictionary,
    work: &mut Work<'_>,
) -> AnalysisResult<()> {
    let mut index = 0;
    while index < input.len() {
        work.tick()?;
        let first = input[index];
        let (point, width) = if (0xd800..=0xdbff).contains(&first)
            && input
                .get(index + 1)
                .is_some_and(|unit| (0xdc00..=0xdfff).contains(unit))
        {
            (
                0x10000 + ((u32::from(first) - 0xd800) << 10) + u32::from(input[index + 1])
                    - 0xdc00,
                2,
            )
        } else {
            (u32::from(first), 1)
        };
        let lower = model
            .unicode(point)
            .expect("complete Unicode profile")
            .lowercase;
        // Lucene writes back in place. The pinned Java simple mappings retain UTF-16 width.
        if usize::from(lower >= 0x10000) + 1 != width {
            return Err(invalid("Nori lowercase", "simple mapping changes UTF-16 width").into());
        }
        if width == 1 {
            input[index] = lower as u16;
        } else {
            let scalar = lower - 0x10000;
            input[index] = 0xd800 + (scalar >> 10) as u16;
            input[index + 1] = 0xdc00 + (scalar & 0x3ff) as u16;
        }
        index += width;
    }
    Ok(())
}
