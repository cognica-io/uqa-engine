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
fn a_session_keeps_only_the_changes_that_decide_nothing() {
    // Clean statistics: the first change marks them stale and starts their age.
    let clean = StatisticsMaintenance {
        analyzed_rows: Some(1_000),
        statistics_format: 1,
        ..StatisticsMaintenance::default()
    };
    assert!(!clean.defers(1, 100));
    let mut state = clean.clone();
    state.record_changes([1; 16], 1, Some(1_000), 100).unwrap();
    // An analysis of 1,000 rows is due after 150 changes, and a session keeps fewer than a quarter of that.
    assert!(state.defers(1, 200));
    assert!(state.defers(36, 200));
    assert!(!state.defers(37, 200));
    // The changes that make the analysis due are recorded, however few they are.
    state.changes = 140;
    assert!(state.defers(9, 200));
    assert!(!state.defers(10, 200));
    // An analysis that is due already decides nothing more, by count or by age.
    state.changes = 150;
    assert!(state.defers(1_000_000, 200));
    state.changes = 1;
    assert!(state.defers(1_000_000, 60_100));
    assert!(!state.defers(37, 60_099));

    // A small table keeps sixteen changes, an empty one or one without statistics awaits its analysis.
    let mut small = StatisticsMaintenance {
        changes: 1,
        dirty_since_ms: 100,
        analyzed_rows: Some(10),
        statistics_format: 1,
        ..StatisticsMaintenance::default()
    };
    assert!(small.defers(15, 200));
    assert!(!small.defers(16, 200));
    small.analyzed_rows = Some(0);
    assert!(small.defers(1_000, 200));
    small.analyzed_rows = None;
    assert!(small.defers(1_000, 200));
}

#[test]
fn an_analysis_covers_the_commits_up_to_its_sample() {
    let mut state = StatisticsMaintenance::default();
    assert!(!state.covers(Some(1)));
    state.analyzed([1; 16], 10, 1, Some(40)).unwrap();
    assert!(state.covers(Some(40)));
    assert!(state.covers(Some(7)));
    assert!(!state.covers(Some(41)));
    // A session that does not know where its commits lie, or an analysis that does not say where it sampled, covers nothing.
    assert!(!state.covers(None));
    let json = serde_json::to_string(&state).unwrap();
    assert!(json.contains(r#""analyzed_at":40"#));
    // Later changes leave the sample where it was.
    state.record_changes([1; 16], 3, Some(10), 100).unwrap();
    assert!(state.covers(Some(40)));
    state.analyzed([1; 16], 13, 1, None).unwrap();
    assert!(!state.covers(Some(40)));
    // A record without a sample is written as it always was, and one written by an earlier version reads as one.
    assert!(!serde_json::to_string(&state)
        .unwrap()
        .contains("analyzed_at"));
    let old: StatisticsMaintenance =
        serde_json::from_str(r#"{"generation":4,"changes":0,"analyzed_rows":120}"#).unwrap();
    assert!(!old.covers(Some(0)));

    // A merge keeps the sample of the transaction that analyzed, and otherwise the one already recorded.
    let before = StatisticsMaintenance {
        object_id: Some([1; 16]),
        analyzed_rows: Some(10),
        statistics_format: 1,
        ..StatisticsMaintenance::default()
    };
    let mut analysis = before.clone();
    analysis.analyzed([1; 16], 12, 1, Some(9)).unwrap();
    let mut written = before.clone();
    written.record_changes([1; 16], 2, Some(10), 100).unwrap();
    let merged = StatisticsMaintenance::merge(&before, &analysis, &written)
        .unwrap()
        .unwrap();
    assert!(merged.covers(Some(9)));
    let merged = StatisticsMaintenance::merge(&before, &written, &analysis)
        .unwrap()
        .unwrap();
    assert!(merged.covers(Some(9)));
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
