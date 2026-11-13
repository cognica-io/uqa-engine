//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::memory::{Budgeted, MemoryBudget};
use serde::de::Error as _;

#[test]
fn typed_json_field_diagnostics_match_public_messages_and_retire_original_names() {
    let position = JsonPosition {
        byte_offset: 17,
        line: 3,
        column: 5,
    };
    for expected in [&[][..], &["a"][..], &["a", "b"][..], &["a", "b", "c"][..]] {
        for variant in [false, true] {
            let name = "private\n한🙂`".repeat(8192);
            let original = if variant {
                serde_json::Error::unknown_variant(&name, expected)
            } else {
                serde_json::Error::unknown_field(&name, expected)
            };
            let memory = MemoryBudget::new(1 << 20);
            let admitted = Budgeted::new(name.clone(), memory.reserve(name.len()).unwrap());
            let actual = if variant {
                JsonDiagnostic::unknown_variant(admitted, expected, position)
            } else {
                JsonDiagnostic::unknown_field(admitted, expected, position)
            };
            assert_eq!(actual.to_string(), format!("{original} at line 3 column 5"));
            assert_eq!(memory.used(), name.len());
            let redacted = actual.into_type_only();
            assert_eq!(memory.used(), 0);
            assert_eq!(redacted.position(), position);
            assert!(redacted.is_data());
            assert!(!redacted.to_string().contains("private"));
            assert!(!format!("{redacted:?}").contains("private"));
        }
    }
    for (actual, original) in [
        (
            JsonDiagnostic::missing_field("field", position),
            serde_json::Error::missing_field("field"),
        ),
        (
            JsonDiagnostic::duplicate_field("field", position),
            serde_json::Error::duplicate_field("field"),
        ),
        (
            JsonDiagnostic::invalid_length(2, "three fields", position),
            serde_json::Error::invalid_length(2, &"three fields"),
        ),
    ] {
        assert_eq!(actual.to_string(), format!("{original} at line 3 column 5"));
    }
}
