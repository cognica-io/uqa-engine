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
    append_fields(&mut text, error.detail(), error.hint());
    if let Some(context) = error.context() {
        text.push_str("\nCONTEXT: ");
        text.push_str(context);
    }
    crate::printing::message_text(&text)
}

/// A notice as the CLI prints it: its level and message, then its detail and hint on lines of their own.
pub(super) fn sql_notice_text(notice: &SQLNotice) -> String {
    let mut text = format!("{}: {}", notice.level.as_str(), notice.message);
    append_fields(&mut text, notice.detail.as_deref(), notice.hint.as_deref());
    crate::printing::message_text(&text)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_parser_diagnostic_fields() {
        let mut error = uqa_sql::parse_statements("SELECT )").unwrap_err();
        let SQLError::ParseDiagnostic(diagnostic) = &mut error else {
            panic!("expected original parser diagnostic");
        };
        diagnostic.detail = Some("parser detail".into());
        diagnostic.hint = Some("parser hint".into());
        let error = error
            .with_context("SQL function \"inner\" statement 1")
            .with_context("PL/pgSQL function outer(integer) line 3 at RETURN");
        let text = sql_error_text(&error);
        assert!(text.starts_with("42601: "));
        assert!(text.ends_with("\nDETAIL: parser detail\nHINT: parser hint\nCONTEXT: SQL function \"inner\" statement 1\nPL/pgSQL function outer(integer) line 3 at RETURN"));
    }
}

#[cfg(test)]
mod terminal_tests {
    #[test]
    fn diagnostic_controls_cannot_become_terminal_commands() {
        let notice = uqa_sql::SQLNotice::notice("value \x1b[31mred");
        let text = super::sql_notice_text(&notice);
        assert!(!text.contains('\x1b'));
        assert!(text.contains("red"));
        let error = uqa_sql::SQLError::Routine {
            sqlstate: "22012".into(),
            message: "division by zero".into(),
        }
        .with_context("SQL function \"\x1b[31minner\" statement 1");
        assert!(!super::sql_error_text(&error).contains('\x1b'));
    }
}
