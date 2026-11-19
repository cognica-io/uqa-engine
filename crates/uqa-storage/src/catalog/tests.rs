//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::RelationIdentity;

#[test]
fn relation_identity_rendering_is_reversible_and_collision_free() {
    let left = RelationIdentity::new("a.b", "c");
    let right = RelationIdentity::new("a", "b.c");
    assert_eq!(left.qualified_name(), "\"a.b\".c");
    assert_eq!(right.qualified_name(), "a.\"b.c\"");
    assert_ne!(left.qualified_name(), right.qualified_name());
    assert_eq!(
        RelationIdentity::from_legacy_name(&left.qualified_name()).unwrap(),
        left
    );
    assert_eq!(
        RelationIdentity::from_legacy_name(&right.qualified_name()).unwrap(),
        right
    );
}

#[test]
fn relation_identity_preserves_quotes_and_unqualified_public_alias() {
    let quoted = RelationIdentity::new("public", "a\"b.c");
    assert_eq!(quoted.qualified_name(), "public.\"a\"\"b.c\"");
    assert_eq!(
        quoted.canonical_and_legacy_public_names(),
        vec![
            "public.\"a\"\"b.c\"".to_string(),
            "\"a\"\"b.c\"".to_string()
        ]
    );
    assert_eq!(
        RelationIdentity::from_legacy_name(&quoted.qualified_name()).unwrap(),
        quoted
    );
    assert_eq!(
        RelationIdentity::from_legacy_name("plain").unwrap(),
        RelationIdentity::new("public", "plain")
    );
    assert_eq!(
        RelationIdentity::new("app", "plain").canonical_and_legacy_public_names(),
        vec!["app.plain".to_string()]
    );
    assert_eq!(
        RelationIdentity::new("public", "Upper").canonical_and_legacy_public_names(),
        vec![
            "public.\"Upper\"".to_string(),
            "\"Upper\"".to_string(),
            "Upper".to_string()
        ]
    );
}
