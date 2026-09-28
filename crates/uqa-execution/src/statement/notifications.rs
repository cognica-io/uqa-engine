//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Admission and execution policy for session-owned SQL notification listeners.

use uqa_core::CancellationToken;
use uqa_sql::SQLError;

/// Check the host's live session policy at the actual listener command boundary.
pub fn require_sql_listener_session(requires_subscription: bool) -> Result<(), SQLError> {
    if requires_subscription {
        Err(SQLError::NotificationRequiresSubscription)
    } else {
        Ok(())
    }
}

/// Admit every SQL message of an API batch before it can produce effects. Parsing classifies only direct commands; semantic compilation remains at each statement's original boundary.
pub fn admit_sql_batch<'sql>(
    requires_subscription: bool,
    messages: impl IntoIterator<Item = &'sql str>,
    cancellation: &CancellationToken,
) -> Result<(), SQLError> {
    if !requires_subscription {
        return Ok(());
    }
    for sql in messages {
        cancellation.check()?;
        let statements = uqa_sql::parse_statements(sql)?;
        for statement in statements {
            cancellation.check()?;
            if statement.is_notification_listener_command() {
                return Err(SQLError::NotificationRequiresSubscription);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{admit_sql_batch, require_sql_listener_session};
    use uqa_core::CancellationToken;
    use uqa_sql::SQLError;

    #[test]
    fn api_batch_classifies_commands_without_searching_embedded_sql() {
        let token = CancellationToken::new();
        admit_sql_batch(
            true,
            [
                "SELECT 'LISTEN'",
                "DO $$ BEGIN EXECUTE 'LISTEN events'; END $$",
                "NOTIFY events, 'UNLISTEN'",
            ],
            &token,
        )
        .unwrap();
        assert!(matches!(
            admit_sql_batch(true, ["SELECT nextval('counter')", "UNLISTEN *"], &token),
            Err(SQLError::NotificationRequiresSubscription)
        ));
        // Ordinary sessions retain their existing parse and execution order.
        admit_sql_batch(false, ["SELECT (", "LISTEN events"], &token).unwrap();
        require_sql_listener_session(false).unwrap();
        let error = require_sql_listener_session(true).unwrap_err();
        assert_eq!(error.sqlstate(), Some("0A000"));
        assert_eq!(error.code(), Some("NOTIFICATION_REQUIRES_SUBSCRIPTION"));
    }

    #[test]
    fn cancelled_admission_does_not_parse_the_next_message() {
        let token = CancellationToken::new();
        token.cancel();
        assert!(matches!(
            admit_sql_batch(true, ["SELECT ("], &token),
            Err(SQLError::Cancelled(_))
        ));
    }
}
