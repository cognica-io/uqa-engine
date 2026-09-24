//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{read_control::StorageReadControl, StorageBackendError};

#[test]
fn occurrence_retention_prefixes_use_canonical_bytes_and_admit_long_table_names() {
    for table in ["", "docs", "left日本語"] {
        let control = StorageReadControl::with_limit(4096);
        let mut expected = legacy_prefixes(table).unwrap();
        expected.push(crate::key_value::occurrence_keys::table_prefix(table).unwrap());
        let prefixes = retained_prefixes(table, &control).unwrap();
        assert_eq!(prefixes.len(), expected.len());
        for (actual, expected) in prefixes.iter().zip(&expected) {
            assert_eq!(&**actual, expected);
        }
        assert!(control.memory().used() > 0);
        drop(prefixes);
        assert_eq!(control.memory().used(), 0);
    }
    let control = StorageReadControl::with_limit(4096);
    assert!(matches!(
        retained_prefixes(&"long_table_".repeat(8192), &control),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(control.memory().used(), 0);
}
