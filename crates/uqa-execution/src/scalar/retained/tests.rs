//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn constants_share_bounded_spill_across_plan_and_worker_lifetimes() {
    let values = RetainedConstants::new(4096);
    let measure = allocation_counter::measure(|| {
        let mut handles = Vec::new();
        for n in 0..128 {
            handles.push(
                values
                    .retain(Value::Str(format!("{n}:{}", "x".repeat(131_072))))
                    .unwrap(),
            );
        }
        assert!(values.source.0.lock().spilled_bytes() > 16 * 1024 * 1024);
        let expected = Value::Str(format!("127:{}", "x".repeat(131_072)));
        let shared = handles.last().unwrap().clone();
        let weak = Arc::downgrade(&values.source);
        drop(values);
        drop(handles);
        assert!(weak.upgrade().is_some());
        assert_eq!(
            std::thread::spawn(move || shared.read().unwrap())
                .join()
                .unwrap(),
            expected
        );
        assert!(weak.upgrade().is_none());
    });
    assert!(
        measure.bytes_max < 2 * 1024 * 1024,
        "{} retained bytes",
        measure.bytes_max
    );
}
