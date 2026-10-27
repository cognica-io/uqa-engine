//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn captured_properties_retain_nested_buffers_until_their_last_owner_drops() {
    let budget = MemoryBudget::new(16 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut child = ExplainObject::new(control);
    child.text("Relation Name", "owned relation").unwrap();
    child.number("Rows", u64::MAX).unwrap();
    child.optional_number("Unknown Population", None).unwrap();
    let child = child.finish().unwrap();
    let child_bytes = budget.used();
    let mut parent = ExplainObject::new(control);
    parent.insert("Child", child).unwrap();
    let parent = parent.finish().unwrap();
    assert!(budget.used() > child_bytes);
    let ExplainValue::Object(fields) = &parent else {
        panic!("object required")
    };
    let ExplainValue::Object(child) = &fields[0].1 else {
        panic!("child required")
    };
    assert!(matches!(child[1].1, ExplainValue::Unsigned(u64::MAX)));
    assert!(matches!(child[2].1, ExplainValue::Null));
    drop(parent);
    assert_eq!(budget.used(), 0);
}

#[test]
fn captured_text_rejects_allowance_before_copying_and_preserves_earlier_fields() {
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let text = "x".repeat(1024 * 1024);
    let mut object = ExplainObject::new(control);
    object.text("Alias", "earlier").unwrap();
    let before = budget.used();
    let allocations = allocation_counter::measure(|| {
        assert_eq!(
            object.text("Relation Name", &text).unwrap_err().sqlstate(),
            Some("53200")
        );
    });
    assert!(allocations.bytes_max < 4096, "{allocations:?}");
    assert_eq!(budget.used(), before);
    let output = object.finish().unwrap();
    let ExplainValue::Object(fields) = &output else {
        panic!("object required")
    };
    assert_eq!(fields.len(), 1);
    let ExplainValue::Text(text) = &fields[0].1 else {
        panic!("text required")
    };
    assert_eq!(text.as_str(), "earlier");
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn each_cancellation_owner_stops_capture_and_releases_completed_properties() {
    for cancel_original in [false, true] {
        let budget = MemoryBudget::new(4096);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let mut object = ExplainObject::new(control);
        object.text("Alias", "retained").unwrap();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert_eq!(object.finish().unwrap_err().sqlstate(), Some("57014"));
        assert_eq!(budget.used(), 0);
    }
}
