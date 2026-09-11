//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{SequenceOwner, SequenceOwnerDependency};

#[test]
fn sequence_owner_encoding_preserves_legacy_defaults_and_dependency_names() {
    let table_id = [1; 16];
    let column_id = [2; 16];
    let legacy = serde_json::json!({"table_object_id": table_id, "column_object_id": column_id});
    let owner: SequenceOwner = serde_json::from_value(legacy.clone()).unwrap();
    assert_eq!(owner.dependency, SequenceOwnerDependency::Automatic);
    assert_eq!(owner.table_object_id, [1; 16]);
    assert_eq!(owner.column_object_id, [2; 16]);
    for (dependency, name, code) in [
        (SequenceOwnerDependency::Automatic, "automatic", "a"),
        (SequenceOwnerDependency::Internal, "internal", "i"),
    ] {
        let owner = SequenceOwner {
            dependency,
            ..owner
        };
        let mut expected = legacy.clone();
        expected["dependency"] = name.into();
        assert_eq!(serde_json::to_value(owner).unwrap(), expected);
        assert_eq!(
            serde_json::from_value::<SequenceOwner>(expected).unwrap(),
            owner
        );
        assert_eq!(dependency.catalog_code(), code);
    }
}
