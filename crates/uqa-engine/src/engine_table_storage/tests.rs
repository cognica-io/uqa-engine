use super::*;

#[test]
fn legacy_relation_references_match_canonical_targets_and_corruption_fails_closed() {
    let public_parent = RelationIdentity::new("public", "parent");
    let app_parent = RelationIdentity::new("app", "parent");

    assert!(stored_relation_reference_matches("parent", &public_parent));
    assert!(stored_relation_reference_matches("parent", &app_parent));
    assert!(stored_relation_reference_matches(
        "public.parent",
        &public_parent
    ));
    assert!(!stored_relation_reference_matches(
        "public.parent",
        &app_parent
    ));
    assert!(stored_relation_reference_matches(
        "corrupt.reference.extra",
        &public_parent
    ));
}

#[test]
fn schema_change_rewrite_rejects_an_unknown_table() {
    let error = Engine::new()
        .rewrite_document_for_schema_change("missing", 1, Document::new())
        .unwrap_err();
    assert!(matches!(error, SQLError::UnknownTable(_)), "{error}");
}
