//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The text of the errors and notices that SQL statements report.

use uqa_sql::{SQLError, SQLNotice};

/// An error as the CLI prints it after `ERROR: `: its SQLSTATE and message, then its detail and hint on lines of their own.
pub(super) fn sql_error_text(error: &SQLError) -> String {
    let mut text = format!("{}: {error}", error.sqlstate().unwrap_or("XX000"));
    if let SQLError::Diagnostic { detail, hint, .. } = error {
        append_fields(&mut text, detail.as_deref(), hint.as_deref());
    }
    text
}

/// A notice as the CLI prints it: its level and message, then its detail and hint on lines of their own.
pub(super) fn sql_notice_text(notice: &SQLNotice) -> String {
    let mut text = format!("{}: {}", notice.level.as_str(), notice.message);
    append_fields(&mut text, notice.detail.as_deref(), notice.hint.as_deref());
    text
}

fn append_fields(text: &mut String, detail: Option<&str>, hint: Option<&str>) {
    if let Some(detail) = detail {
        text.push_str("\nDETAIL: ");
        text.push_str(detail);
    }
    if let Some(hint) = hint {
        text.push_str("\nHINT: ");
        text.push_str(hint);
    }
}
