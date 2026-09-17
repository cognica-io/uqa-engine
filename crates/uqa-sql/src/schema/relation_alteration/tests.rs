//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn materialized_storage_options_use_weaker_locks_than_visible_definition_changes() {
    for (sql, mode) in [
        (
            "ALTER MATERIALIZED VIEW m SET (fillfactor=80)",
            TableLockMode::ShareUpdateExclusive,
        ),
        (
            "ALTER MATERIALIZED VIEW m RESET (fillfactor)",
            TableLockMode::ShareUpdateExclusive,
        ),
        (
            "ALTER MATERIALIZED VIEW m OWNER TO other",
            TableLockMode::AccessExclusive,
        ),
        (
            "ALTER MATERIALIZED VIEW m RENAME TO other",
            TableLockMode::AccessExclusive,
        ),
        (
            "ALTER VIEW v SET (security_barrier=true)",
            TableLockMode::AccessExclusive,
        ),
        (
            "ALTER VIEW v RESET (security_invoker)",
            TableLockMode::AccessExclusive,
        ),
        (
            "ALTER VIEW v OWNER TO other",
            TableLockMode::AccessExclusive,
        ),
        (
            "ALTER VIEW v RENAME TO other",
            TableLockMode::AccessExclusive,
        ),
    ] {
        let crate::Statement::AlterView(statement) = crate::compile(sql).unwrap().remove(0) else {
            panic!("expected ALTER VIEW: {sql}");
        };
        assert_eq!(view_alter_lock_mode(&statement), mode, "{sql}");
    }
}
