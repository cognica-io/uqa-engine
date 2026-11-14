//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::stored_input::test_control;

#[test]
fn stored_index_columns_keep_resource_failures_terminal_inside_untagged_choices() {
    let mut expression = query(QUERIES[9]);
    let alternative = &mut expression["ScalarSubquery"]["with"][0]["update"]["assignments"][0][0];
    assert!(alternative.get("targets").is_some());
    alternative["column"] = serde_json::json!("first");
    alternative["indirection"] = serde_json::json!([{"Index":{"Literal":"x".repeat(1 << 20)}}]);
    let text: Arc<str> = keys(expression).into();
    assert!(serde_json::from_str::<Vec<IndexKey>>(&text).is_ok());
    for cancelled in [false, true] {
        let memory = MemoryBudget::new(if cancelled { 8 << 20 } else { 64 << 10 });
        let token = CancellationToken::new();
        let mut read = input(text.clone(), &memory, &token);
        let error = if cancelled {
            test_control::cancel_after_allocation(&memory, &token, 128 << 10, || {
                finish(&mut read, 1).unwrap_err()
            })
        } else {
            finish(&mut read, 1).unwrap_err()
        };
        assert!(matches!(
            (cancelled, error),
            (true, JsonInputError::Cancelled(_)) | (false, JsonInputError::Memory(_))
        ));
        assert_eq!(memory.used(), 0);
        assert!(read
            .advance(
                &mut std::io::empty(),
                ImageDecodeWindow { bytes: 1, steps: 1 }
            )
            .is_err());
        drop(read);
        assert_eq!(memory.used(), 0);
    }
}
