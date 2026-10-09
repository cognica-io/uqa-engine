//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The same SQL key support state follows rows through every external sort run.

use super::{DecoratedRow, ExecResult, Ordering, SortComparison};

pub(super) fn compare_records(
    keys: &SortComparison<'_>,
    source_width: usize,
    left: &DecoratedRow,
    right: &DecoratedRow,
) -> ExecResult<Ordering> {
    Ok(keys
        .compare_by(|index| {
            (
                left.row
                    .value(source_width + index)
                    .expect("validated external sort run key"),
                right
                    .row
                    .value(source_width + index)
                    .expect("validated external sort run key"),
            )
        })?
        .then_with(|| left.sequence.cmp(&right.sequence)))
}
