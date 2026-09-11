//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::plan::UnifiedPlan;

fn command(sql: &str) -> Box<CommandPlan> {
    let UnifiedPlan::Command(command) = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
    else {
        panic!("expected a command");
    };
    command
}

fn message(error: SQLError) -> String {
    assert_eq!(error.sqlstate(), Some("0A000"));
    let SQLError::Routine { message, .. } = error else {
        panic!("expected SQL cursor diagnostic");
    };
    message
}

#[test]
fn hold_conflict_precedes_scroll_conflict_for_locked_queries() {
    assert_eq!(
        message(
            validate_query_options(PortalDeclarationContext::Sql, true, true, Some(true))
                .unwrap_err()
        ),
        "DECLARE CURSOR WITH HOLD ... FOR UPDATE is not supported"
    );
}

#[test]
fn scroll_diagnostics_retain_sql_and_procedural_context() {
    for (context, expected) in [
        (
            PortalDeclarationContext::Sql,
            "DECLARE SCROLL CURSOR ... FOR UPDATE is not supported",
        ),
        (
            PortalDeclarationContext::PLpgSQL,
            "DECLARE SCROLL CURSOR ... FOR UPDATE/SHARE is not supported",
        ),
    ] {
        assert_eq!(
            message(validate_query_options(context, true, false, Some(true)).unwrap_err()),
            expected
        );
        for scroll in [None, Some(false)] {
            validate_query_options(context, true, false, scroll).unwrap();
        }
        validate_query_options(context, false, true, Some(true)).unwrap();
    }
}

#[test]
fn only_explicit_scroll_modifying_commands_request_null_tuple_images() {
    for sql in [
        "INSERT INTO t VALUES (1) RETURNING *",
        "UPDATE t SET id = 2 RETURNING *",
        "DELETE FROM t RETURNING *",
    ] {
        let command = command(sql);
        assert!(command_scroll_returns_nulls(&command, Some(true)).unwrap());
        assert!(!command_scroll_returns_nulls(&command, Some(false)).unwrap());
        assert!(!command_scroll_returns_nulls(&command, None).unwrap());
    }
    assert!(!command_scroll_returns_nulls(&command("SHOW work_mem"), Some(true)).unwrap());
}

#[test]
fn merge_scroll_rejects_without_changing_command_cursor_diagnostics() {
    let command = command("MERGE INTO dst d USING src s ON d.id = s.id WHEN MATCHED THEN DELETE");
    assert_eq!(
        message(command_scroll_returns_nulls(&command, Some(true)).unwrap_err()),
        "DECLARE SCROLL CURSOR ... FOR UPDATE/SHARE is not supported"
    );
    assert!(!command_scroll_returns_nulls(&command, None).unwrap());
    let error = cannot_open_command_cursor(&command);
    assert_eq!(error.sqlstate(), Some("42P11"));
    assert!(
        matches!(error, SQLError::Routine { message, .. } if message == "cannot open MERGE query as cursor")
    );
}
