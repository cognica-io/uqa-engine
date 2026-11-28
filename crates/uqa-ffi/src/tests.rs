//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The single native-library test harness.

#[path = "tests/allocations.rs"]
mod allocations;

#[cfg(feature = "test-support")]
#[path = "tests/panics.rs"]
mod panics;

#[path = "tests/array_values.rs"]
mod array_values;

#[path = "tests/config.rs"]
mod config;

#[path = "tests/copy.rs"]
mod copy;

#[path = "tests/cursors.rs"]
mod cursors;

#[path = "tests/dense_values.rs"]
mod dense_values;

#[path = "tests/headers.rs"]
mod headers;

#[path = "tests/notifications.rs"]
mod notifications;

#[path = "tests/sessions.rs"]
mod sessions;

#[path = "tests/statements.rs"]
mod statements;

#[path = "tests/values.rs"]
mod values;

#[path = "tests/xml_values.rs"]
mod xml_values;

#[path = "tests/record_values.rs"]
mod record_values;

#[path = "tests/range_values.rs"]
mod range_values;

#[cfg(feature = "test-support")]
#[path = "tests/database.rs"]
mod database;

#[cfg(feature = "test-support")]
#[path = "tests/completion.rs"]
mod completion;

/// Keep copies alive until the shared limit rejects one, checking the charge on every successful copy.
#[allow(unsafe_code)]
unsafe fn fill_allowance(value: *const crate::UQAValue) -> Vec<*mut crate::UQAValue> {
    let budget = unsafe { &*value }
        .control()
        .budget()
        .expect("session-backed value");
    let mut copies = Vec::new();
    loop {
        let before = budget.used();
        let mut copy = std::ptr::null_mut();
        let status = unsafe { crate::uqa_value_create_copy(value, &raw mut copy) };
        if status == crate::UQA_ERROR_LIMIT {
            assert!(copy.is_null());
            assert_eq!(budget.used(), before);
            return copies;
        }
        assert_eq!(status, crate::UQA_OK);
        assert!(budget.used() - before >= size_of::<crate::UQAValue>());
        copies.push(copy);
    }
}
