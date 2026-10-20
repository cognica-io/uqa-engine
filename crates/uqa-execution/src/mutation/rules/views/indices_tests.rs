//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::CancellationToken;
use uqa_sql::ast::RuleEvent;

const COUNT: usize = 262_144;
const MAX_LIVE_BYTES: u64 = 512 * 1024;

fn verify_selection(sparse: bool) {
    let mut batches =
        PreparedViewRuleBatches::empty(COUNT, RuleEvent::Update, CancellationToken::new());
    if sparse {
        for index in (0..COUNT).step_by(2) {
            batches.suppress_original.suppress(index).unwrap();
        }
    }
    let allocated = allocation_counter::measure(|| {
        let indices = batches
            .unsuppressed_indices(COUNT, 16 * 1024, CancellationToken::new())
            .unwrap();
        let expected_count = if sparse { COUNT / 2 } else { COUNT };
        assert_eq!(indices.len(), expected_count);
        for (local, global) in indices.reader().unwrap().map(Result::unwrap).enumerate() {
            assert_eq!(global, if sparse { local * 2 + 1 } else { local });
        }
    });
    assert!(allocated.bytes_max < MAX_LIVE_BYTES, "{allocated:?}");
}

#[test]
fn unsuppressed_rule_ordinals_bound_full_selection_memory() {
    verify_selection(false);
}

#[test]
fn unsuppressed_rule_ordinals_bound_fragmented_selection_memory() {
    verify_selection(true);
}
