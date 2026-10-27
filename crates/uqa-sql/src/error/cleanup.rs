//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Secondary cleanup failures without replacing a primary SQL diagnostic.

use super::SQLError;
use std::fmt::Write as _;

impl SQLError {
    /// Retain a cleanup failure alongside the original typed error. Clients
    /// without a typed cause API receive its full diagnostic in DETAIL.
    pub fn with_cleanup(self, cleanup: Self, operation: impl Into<String>) -> Self {
        let operation = operation.into();
        let (source, mut failures, mut detail) = match self {
            Self::Cleanup {
                source,
                failures,
                detail,
            } => (source, failures, detail),
            Self::Context { source, context } => {
                return Self::Context {
                    source: Box::new(source.with_cleanup(cleanup, operation)),
                    context,
                }
            }
            Self::Position { source, position } => {
                return Self::Position {
                    source: Box::new(source.with_cleanup(cleanup, operation)),
                    position,
                }
            }
            Self::InternalPosition {
                source,
                position,
                query,
            } => {
                return Self::InternalPosition {
                    source: Box::new(source.with_cleanup(cleanup, operation)),
                    position,
                    query,
                }
            }
            source => {
                let detail = source.detail().unwrap_or_default().to_owned();
                (Box::new(source), Vec::new(), detail)
            }
        };
        if !detail.is_empty() {
            detail.push('\n');
        }
        detail.push_str(&operation);
        detail.push_str(": ");
        if let Some(state) = cleanup.sqlstate() {
            write!(detail, "[{state}] ").expect("writing to a String cannot fail");
        }
        detail.push_str(&cleanup.to_string());
        for (label, value) in [
            ("DETAIL", cleanup.detail()),
            ("HINT", cleanup.hint()),
            ("CONTEXT", cleanup.context()),
            ("INTERNAL QUERY", cleanup.internal_query()),
        ] {
            if let Some(value) = value {
                write!(detail, "\n{label}: {value}").expect("writing to a String cannot fail");
            }
        }
        if let Some(position) = cleanup.position() {
            write!(detail, "\nPOSITION: {position}").expect("writing to a String cannot fail");
        }
        if let Some(position) = cleanup.internal_position() {
            write!(detail, "\nINTERNAL POSITION: {position}")
                .expect("writing to a String cannot fail");
        }
        failures.push((operation, cleanup));
        Self::Cleanup {
            source,
            failures,
            detail,
        }
    }

    /// Most recently attached cleanup failure and the operation that reported it.
    pub fn cleanup(&self) -> Option<(&str, &Self)> {
        match self {
            Self::Cleanup { failures, .. } => failures
                .last()
                .map(|(operation, cleanup)| (operation.as_str(), cleanup)),
            Self::Context { source, .. }
            | Self::Position { source, .. }
            | Self::InternalPosition { source, .. } => source.cleanup(),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diagnostic(state: &str, message: &str) -> SQLError {
        SQLError::Diagnostic {
            sqlstate: state.into(),
            message: message.into(),
            detail: Some(format!("{message} detail")),
            hint: Some(format!("{message} hint")),
        }
    }

    #[test]
    fn cleanup_preserves_typed_primary_and_secondary_diagnostics() {
        let error = diagnostic("22012", "primary")
            .with_context("inner")
            .at_position(Some(7))
            .with_cleanup(
                diagnostic("58030", "cleanup")
                    .with_context("cleanup context")
                    .at_position(Some(13)),
                "close scan",
            )
            .with_context("outer")
            .at_position(Some(19));
        assert_eq!(error.to_string(), "primary");
        assert_eq!(error.sqlstate(), Some("22012"));
        assert_eq!(error.hint(), Some("primary hint"));
        assert_eq!(error.context(), Some("inner\nouter"));
        assert_eq!(error.position(), Some(7));
        assert_eq!(error.primary().detail(), Some("primary detail"));
        assert_eq!(error.detail(), Some("primary detail\nclose scan: [58030] cleanup\nDETAIL: cleanup detail\nHINT: cleanup hint\nCONTEXT: cleanup context\nPOSITION: 13"));
        let (operation, cleanup) = error.cleanup().unwrap();
        assert_eq!(operation, "close scan");
        assert_eq!(cleanup.sqlstate(), Some("58030"));
        assert_eq!(cleanup.detail(), Some("cleanup detail"));
        assert_eq!(cleanup.hint(), Some("cleanup hint"));
        assert_eq!(cleanup.context(), Some("cleanup context"));
        assert_eq!(cleanup.position(), Some(13));

        let error = error.with_cleanup(diagnostic("58000", "second cleanup"), "close child");
        assert_eq!(error.to_string(), "primary");
        assert_eq!(error.sqlstate(), Some("22012"));
        let detail = error.detail().unwrap();
        assert!(detail.contains("close scan: [58030] cleanup"));
        assert!(detail.contains("close child: [58000] second cleanup"));
    }

    #[test]
    fn cleanup_preserves_cancellation_application_code_and_routine_position() {
        let error = SQLError::Cancelled(uqa_core::QueryCancelled::USER_REQUEST)
            .with_cleanup(diagnostic("58030", "cleanup"), "close scan");
        assert!(matches!(error.primary(), SQLError::Cancelled(_)));
        assert_eq!(error.sqlstate(), Some("57014"));
        let error = SQLError::NotificationRequiresSubscription
            .with_cleanup(diagnostic("58030", "cleanup"), "close scan");
        assert_eq!(error.code(), Some("NOTIFICATION_REQUIRES_SUBSCRIPTION"));
        let error = diagnostic("22012", "primary")
            .at_position(Some(3))
            .with_cleanup(diagnostic("58030", "cleanup"), "close scan")
            .in_routine_source("routine source", None)
            .with_context("routine frame");
        assert_eq!(error.position(), None);
        assert_eq!(error.internal_position(), Some(3));
        assert_eq!(error.internal_query(), Some("routine source"));
        assert_eq!(error.context(), Some("routine frame"));
        assert_eq!(error.cleanup().unwrap().1.sqlstate(), Some("58030"));
    }

    #[test]
    fn multiple_failed_resources_append_to_one_cleanup_collection() {
        let mut error = diagnostic("22012", "primary")
            .with_context("routine")
            .at_position(Some(4));
        for index in 0..300 {
            error = error.with_cleanup(
                diagnostic("58030", "cleanup"),
                format!("close scan {index}"),
            );
        }
        assert_eq!(error.sqlstate(), Some("22012"));
        assert_eq!(error.position(), Some(4));
        assert_eq!(error.context(), Some("routine"));
        assert_eq!(error.detail().unwrap().matches("primary detail").count(), 1);
        assert_eq!(error.detail().unwrap().matches("close scan ").count(), 300);
        let SQLError::Position { source, .. } = error else {
            panic!("position");
        };
        let SQLError::Context { source, .. } = *source else {
            panic!("context");
        };
        let SQLError::Cleanup {
            source, failures, ..
        } = *source
        else {
            panic!("cleanup");
        };
        assert!(matches!(*source, SQLError::Diagnostic { .. }));
        assert_eq!(failures.len(), 300);
    }
}
