//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::CollationSelection;
use crate::catalog::collations::{builtin_collation_ctype, builtin_collation_name};

#[test]
fn one_byte_selects_each_builtin_collation_or_none() {
    assert_eq!(size_of::<CollationSelection>(), 1);
    assert_eq!(size_of::<Option<CollationSelection>>(), 2);
    assert_eq!(
        CollationSelection::from_oid(0),
        Some(CollationSelection::INDETERMINATE)
    );
    assert_eq!(CollationSelection::INDETERMINATE.oid(), 0);
    for oid in [100, 811, 950, 951, 962, 963, 6411] {
        let selection = CollationSelection::from_oid(oid).unwrap();
        assert_ne!(selection, CollationSelection::INDETERMINATE);
        assert_eq!(selection.oid(), oid);
        assert!(builtin_collation_name(i64::from(oid)).is_some());
        assert!(builtin_collation_ctype(oid).is_some());
    }
    for oid in [1, 99, 101, 16_384, u32::MAX] {
        assert_eq!(CollationSelection::from_oid(oid), None, "{oid}");
    }
}

#[test]
fn selections_serialize_as_postgresql_collation_oids() {
    for oid in [0, 100, 811, 950, 951, 962, 963, 6411] {
        let selection = CollationSelection::from_oid(oid).unwrap();
        let encoded = serde_json::to_string(&selection).unwrap();
        assert_eq!(encoded, oid.to_string());
        assert_eq!(
            serde_json::from_str::<CollationSelection>(&encoded).unwrap(),
            selection
        );
    }
    assert_eq!(
        serde_json::from_str::<CollationSelection>("16384")
            .unwrap_err()
            .to_string(),
        "collation 16384 is not a built-in collation"
    );
    assert!(serde_json::from_str::<CollationSelection>("-1").is_err());
}
