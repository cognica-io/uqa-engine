//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::parse_statements;

#[test]
fn listener_classification_uses_direct_parser_nodes_without_compiling_bodies() {
    let statements = parse_statements(
        "/* LISTEN ignored */ SELECT 'UNLISTEN *'; \
         LiStEn \"NOTIFY\"; UNLISTEN \"Quoted channel\"; UNLISTEN *; \
         NOTIFY events, 'LISTEN'; SELECT pg_notify('events', 'UNLISTEN'); \
         DO $$ BEGIN LISTEN nested; END $$; \
         CREATE FUNCTION deferred() RETURNS void LANGUAGE plpgsql AS $$ BEGIN UNLISTEN *; END $$",
    )
    .unwrap();
    assert_eq!(
        statements
            .iter()
            .map(crate::ParsedStatement::is_notification_listener_command)
            .collect::<Vec<_>>(),
        [false, true, true, true, false, false, false, false]
    );
}

#[test]
fn listener_classification_does_not_move_semantic_errors_before_admission() {
    let statements = parse_statements("SELECT missing_column FROM absent; LISTEN events").unwrap();
    assert!(!statements[0].is_notification_listener_command());
    assert!(statements[1].is_notification_listener_command());
}
