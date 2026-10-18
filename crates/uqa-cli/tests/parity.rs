//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! CLI behavior coverage for the `usql` binary.

use super::binary_path;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

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
        out.contains("usql 0.6.0 -- UQA interactive SQL shell"),
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
fn legacy_vector_output_errors_reach_the_cli_exit_status() {
    let dir = tempfile::tempdir().unwrap();
    for ty in ["int2vector", "oidvector"] {
        let sql = format!("SELECT trim_array('1 2'::{ty},2)");
        for mode in [None, Some("--copy-text")] {
            let mut args = Vec::new();
            args.extend(mode);
            args.extend(["-c", &sql]);
            let output = run_usql(&args, "", dir.path());
            assert!(
                !output.status.success(),
                "{ty} {mode:?}: {}",
                stdout(&output)
            );
            assert!(
                stdout(&output).contains(&format!("42804: array is not a valid {ty}")),
                "{}",
                stdout(&output)
            );
        }
    }
}

#[test]
fn builtin_nori_follows_the_cli_feature_configuration() {
    verify_builtin("nori", "한국 경제", 0, "한국", cfg!(feature = "nori"));
}

#[test]
fn builtin_kuromoji_follows_the_cli_feature_configuration() {
    verify_builtin(
        "kuromoji",
        "ＵＱＡで走りました",
        1,
        "走る",
        cfg!(feature = "kuromoji"),
    );
    verify_builtin(
        "kuromoji_completion",
        "東京",
        1,
        "toukyou",
        cfg!(feature = "kuromoji"),
    );
}

fn verify_builtin(name: &str, input: &str, position: usize, expected: &str, enabled: bool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let sql = format!("SELECT analysis -> 'tokens' -> {position} ->> 'term' AS term FROM analyze_text('{name}', '{input}')");
    let output = run_usql(&["--copy-text", "-c", &sql], "", dir.path());
    if enabled {
        assert!(output.status.success(), "{}", stderr(&output));
        assert_eq!(stdout(&output), format!("{expected}\n"));
    } else {
        assert!(!output.status.success());
        assert!(
            stdout(&output).contains(&format!("analyzer `{name}` is not registered")),
            "{}",
            stdout(&output)
        );
    }
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
fn result_output_resolves_regtype_oids_with_session_catalog() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sql = "CREATE DOMAIN cli_domain AS integer; SELECT pg_typeof(1), pg_typeof(1::cli_domain), ARRAY['integer'::regtype, 'cli_domain'::regtype]";
    let output = run_usql(&["--copy-text", "-c", sql], "", dir.path());
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "integer\tcli_domain\t{integer,cli_domain}\n"
    );
    let output = run_usql(&["-c", sql], "", dir.path());
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(
        stdout(&output)
            .lines()
            .any(|line| line.split('|').map(str::trim).collect::<Vec<_>>()
                == ["integer", "cli_domain", "{integer,cli_domain}"]),
        "{}",
        stdout(&output)
    );
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
fn command_string_prints_error_detail_and_hint_lines() {
    let dir = tempfile::tempdir().expect("tempdir");
    let output = run_usql(
        &["-c", "SET session_replication_role = rep"],
        "",
        dir.path(),
    );
    assert!(!output.status.success(), "stdout: {}", stdout(&output));
    assert!(
        stdout(&output).contains(
            "ERROR: 22023: invalid value for parameter \"session_replication_role\": \"rep\"\nHINT: Available values: origin, replica, local.\n"
        ),
        "{}",
        stdout(&output)
    );
    let output = run_usql(
        &[
            "-c",
            "CREATE TABLE pt (k int) PARTITION BY RANGE (k); CREATE TABLE p1 PARTITION OF pt FOR VALUES FROM (0) TO (10); ALTER TABLE pt DETACH PARTITION p1 FINALIZE",
        ],
        "",
        dir.path(),
    );
    assert!(
        stdout(&output).contains(
            "ERROR: 55000: cannot complete detaching partition \"p1\"\nDETAIL: There's no pending concurrent detach.\n"
        ),
        "{}",
        stdout(&output)
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
fn piped_final_statement_executes_at_eof_like_psql() {
    let dir = tempfile::tempdir().expect("tempdir");
    let output = run_usql(&[], "SELECT 1\n", dir.path());
    assert!(output.status.success(), "stdout: {}", stdout(&output));
    assert!(stdout(&output).contains('1'));
    assert!(!stdout(&output).contains("ERROR"));
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
    assert!(out.contains("List sequences [sequence]"), "{out}");
    assert!(out.contains("\\dF"), "{out}");
    assert!(out.contains("\\dS"), "{out}");
    assert!(out.contains("Show statistics <table>"), "{out}");
    assert!(out.contains("Open database <path>"), "{out}");
    assert!(!out.contains("persistent SQLite storage"), "{out}");
    assert!(out.contains("table_name"), "{out}");
    assert!(out.contains("users"), "{out}");
    assert!(out.contains("public\".\"users"), "{out}");
    assert!(out.contains("indexname"), "{out}");
    assert!(out.contains("Statistics for \"users\""), "{out}");
    assert!(out.contains("sequence_name"), "{out}");
    assert!(out.contains("user_seq"), "{out}");
    assert!(out.contains("-[ RECORD 1 ]"), "{out}");
    assert!(out.contains("Output redirected to:"), "{out}");
    assert!(out.contains("Output restored to stdout"), "{out}");
    assert!(out.contains("Engine reset."), "{out}");
    assert!(out.contains("(0 rows)"), "{out}");

    let redirected = std::fs::read_to_string(&output_file).expect("redirected output");
    assert!(redirected.contains("Alice"), "{redirected}");
}

#[test]
fn foreign_catalog_commands_do_not_require_source_handlers() {
    let dir = tempfile::tempdir().unwrap();
    let output = run_usql(
        &[],
        "CREATE SERVER remote_source FOREIGN DATA WRAPPER duckdb_fdw;\n\
         CREATE FOREIGN TABLE remote_items(cursor refcursor, ids smallint[], amount numeric(9,2)) SERVER remote_source OPTIONS (source 'items');\n\
         \\dt\n\\d remote_items\n\\dft\n\\dfs\n\\q\n",
        dir.path(),
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let out = stdout(&output);
    for expected in [
        "remote_items",
        "remote_source",
        "duckdb_fdw",
        "refcursor",
        "smallint[]",
        "numeric(9,2)",
    ] {
        assert!(out.contains(expected), "missing {expected}: {out}");
    }
    assert!(!out.contains("ERROR"), "{out}");
    assert!(!out.contains("not available"), "{out}");
    assert!(stderr(&output).is_empty(), "{}", stderr(&output));
}

#[cfg(unix)]
#[test]
fn terminal_editor_signals_and_idle_notifications_use_the_real_binary() {
    let dir = tempfile::tempdir().unwrap();
    let output = std::process::Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/terminal.py"
        ))
        .arg(binary_path())
        .arg(dir.path())
        .output()
        .expect("execute the Unix terminal fixture with Python 3");
    assert!(
        output.status.success(),
        "{}\n{}",
        stdout(&output),
        stderr(&output)
    );
}

#[test]
fn catalog_commands_query_native_catalogs_instead_of_missing_views() {
    let dir = tempfile::tempdir().unwrap();
    let output = run_usql(
        &[],
        r"CREATE TABLE shell_catalog_item(id int PRIMARY KEY);
CREATE VIEW shell_catalog_view AS SELECT id FROM shell_catalog_item;
\d shell_catalog_item
\d shell_catalog_view
\dt shell_catalog_*
\di shell_catalog_*
\dv shell_catalog_*
\dn public
\df
\da
\du
\dg
\l
\dS
\dx
\dp shell_catalog_*
\z shell_catalog_*
\dft
\dfs
\analyzers
\graphs
\q
",
        dir.path(),
    );
    let out = stdout(&output);
    assert!(output.status.success(), "{out}\n{}", stderr(&output));
    assert!(!out.contains("ERROR"), "{out}");
    for expected in [
        "shell_catalog_item",
        "shell_catalog_view",
        "shell_catalog_item_pkey",
        "plpgsql",
        "extversion",
        "access_privileges",
    ] {
        assert!(out.contains(expected), "missing {expected}: {out}");
    }
}
