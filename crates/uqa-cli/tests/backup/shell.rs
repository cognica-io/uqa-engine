//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{is_id, retire_parent, success, Cli, CHANGE, ROWS, SEED, SNAPSHOT, WRITE, WRITTEN};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, ExitStatus, Stdio};

struct Shell {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl Shell {
    fn new(cli: &Cli, connection: &[&str]) -> Self {
        let mut child = cli
            .command(connection)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        Self {
            input: child.stdin.take().unwrap(),
            output: BufReader::new(child.stdout.take().unwrap()),
            child,
        }
    }

    fn execute(&mut self, commands: &str) -> String {
        const END: &str = "backup-test-command-complete";
        writeln!(self.input, "{commands}\n\\echo {END}").unwrap();
        self.input.flush().unwrap();
        let mut transcript = String::new();
        let mut line = String::new();
        loop {
            line.clear();
            assert_ne!(self.output.read_line(&mut line).unwrap(), 0, "{transcript}");
            let text = line.replace("usql> ", "").replace("\r\n", "\n");
            if text.trim_end() == END {
                return transcript;
            }
            transcript.push_str(&text);
        }
    }

    fn capture(&mut self, command: &str) -> String {
        let response = self.execute(command);
        let id = response.trim_end();
        assert!(is_id(id), "unexpected capture output {response:?}");
        id.to_owned()
    }

    fn finish(mut self) -> ExitStatus {
        writeln!(self.input, "\\q").unwrap();
        drop(self.input);
        self.child.wait().unwrap()
    }
}

pub(super) fn capture_in_session(
    cli: &Cli,
    connection: &[&str],
    repository: &str,
) -> (String, String) {
    let mut source = Shell::new(cli, connection);
    source.execute(&format!("\\set ON_ERROR_STOP on\n{SEED}"));
    let full = source.capture(&format!("\\backup create '{repository}'"));
    source.execute(CHANGE);
    let incremental = source.capture(&format!("\\backup create '{repository}' --parent {full}"));
    assert!(source.finish().success());
    (full, incremental)
}

#[test]
fn piped_shell_restores_standalone_memory_images_and_keeps_writes_in_a_new_capture() {
    let cli = Cli::new();
    let repo = "memory archive";
    success(cli.backup(&[], &["init", repo]));
    success(cli.backup(&[], &["init", "later archive"]));
    let (full, incremental) = capture_in_session(&cli, &[], repo);
    retire_parent(&cli, repo, &full, &incremental);

    let mut restored = Shell::new(&cli, &[]);
    restored.execute(&format!("\\set ON_ERROR_STOP on\n\\pset format unaligned\n\\t on\n\\backup restore 'memory archive' {incremental} :memory:"));
    assert_eq!(restored.execute(ROWS), SNAPSHOT.replace('\t', "|"));
    restored.execute(WRITE);
    let after_write = restored.capture("\\backup create 'later archive'");
    assert!(restored.finish().success());

    let mut reopened = Shell::new(&cli, &[]);
    reopened.execute(&format!("\\set ON_ERROR_STOP on\n\\pset format unaligned\n\\t on\n\\backup restore 'later archive' {after_write} :memory:"));
    assert_eq!(reopened.execute(ROWS), WRITTEN.replace('\t', "|"));
    assert!(reopened.finish().success());
}

#[test]
fn shell_backup_admission_uses_the_active_role_and_preserves_the_session_on_error() {
    let cli = Cli::new();
    success(cli.backup(&[], &["init", "repo"]));
    let mut shell = Shell::new(&cli, &[]);
    shell.execute(SEED);
    let full = shell.capture("\\backup create repo");
    shell.execute(&format!(
        "{WRITE}\nCREATE ROLE backup_reader; SET ROLE backup_reader;"
    ));
    let denied = shell.execute("\\backup create repo");
    assert!(
        denied.contains("database backup requires superuser authority"),
        "{denied}"
    );
    let denied = shell.execute(&format!("\\backup restore repo {full} :memory:"));
    assert!(
        denied.contains("database restore requires superuser authority"),
        "{denied}"
    );
    shell.execute("RESET ROLE;\n\\pset format unaligned\n\\t on");
    assert_eq!(
        shell.execute("SELECT note FROM items WHERE id = 5;"),
        "later\n"
    );
    assert!(!shell.finish().success());
    assert_eq!(
        success(cli.backup(&[], &["list", "repo"])),
        format!("{full}\t-\n")
    );
}
