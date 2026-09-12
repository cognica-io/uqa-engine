//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn schema_change_rewrite_rejects_an_unknown_table() {
    let error = Engine::new()
        .rewrite_document_for_schema_change("missing", 1, Document::new())
        .unwrap_err();
    assert!(matches!(error, SQLError::UnknownTable(_)), "{error}");
}
