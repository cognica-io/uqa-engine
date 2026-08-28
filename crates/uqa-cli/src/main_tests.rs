//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::completion::highlight_sql_line;
use super::*;
use rustyline::completion::Candidate as _;
use rustyline::history::MemHistory;

#[test]
fn completion_reads_uqa_function_registry() {
    let helper = UsqlHelper::new(Vec::new(), Vec::new(), Vec::new());
    let history = MemHistory::new();
    let ctx = Context::new(&history);
    let (_start, candidates) = helper.complete("SELECT dee", 10, &ctx).unwrap();
    let replacements = candidates
        .iter()
        .map(rustyline::completion::Candidate::replacement)
        .collect::<Vec<_>>();
    assert!(replacements.contains(&"deep_predict"));
    assert!(replacements.contains(&"deep_learn"));
}

#[test]
fn completion_uses_live_schema_names() {
    let helper = UsqlHelper::new(
        vec!["users".into()],
        vec!["events_ext".into()],
        vec!["user_id".into()],
    );
    let history = MemHistory::new();
    let ctx = Context::new(&history);
    let (_start, from_candidates) = helper.complete("SELECT * FROM us", 16, &ctx).unwrap();
    assert!(from_candidates
        .iter()
        .any(|candidate| candidate.replacement() == "users"));

    let (_start, empty_from_candidates) = helper
        .complete("SELECT * FROM ", "SELECT * FROM ".len(), &ctx)
        .unwrap();
    assert!(empty_from_candidates
        .iter()
        .any(|candidate| candidate.replacement() == "users"));

    let (_start, column_candidates) = helper.complete("SELECT user", 11, &ctx).unwrap();
    assert!(column_candidates
        .iter()
        .any(|candidate| candidate.replacement() == "user_id"));
}

#[test]
fn completion_uses_live_schema_names_for_backslash_table_args() {
    let helper = UsqlHelper::new(
        vec!["users".into()],
        vec!["events_ext".into()],
        vec!["user_id".into()],
    );
    let history = MemHistory::new();
    let ctx = Context::new(&history);

    let (_start, stats_candidates) = helper.complete("\\stats ", "\\stats ".len(), &ctx).unwrap();
    assert!(stats_candidates
        .iter()
        .any(|candidate| candidate.replacement() == "users"));
    assert!(!stats_candidates
        .iter()
        .any(|candidate| candidate.replacement() == "events_ext"));

    let (_start, describe_candidates) = helper.complete("\\d ev", "\\d ev".len(), &ctx).unwrap();
    assert!(describe_candidates
        .iter()
        .any(|candidate| candidate.replacement() == "events_ext"));
}

#[test]
fn split_statements_respects_dollar_quoting() {
    let text = "CREATE FUNCTION f() RETURNS int AS $$\nBEGIN\n  RETURN 1;\nEND;\n$$ LANGUAGE plpgsql;\nSELECT f();";
    let parts = split_statements(text);
    assert_eq!(parts.len(), 2, "{parts:?}");
    assert!(parts[0].contains("RETURN 1;"));
    assert_eq!(parts[1], "SELECT f()");
}

#[test]
fn split_statements_respects_tagged_dollar_quoting_and_params() {
    let text = "DO $body$ BEGIN PERFORM 1; END; $body$; SELECT $1; SELECT 2;";
    let parts = split_statements(text);
    assert_eq!(parts.len(), 3, "{parts:?}");
    assert!(parts[0].starts_with("DO $body$"));
    assert_eq!(parts[1], "SELECT $1");
}

#[test]
fn split_statements_respects_comments_and_identifiers() {
    let text = "SELECT 1 -- trailing; comment\n; SELECT /* block ; comment */ \"odd;name\", 'a;b'; SELECT 3";
    let parts = split_statements(text);
    assert_eq!(parts.len(), 3, "{parts:?}");
    assert!(parts[1].contains("odd;name"));
    assert!(parts[1].contains("'a;b'"));
}

#[test]
fn split_statements_respects_postgresql_escape_and_delimited_quotes() {
    let text = r#"SELECT E'escaped\';semicolon', """odd;identifier"""; SELECT U&'d\0061t;a';"#;
    let parts = split_statements(text);
    assert_eq!(parts.len(), 2, "{parts:?}");
    assert!(parts[0].contains("escaped\\';semicolon"));
    assert!(parts[0].contains(r#"""odd;identifier"""#));
    assert!(parts[1].contains(r"U&'d\0061t;a'"));
}

#[test]
fn split_statements_keeps_sql_standard_atomic_body_together() {
    let text = "CREATE FUNCTION atomic_body(value anyelement) RETURNS integer LANGUAGE SQL BEGIN ATOMIC SELECT 1; END; SELECT 2;";
    let parts = split_statements(text);
    assert_eq!(parts.len(), 2, "{parts:?}");
    assert!(parts[0].contains("BEGIN ATOMIC SELECT 1; END"));
    assert_eq!(parts[1], "SELECT 2");
}

#[test]
fn split_statements_tracks_case_and_nested_atomic_bodies() {
    let text = "CREATE FUNCTION outer_body() RETURNS integer LANGUAGE SQL BEGIN ATOMIC \
                SELECT CASE WHEN ';' = ';' THEN 1 ELSE 0 END; \
                CREATE FUNCTION inner_body() RETURNS integer LANGUAGE SQL BEGIN ATOMIC \
                    SELECT 2 /* body; comment */; \
                END; \
                SELECT 3; \
                END; \
                SELECT $$after;body$$;";
    let parts = split_statements(text);
    assert_eq!(parts.len(), 2, "{parts:?}");
    assert!(parts[0].contains("CASE WHEN ';' = ';' THEN 1 ELSE 0 END;"));
    assert!(parts[0].contains("CREATE FUNCTION inner_body()"));
    assert!(parts[0].contains("SELECT 2 /* body; comment */;"));
    assert!(parts[0].contains("SELECT 3;"));
    assert_eq!(parts[1], "SELECT $$after;body$$");
}

#[test]
fn begin_atomic_only_nests_inside_a_routine_declaration() {
    let parts = split_statements("BEGIN ATOMIC; SELECT 2;");
    assert_eq!(parts, ["BEGIN ATOMIC", "SELECT 2"]);
}

#[test]
fn command_text_uses_one_implicit_transaction_for_multiple_statements() {
    let engine = Engine::new();
    engine
        .sql("CREATE TABLE parent (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    engine
        .sql(
            "CREATE TABLE child (id INTEGER PRIMARY KEY, parent_id INTEGER, CONSTRAINT child_parent_fk FOREIGN KEY (parent_id) REFERENCES parent(id) DEFERRABLE INITIALLY IMMEDIATE)",
            &[],
        )
        .unwrap();
    let mut session = Session {
        engine,
        db_path: None,
        db_key: None,
        location: ":memory:".into(),
        history: Vec::new(),
        history_path: None,
        show_timing: false,
        expanded: false,
        copy_text: false,
        output_path: None,
    };
    let mut out = Vec::new();
    session
        .execute_command_text_with_history(
            "SET CONSTRAINTS child_parent_fk DEFERRED; INSERT INTO child VALUES (1, 101); INSERT INTO parent VALUES (101); COMMIT;",
            &mut out,
            false,
        )
        .unwrap();
    assert_eq!(
        session
            .engine
            .sql("SELECT parent_id FROM child WHERE id = 1", &[])
            .unwrap()
            .rows[0]["parent_id"],
        Value::Int(101)
    );
    let duplicate = session
        .execute_command_text_with_history(
            "INSERT INTO parent VALUES (303); INSERT INTO parent VALUES (303); COMMIT;",
            &mut out,
            false,
        )
        .unwrap_err();
    assert!(duplicate.starts_with("23505:"), "{duplicate}");
    assert!(session
        .engine
        .sql("SELECT id FROM parent WHERE id = 303", &[])
        .unwrap()
        .rows
        .is_empty());

    session
        .execute_command_text_with_history(
            "INSERT INTO parent VALUES (404); ROLLBACK;",
            &mut out,
            false,
        )
        .unwrap();
    assert!(session
        .engine
        .sql("SELECT id FROM parent WHERE id = 404", &[])
        .unwrap()
        .rows
        .is_empty());

    let savepoint = session
        .execute_command_text_with_history(
            "INSERT INTO parent VALUES (405); SAVEPOINT command_savepoint;",
            &mut out,
            false,
        )
        .unwrap_err();
    assert!(savepoint.starts_with("25P01:"), "{savepoint}");
    assert!(session
        .engine
        .sql("SELECT id FROM parent WHERE id = 405", &[])
        .unwrap()
        .rows
        .is_empty());

    session
        .execute_command_text_with_history(
            "INSERT INTO parent VALUES (406); BEGIN; INSERT INTO parent VALUES (407); ROLLBACK;",
            &mut out,
            false,
        )
        .unwrap();
    assert!(session
        .engine
        .sql("SELECT id FROM parent WHERE id IN (406, 407)", &[])
        .unwrap()
        .rows
        .is_empty());
}

#[test]
fn terminator_detection_waits_for_dollar_quote_close() {
    assert!(!contains_statement_terminator(
        "CREATE FUNCTION f() AS $$ BEGIN RETURN 1;"
    ));
    assert!(contains_statement_terminator(
        "CREATE FUNCTION f() AS $$ BEGIN RETURN 1; END; $$ LANGUAGE plpgsql;"
    ));
}

#[test]
fn terminator_detection_waits_for_atomic_body_end() {
    assert!(!contains_statement_terminator(
        "CREATE FUNCTION f() RETURNS integer LANGUAGE SQL BEGIN ATOMIC SELECT 1;"
    ));
    assert!(contains_statement_terminator(
        "CREATE FUNCTION f() RETURNS integer LANGUAGE SQL BEGIN ATOMIC SELECT 1; END;"
    ));
}

#[test]
fn meta_ds_lists_sequences_using_search_path() {
    let engine = Engine::new();
    engine.sql("CREATE SCHEMA app", &[]).unwrap();
    engine.set_search_path(vec!["app".into(), "public".into()]);
    assert!(engine.create_sequence("acct_seq", 10, 2, false).unwrap());
    assert_eq!(engine.nextval("acct_seq").unwrap(), 10);
    let mut session = Session {
        engine,
        db_path: None,
        db_key: None,
        location: ":memory:".into(),
        history: Vec::new(),
        history_path: None,
        show_timing: false,
        expanded: false,
        copy_text: false,
        output_path: None,
    };

    let mut out = Vec::new();
    assert_eq!(
        session.handle_meta("ds acct_seq", &mut out),
        PromptLineOutcome::Continue
    );
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("app.acct_seq"), "{text}");
    assert!(text.contains("10"), "{text}");
}

#[test]
fn highlighter_marks_keywords_registry_functions_and_literals() {
    let highlighted = highlight_sql_line("select text_match(body, 'rust') -- comment");
    assert!(highlighted.contains("\x1b[1;34mselect\x1b[0m"));
    assert!(highlighted.contains("\x1b[1;34mtext_match\x1b[0m"));
    assert!(highlighted.contains("\x1b[32m'rust'\x1b[0m"));
    assert!(highlighted.contains("\x1b[90m-- comment\x1b[0m"));
}

#[test]
fn highlighter_forces_refresh_while_typing_sql_tokens() {
    let helper = UsqlHelper::new(Vec::new(), Vec::new(), Vec::new());
    assert!(helper.highlight_char("sele", 4, CmdKind::Other));
    assert!(helper
        .highlight("select", 6)
        .contains("\x1b[1;34mselect\x1b[0m"));
}

#[test]
fn highlighter_keeps_uppercase_keywords_case_insensitive() {
    let highlighted = highlight_sql_line("SELECT text_match(body, 'rust') -- comment");
    assert!(highlighted.contains("\x1b[1;34mSELECT\x1b[0m"));
    assert!(highlighted.contains("\x1b[1;34mtext_match\x1b[0m"));
    assert!(highlighted.contains("\x1b[32m'rust'\x1b[0m"));
    assert!(highlighted.contains("\x1b[90m-- comment\x1b[0m"));
}
