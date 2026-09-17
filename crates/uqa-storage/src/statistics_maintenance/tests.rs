//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn automatic_refresh_covers_first_use_threshold_and_small_idle_changes() {
    assert!(StatisticsMaintenance::default().due(true, 0, 1));
    let mut state = StatisticsMaintenance {
        changes: 1,
        dirty_since_ms: 100,
        analyzed_rows: Some(100),
        statistics_format: 1,
        ..StatisticsMaintenance::default()
    };
    assert!(!state.due(false, 101, 1));
    assert!(state.due(false, 60_100, 1));
    state.changes = 60;
    assert!(state.due(false, 101, 1));
}

#[test]
fn legacy_statistics_are_refreshed_without_waiting_for_another_write() {
    let old: StatisticsMaintenance =
        serde_json::from_str(r#"{"generation":4,"changes":0,"analyzed_rows":120}"#).unwrap();
    assert!(old.due(false, 0, 1));
}

#[test]
fn merged_changes_reject_overflow_and_incompatible_object_replacements() {
    let before = StatisticsMaintenance {
        object_id: Some([1; 16]),
        ..StatisticsMaintenance::default()
    };
    let mut after = before.clone();
    after.record_changes([1; 16], 1, Some(0), 100).unwrap();
    for field in ["changes", "generation"] {
        let mut current = before.clone();
        if field == "changes" {
            current.changes = u64::MAX;
        } else {
            current.generation = u64::MAX;
        }
        assert!(StatisticsMaintenance::merge(&before, &after, &current).is_err());
    }
    let mut replacement = before.clone();
    replacement.object_id = Some([2; 16]);
    assert!(StatisticsMaintenance::merge(&before, &after, &replacement)
        .unwrap()
        .is_none());
    assert!(StatisticsMaintenance::merge(&before, &replacement, &before)
        .unwrap()
        .is_none());
}
