//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! CLI behavior coverage for the `usql` binary.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn binary_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_usql"))
}

fn run_usql(args: &[&str], input: &str, history_dir: &Path) -> Output {
    let history = history_dir.join("hist");
    let mut child = Command::new(binary_path())
        .args(args)
        .env("UQA_HISTORY", history)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn usql");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(input.as_bytes())
        .expect("write stdin");
    child.wait_with_output().expect("wait")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn banner_matches_expected_usql_shape() {
    let dir = tempfile::tempdir().expect("tempdir");
    let output = run_usql(&[], "\\q\n", dir.path());
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let out = stdout(&output);
    assert!(
        out.contains("usql 0.2.0 -- UQA interactive SQL shell"),
        "{out}"
    );
    assert!(out.contains("Database: :memory:"), "{out}");
    assert!(
        out.contains("Type SQL statements terminated by ';'"),
        "{out}"
    );
    assert!(out.contains("Use \\? for help, \\q to quit."), "{out}");
}

#[test]
fn command_string_executes_without_repl_banner() {
    let dir = tempfile::tempdir().expect("tempdir");
    let output = run_usql(&["-c", "SELECT 1 AS x"], "", dir.path());
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let out = stdout(&output);
    assert!(out.contains('x'), "{out}");
    assert!(out.lines().any(|line| line.trim() == "1"), "{out}");
    assert!(!out.contains("UQA interactive SQL shell"), "{out}");
}

#[test]
fn copy_text_output_distinguishes_null_empty_and_literal_null() {
    let dir = tempfile::tempdir().expect("tempdir");
    let output = run_usql(
        &[
            "--copy-text",
            "-c",
            "SELECT ''::text AS empty, NULL AS missing, 'NULL'::text AS literal",
        ],
        "",
        dir.path(),
    );
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert_eq!(stdout(&output), "\t\\N\tNULL\n");
}

#[test]
fn copy_text_output_uses_postgresql_boolean_type_output() {
    let dir = tempfile::tempdir().expect("tempdir");
    let output = run_usql(
        &["--copy-text", "-c", "SELECT true, false, ROW(true, false)"],
        "",
        dir.path(),
    );
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert_eq!(stdout(&output), "t\tf\t(t,f)\n");
}

#[test]
fn command_string_ignores_interactive_history() {
    let dir = tempfile::tempdir().expect("tempdir");
    let history = dir.path().join("history-is-a-directory");
    std::fs::create_dir(&history).expect("create history directory");
    let output = Command::new(binary_path())
        .args(["-c", "SELECT 1 AS x"])
        .env("UQA_HISTORY", &history)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run usql");
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(stdout(&output).lines().any(|line| line.trim() == "1"));
}

#[test]
fn command_string_unaliased_literal_displays_value() {
    let dir = tempfile::tempdir().expect("tempdir");
    let output = run_usql(&["-c", "SELECT 1"], "", dir.path());
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let out = stdout(&output);
    assert!(out.contains("?column?"), "{out}");
    assert!(out.lines().any(|line| line.trim() == "1"), "{out}");
    assert!(!out.contains("NULL"), "{out}");
}

#[test]
fn command_string_returns_failure_when_sql_execution_fails() {
    let dir = tempfile::tempdir().expect("tempdir");
    let output = run_usql(&["-c", "SELECT * FROM missing_table"], "", dir.path());
    assert!(!output.status.success(), "stdout: {}", stdout(&output));
    assert!(stdout(&output).contains("ERROR:"), "{}", stdout(&output));
}

#[test]
fn command_string_routes_sql_notices_to_stderr() {
    let dir = tempfile::tempdir().expect("tempdir");
    let output = run_usql(&["--copy-text", "-c", "COMMIT"], "", dir.path());
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert_eq!(stdout(&output), "");
    assert_eq!(
        stderr(&output),
        "WARNING: there is no transaction in progress\n"
    );
}

#[test]
fn command_string_preserves_sql_standard_function_body_for_engine_validation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sql = "CREATE FUNCTION cli_atomic(value anyelement) RETURNS integer LANGUAGE SQL BEGIN ATOMIC SELECT 1; END;";
    let output = run_usql(&["-c", sql], "", dir.path());
    assert!(!output.status.success(), "stdout: {}", stdout(&output));
    assert!(
        stdout(&output).contains("ERROR: 42P13"),
        "{}",
        stdout(&output)
    );
    assert!(!stdout(&output).contains("42601"), "{}", stdout(&output));
}

#[test]
fn piped_sql_returns_failure_when_execution_fails() {
    let dir = tempfile::tempdir().expect("tempdir");
    let output = run_usql(&[], "SELECT * FROM missing_table;\n", dir.path());
    assert!(!output.status.success(), "stdout: {}", stdout(&output));
    assert!(stdout(&output).contains("ERROR:"), "{}", stdout(&output));
}

#[test]
fn piped_unterminated_sql_is_not_silently_discarded() {
    let dir = tempfile::tempdir().expect("tempdir");
    let output = run_usql(&[], "SELECT 1\n", dir.path());
    assert!(!output.status.success(), "stdout: {}", stdout(&output));
    assert!(
        stdout(&output).contains("unterminated SQL statement"),
        "{}",
        stdout(&output)
    );
}

#[test]
fn piped_meta_command_failure_sets_a_failure_exit_status() {
    let dir = tempfile::tempdir().expect("tempdir");
    let output = run_usql(&[], "\\open\n\\definitely-not-a-command\n", dir.path());
    assert!(!output.status.success(), "stdout: {}", stdout(&output));
    let out = stdout(&output);
    assert!(out.contains("ERROR: usage: \\open <path>"), "{out}");
    assert!(out.contains("ERROR: unknown command"), "{out}");
}

#[test]
fn db_argument_persists_between_invocations() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("uqa.db");
    let db_arg = db.to_string_lossy().into_owned();
    let create = run_usql(
        &[
            "--db",
            &db_arg,
            "-c",
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT); INSERT INTO users (id, name) VALUES (1, 'Alice')",
        ],
        "",
        dir.path(),
    );
    assert!(create.status.success(), "stderr: {}", stderr(&create));

    let select = run_usql(
        &["--db", &db_arg, "-c", "SELECT name FROM users"],
        "",
        dir.path(),
    );
    assert!(select.status.success(), "stderr: {}", stderr(&select));
    let out = stdout(&select);
    assert!(out.contains("Alice"), "{out}");
}

#[test]
fn script_file_executes_and_exits_when_stdin_is_not_terminal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("script.sql");
    std::fs::write(&script, "SELECT 2 AS y;").expect("write script");
    let script_arg = script.to_string_lossy().into_owned();
    let output = run_usql(&[&script_arg], "", dir.path());
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let out = stdout(&output);
    assert!(out.contains('y'), "{out}");
    assert!(out.contains('2'), "{out}");
    assert!(!out.contains("UQA interactive SQL shell"), "{out}");
}

#[test]
fn script_file_returns_failure_when_sql_execution_fails() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("broken.sql");
    std::fs::write(&script, "SELECT * FROM missing_table;").expect("write script");
    let script_arg = script.to_string_lossy().into_owned();
    let output = run_usql(&[&script_arg], "", dir.path());
    assert!(!output.status.success(), "stdout: {}", stdout(&output));
    assert!(
        stderr(&output).contains("missing_table"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn python_backslash_surface_is_available() {
    let dir = tempfile::tempdir().expect("tempdir");
    let output_file = dir.path().join("out.txt");
    let input = format!(
        "\\?\n\
         CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INT);\n\
         INSERT INTO users (id, name, age) VALUES (1, 'Alice', 30);\n\
         \\dt\n\
         \\d users\n\
         CREATE INDEX idx_users_name ON users USING gin (name);\n\
         \\di\n\
         \\stats users\n\
         CREATE SEQUENCE user_seq START WITH 5;\n\
         \\ds user_seq\n\
         \\x\n\
         SELECT id, name FROM users ORDER BY id;\n\
         \\x\n\
         \\o {}\n\
         SELECT name FROM users;\n\
         \\o\n\
         \\reset\n\
         \\dt\n\
         \\q\n",
        output_file.display()
    );
    let output = run_usql(&[], &input, dir.path());
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let out = stdout(&output);
    assert!(out.contains("Backslash commands:"), "{out}");
    assert!(out.contains("\\di"), "{out}");
    assert!(out.contains("\\ds [sequence]"), "{out}");
    assert!(out.contains("\\dF"), "{out}");
    assert!(out.contains("\\dS"), "{out}");
    assert!(out.contains("\\stats <table>"), "{out}");
    assert!(
        out.contains("\\open <path>    Switch to persistent storage"),
        "{out}"
    );
    assert!(!out.contains("persistent SQLite storage"), "{out}");
    assert!(out.contains("table_name"), "{out}");
    assert!(out.contains("users"), "{out}");
    assert!(out.contains("Table \"users\""), "{out}");
    assert!(out.contains("indexed_fields"), "{out}");
    assert!(out.contains("Statistics for \"users\""), "{out}");
    assert!(out.contains("sequence_name"), "{out}");
    assert!(out.contains("user_seq"), "{out}");
    assert!(out.contains("-[ RECORD 1 ]"), "{out}");
    assert!(out.contains("Output redirected to:"), "{out}");
    assert!(out.contains("Output restored to stdout"), "{out}");
    assert!(out.contains("Engine reset."), "{out}");
    assert!(out.contains("No tables."), "{out}");

    let redirected = std::fs::read_to_string(&output_file).expect("redirected output");
    assert!(redirected.contains("Alice"), "{redirected}");
}
