//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
#[test]
fn ddl_table_binding_preserves_absent_targets_and_wrong_kind_diagnostics() {
    assert_eq!(resolved_table_ddl_target(None, "DROP TABLE").unwrap(), None);
    assert_eq!(
        resolved_table_ddl_target(Some(("tenant.items".into(), "table")), "ALTER TABLE").unwrap(),
        Some("tenant.items".into())
    );
    assert_eq!(
        resolved_table_ddl_target(Some(("tenant.items".into(), "view")), "DROP TABLE").unwrap_err(),
        "DROP TABLE: relation `tenant.items` is a view, not a table"
    );
}
