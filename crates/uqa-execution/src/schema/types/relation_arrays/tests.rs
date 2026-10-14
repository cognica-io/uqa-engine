//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{current_array_name, type_name_exists, RelationArrayMetadata};
use std::borrow::Cow;
use uqa_core::RelationIdentity;

fn metadata(oid: Option<u32>, name: Option<&str>) -> RelationArrayMetadata<'_> {
    RelationArrayMetadata {
        array_oid: oid,
        recorded_name: name.map(Cow::Borrowed),
    }
}

#[test]
fn array_name_projection_preserves_missing_oid_schema_and_generated_fallback() {
    let relation = RelationIdentity::new("schema", "item");
    let original = RelationIdentity::new("schema", "_item");
    assert!(type_name_exists(
        [(&relation, metadata(Some(1), None))],
        &original
    ));
    assert!(!type_name_exists(
        [(&relation, metadata(None, Some("_item")))],
        &original
    ));
    assert!(!type_name_exists(
        [(&relation, metadata(Some(1), Some("__item")))],
        &original
    ));
    assert!(!type_name_exists(
        [(&relation, metadata(Some(1), None))],
        &RelationIdentity::new("other", "_item")
    ));
    assert!(type_name_exists(
        [(&relation, metadata(Some(1), Some("__item")))],
        &RelationIdentity::new("schema", "__item")
    ));
}

#[test]
fn current_array_projection_keeps_order_exact_identity_and_absent_names() {
    assert_eq!(
        current_array_name(
            [
                metadata(Some(1), Some("first")),
                metadata(Some(1), Some("second"))
            ],
            1
        )
        .as_deref(),
        Some("first")
    );
    assert_eq!(
        current_array_name(
            [
                metadata(Some(2), Some("wrong")),
                metadata(Some(1), None),
                metadata(Some(1), Some("last"))
            ],
            1
        )
        .as_deref(),
        Some("last")
    );
    assert_eq!(current_array_name([metadata(Some(1), None)], 1), None);
    assert_eq!(
        current_array_name([metadata(None, Some("absent"))], 1),
        None
    );
}
