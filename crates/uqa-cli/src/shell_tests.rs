//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use rustyline::history::MemHistory;

fn session() -> Session {
    Session::new_without_history(None, None).unwrap()
}
fn script(session: &mut Session, text: &str) -> (String, bool) {
    let mut output = Vec::new();
    let mut buffer = String::new();
    let mut failed = false;
    for line in text.lines() {
        failed |= session.process_line(line, &mut buffer, &mut output, false)
            == PromptLineOutcome::Failed;
    }
    if !buffer.trim().is_empty() {
        failed |= session.finish_input(&mut buffer, &mut output).is_err();
    }
    (String::from_utf8(output).unwrap(), failed)
}

#[test]
fn editor_context_preserves_literals_comments_and_real_catalog_names() {
    let session = session();
    session
        .engine
        .sql(
            "CREATE TABLE shell_probe(id int); CREATE TABLE \"user\"(id int); CREATE TABLE \"이름 표\"(\"열 이름\" int)",
            &[],
        )
        .unwrap();
    let mut helper = session.repl_helper().unwrap();
    let history = MemHistory::new();
    let context = Context::new(&history);
    for input in [
        "SELECT * FROM shell_pr",
        "SELECT * FROM public.shell_pr",
        "SELECT * FROM \"이름",
        "SELECT * FROM \"us",
    ] {
        let (_, matches) = helper.complete(input, input.len(), &context).unwrap();
        assert!(!matches.is_empty(), "{input}");
    }
    for (buffer, line) in [
        ("", "SELECT 'SEL"),
        ("", "-- SEL"),
        ("SELECT $é$", "SELECT"),
        ("/* nested /* comment", "SELECT"),
    ] {
        helper.configure(buffer, true, &crate::variables::Variables::default());
        let (_, matches) = helper.complete(line, line.len(), &context).unwrap();
        assert!(matches.is_empty(), "{buffer}\n{line}");
        assert!(
            !helper
                .highlight(line, line.len())
                .contains("\x1b[1;34mSELECT"),
            "{buffer}\n{line}"
        );
    }
}

#[test]
fn completion_displays_controls_without_changing_the_inserted_identifier() {
    let name = "\"unsafe\x1b[31mname\"";
    let helper = UsqlHelper::new(vec![name.into()], Vec::new(), Vec::new());
    let candidates = helper.completion_candidates("\"unsafe", true);
    assert_eq!(candidates.len(), 1);
    assert!(!candidates[0].display.contains('\x1b'));
    assert_eq!(candidates[0].replacement, name);
}

#[test]
fn timing_and_expanded_arguments_are_settings_not_toggles() {
    let mut session = session();
    let (output, failed) = script(
        &mut session,
        "\\timing off\n\\timing off\n\\x auto\n\\x off\n\\x off\n",
    );
    assert!(!failed, "{output}");
    assert!(!session.show_timing);
    assert_eq!(session.shell.printing.expanded, "off");
    assert_eq!(output.matches("Timing is off.").count(), 2);
    let (_, failed) = script(&mut session, "\\timing invalid\n\\x invalid\n");
    assert!(failed);
    assert!(!session.show_timing);
    assert_eq!(session.shell.printing.expanded, "off");
}

#[test]
fn query_buffer_variables_conditionals_and_gset_preserve_execution_order() {
    let mut session = session();
    let (output, failed) = script(
        &mut session,
        r"\set ON_ERROR_STOP on
\set n 41
SELECT :n+1 AS answer
\gset result_
\if :{?result_answer}
SELECT :'result_answer' AS value \g
\else
SELECT nonexistent FROM nonexistent;
\endif
SELECT 9
\r
SELECT 7 \gx
",
    );
    assert!(!failed, "{output}");
    assert_eq!(session.shell.variables.get("result_answer"), Some("42"));
    assert!(output.contains("42"));
    assert!(output.contains("RECORD 1"));
    assert!(!output.contains("nonexistent"));
}

#[test]
fn finished_statements_do_not_execute_the_incomplete_tail() {
    let mut session = session();
    let mut output = Vec::new();
    let mut buffer = String::new();
    assert_eq!(
        session.process_line(
            "CREATE TABLE saved(id int); INSERT INTO saved VALUES ('",
            &mut buffer,
            &mut output,
            false
        ),
        PromptLineOutcome::Continue
    );
    assert!(session.engine.describe_table("saved").unwrap().is_some());
    assert!(buffer.contains("INSERT INTO"));
    assert_eq!(
        session.process_line("1');", &mut buffer, &mut output, false),
        PromptLineOutcome::Continue
    );
    assert!(buffer.is_empty());
}

#[test]
fn query_failure_consumes_g_buffer_and_does_not_repeat_a_write() {
    let mut session = session();
    let (output,failed)=script(&mut session,"CREATE TABLE calls(id int PRIMARY KEY);\nINSERT INTO calls VALUES (1)\\; SELECT missing\n\\g\nSELECT COUNT(*) FROM calls;\n");
    assert!(failed, "{output}");
    let result = session
        .engine
        .sql("SELECT COUNT(*) FROM calls", &[])
        .unwrap();
    assert_eq!(result.value_at(0, 0), Some(&Value::Int(0)));
}

#[test]
fn formats_preserve_positional_columns_and_escape_terminal_controls() {
    let mut session = session();
    let (output, failed) = script(
        &mut session,
        "\\pset format csv\nSELECT 1 AS x, 2 AS x;\nSELECT '' AS empty, NULL AS null;\n",
    );
    assert!(!failed, "{output}");
    assert!(output.contains("x,x\n1,2\n"), "{output}");
    assert!(output.contains("empty,null\n,\n"), "{output}");
    let (output,failed)=script(&mut session,"\\pset format aligned\nSELECT '한글' AS v, 1 AS n UNION ALL SELECT 'a', 2;\nSELECT chr(27)||'[31mred' AS v;\n");
    assert!(!failed, "{output}");
    assert!(!output.contains('\x1b'));
    assert!(output.contains("한글 |"), "{output}");
    assert!(output.contains("a    |"), "{output}");
}

#[test]
fn file_copy_round_trip_and_failed_export_preserve_destination() {
    let mut session = session();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rows with spaces.csv");
    let (output,failed)=script(&mut session,&format!("CREATE TABLE copied(id int, body text);\nINSERT INTO copied VALUES (1, 'a,b'),(2,'한글');\n\\copy copied TO '{}' WITH (FORMAT CSV, HEADER)\nDELETE FROM copied;\n\\copy copied FROM '{}' WITH (FORMAT CSV, HEADER)\n",path.display(),path.display()));
    assert!(!failed, "{output}");
    let before = std::fs::read(&path).unwrap();
    let (_, failed) = script(
        &mut session,
        &format!(
            "\\copy (SELECT 1/0) TO '{}' WITH (FORMAT CSV)",
            path.display()
        ),
    );
    assert!(failed);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(
        session
            .engine
            .sql("SELECT COUNT(*) FROM copied", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Int(2))
    );
}

#[test]
fn script_relative_includes_conditionals_and_cycle_detection() {
    let mut session = session();
    let dir = tempfile::tempdir().unwrap();
    let outer = dir.path().join("outer.sql");
    let inner = dir.path().join("inner.sql");
    std::fs::write(&inner, "\\set answer 42\n").unwrap();
    std::fs::write(&outer, "\\ir inner.sql\nSELECT :answer;\n").unwrap();
    let mut output = Vec::new();
    session.include_script(&outer, &mut output).unwrap();
    assert!(String::from_utf8(output).unwrap().contains("42"));
    std::fs::write(&inner, "\\ir outer.sql\n").unwrap();
    assert!(session.include_script(&outer, &mut Vec::new()).is_err());
    assert!(session.shell.includes.is_empty());
}

#[test]
fn catalog_patterns_and_role_alias_do_not_ignore_arguments() {
    let mut session = session();
    let (output,failed)=script(&mut session,"CREATE TABLE keep(id int PRIMARY KEY); CREATE TABLE other(id int);\n\\dt kee*\n\\dg\n\\di\n");
    assert!(!failed, "{output}");
    assert!(output.contains("keep"));
    assert!(!output.contains("other"));
    assert!(output.contains("rolname"));
    assert!(output.contains("keep_pkey"));
}

#[test]
fn autocommit_and_gset_preserve_multi_statement_transaction_boundaries() {
    let mut session = session();
    let (output,failed)=script(&mut session,"\\set AUTOCOMMIT off\nCREATE TABLE pending(id int);\nINSERT INTO pending VALUES (1);\nROLLBACK;\n");
    assert!(!failed, "{output}");
    assert!(session.engine.describe_table("pending").unwrap().is_none());
    let (output,failed)=script(&mut session,"\\set AUTOCOMMIT on\nCREATE TABLE atomic_write(id int);\nINSERT INTO atomic_write VALUES (1)\\; SELECT absent\n\\gset\n");
    assert!(failed, "{output}");
    assert_eq!(
        session
            .engine
            .sql("SELECT COUNT(*) FROM atomic_write", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Int(0))
    );
}

#[test]
fn history_settings_control_duplicates_spaces_and_retained_length() {
    let mut session = session();
    session
        .shell
        .variables
        .set("HISTCONTROL", "none".into())
        .unwrap();
    session.remember_prompt_line("SELECT 1");
    session.remember_prompt_line("SELECT 1");
    assert_eq!(session.history.len(), 2);
    session
        .shell
        .variables
        .set("HISTCONTROL", "ignoreboth".into())
        .unwrap();
    assert!(!session.remember_prompt_line("SELECT 1"));
    assert!(!session.remember_prompt_line(" SELECT 2"));
    session.shell.variables.set("HISTSIZE", "1".into()).unwrap();
    session.remember_prompt_line("SELECT 3");
    assert_eq!(session.history, vec!["SELECT 3"]);
}

#[test]
fn no_idle_notification_polling_after_unlisten_or_rolled_back_listen() {
    let mut session = session();
    let mut out = Vec::new();
    session
        .run_statement_with_history("LISTEN updates", &mut out, false)
        .unwrap();
    assert!(session.shell.listen_seen);
    session
        .run_statement_with_history("UNLISTEN *", &mut out, false)
        .unwrap();
    assert!(!session.shell.listen_seen);
    session
        .run_statement_with_history("BEGIN", &mut out, false)
        .unwrap();
    session
        .run_statement_with_history("LISTEN updates", &mut out, false)
        .unwrap();
    session
        .run_statement_with_history("ROLLBACK", &mut out, false)
        .unwrap();
    assert!(!session.shell.listen_seen);
}

#[test]
fn postgres_csv_reference_preserves_boolean_array_null_and_duplicate_columns() {
    let mut session = session();
    session.shell.printing.format = "csv".into();
    let (output, failed) = script(
        &mut session,
        "SELECT true AS b, ARRAY[true,false] AS a, '' AS empty, NULL AS null, 1 AS x, 2 AS x;\n",
    );
    assert!(!failed, "{output}");
    // Captured independently from PostgreSQL 18.4 psql, with its default CSV and NULL settings.
    assert_eq!(output, "b,a,empty,null,x,x\nt,\"{t,f}\",,,1,2\n");
}

#[test]
fn finite_watch_executes_the_same_buffer_and_does_not_change_print_settings() {
    let mut session = session();
    let (output, failed) = script(&mut session, "SELECT 7 AS value\n\\watch 0.001 c=2\n");
    assert!(!failed, "{output}");
    assert_eq!(output.matches("(1 row)").count(), 2);
    assert_eq!(session.shell.printing.expanded, "off");
}
