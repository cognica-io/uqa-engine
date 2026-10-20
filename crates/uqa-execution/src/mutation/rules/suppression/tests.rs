//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

const COUNT: usize = 1_048_576;
const MAX_LIVE_BYTES: u64 = 128 * 1024;

#[test]
fn uniform_rule_suppression_keeps_constant_metadata() {
    let allocated = allocation_counter::measure(|| {
        let mut flags = RuleSuppression::new(COUNT, CancellationToken::new());
        assert!(!flags.suppresses(0).unwrap());
        assert!(!flags.suppresses(COUNT - 1).unwrap());
        flags.suppress_all().unwrap();
        assert!(flags.suppresses(0).unwrap());
        assert!(flags.suppresses(COUNT - 1).unwrap());
        assert!(!flags.suppresses(COUNT).unwrap());
    });
    assert!(allocated.bytes_max < MAX_LIVE_BYTES, "{allocated:?}");
}

#[test]
fn sparse_rule_suppression_bounds_metadata_and_preserves_union() {
    let allocated = allocation_counter::measure(|| {
        let mut flags = RuleSuppression::new(COUNT, CancellationToken::new());
        for index in (0..COUNT).step_by(257) {
            flags.suppress(index).unwrap();
        }
        for index in (0..COUNT).step_by(509) {
            flags.suppress(index).unwrap();
        }
        for index in 0..COUNT {
            assert_eq!(
                flags.suppresses(index).unwrap(),
                index % 257 == 0 || index % 509 == 0,
                "{index}"
            );
        }
    });
    assert!(allocated.bytes_max < MAX_LIVE_BYTES, "{allocated:?}");
}

#[test]
fn rule_suppression_handles_all_override_empty_bounds_and_cancellation() {
    let token = CancellationToken::new();
    let mut empty = RuleSuppression::new(0, token.clone());
    empty.suppress_all().unwrap();
    assert!(!empty.suppresses(0).unwrap());
    assert!(empty.suppress(0).is_err());
    let mut flags = RuleSuppression::new(12, token.clone());
    flags.suppress(7).unwrap();
    assert!(!flags.suppresses(3).unwrap());
    assert!(flags.suppresses(7).unwrap());
    flags.suppress_all().unwrap();
    for index in 0..12 {
        assert!(flags.suppresses(index).unwrap());
    }
    assert!(!flags.suppresses(12).unwrap());
    assert!(flags.suppress(12).is_err());
    token.cancel();
    assert_eq!(flags.suppresses(0).unwrap_err().sqlstate(), Some("57014"));
    assert_eq!(flags.suppress(0).unwrap_err().sqlstate(), Some("57014"));
    assert_eq!(flags.suppress_all().unwrap_err().sqlstate(), Some("57014"));
}
