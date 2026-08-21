//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::ffi::{OsStr, OsString};
use std::io;
use std::process::Stdio;
use std::time::Duration;

use secrecy::SecretString;
use serde::Deserialize;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::time::timeout;
use zeroize::{Zeroize, Zeroizing};

use crate::HttpEngineError;

const CLI_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CLI_OUTPUT_BYTES: usize = 64 * 1024;

#[derive(Deserialize)]
pub(crate) struct ResolvedConnection {
    pub(crate) url: String,
    pub(crate) token: SecretString,
}

struct CapturedOutput {
    bytes: Vec<u8>,
    exceeded_limit: bool,
}

impl Drop for CapturedOutput {
    fn drop(&mut self) {
        self.bytes.zeroize();
    }
}

pub(crate) async fn resolve_local(
    cli_path: &OsStr,
    project: &str,
) -> Result<ResolvedConnection, HttpEngineError> {
    validate_project(project)?;
    resolve(cli_path, local_arguments(project), CLI_TIMEOUT).await
}

pub(crate) async fn resolve_cloud(
    cli_path: &OsStr,
    project: &str,
    organization: Option<&str>,
) -> Result<ResolvedConnection, HttpEngineError> {
    validate_project(project)?;
    if organization.is_some_and(|value| value.trim().is_empty()) {
        return Err(HttpEngineError::EmptyOrganizationName);
    }
    resolve(
        cli_path,
        cloud_arguments(project, organization),
        CLI_TIMEOUT,
    )
    .await
}

fn validate_project(project: &str) -> Result<(), HttpEngineError> {
    if project.trim().is_empty() {
        Err(HttpEngineError::EmptyProjectName)
    } else {
        Ok(())
    }
}

fn local_arguments(project: &str) -> Vec<OsString> {
    ["local", "connection", project, "--format", "json"]
        .into_iter()
        .map(OsString::from)
        .collect()
}

fn cloud_arguments(project: &str, organization: Option<&str>) -> Vec<OsString> {
    let mut arguments = ["cloud", "connection", project, "--format", "json"]
        .into_iter()
        .map(OsString::from)
        .collect::<Vec<_>>();
    if let Some(organization) = organization {
        arguments.push(OsString::from("--org"));
        arguments.push(OsString::from(organization));
    }
    arguments
}

async fn resolve(
    cli_path: &OsStr,
    arguments: Vec<OsString>,
    maximum_duration: Duration,
) -> Result<ResolvedConnection, HttpEngineError> {
    let mut child = cli_command(cli_path, arguments)
        .spawn()
        .map_err(HttpEngineError::CLIUnavailable)?;
    let stdout = child.stdout.take().ok_or_else(|| {
        HttpEngineError::CLIExecution(io::Error::other("uqa CLI stdout pipe unavailable"))
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        HttpEngineError::CLIExecution(io::Error::other("uqa CLI stderr pipe unavailable"))
    })?;

    let execution = timeout(maximum_duration, async {
        let (status, stdout, stderr_exceeded_limit) =
            tokio::join!(child.wait(), capture_bounded(stdout), drain_bounded(stderr));
        let status = status.map_err(HttpEngineError::CLIExecution)?;
        let stdout = stdout.map_err(HttpEngineError::CLIExecution)?;
        let stderr_exceeded_limit = stderr_exceeded_limit.map_err(HttpEngineError::CLIExecution)?;
        Ok::<_, HttpEngineError>((status, stdout, stderr_exceeded_limit))
    })
    .await;

    let Ok(result) = execution else {
        let _ = child.kill().await;
        let _ = child.wait().await;
        return Err(HttpEngineError::CLITimedOut);
    };
    let (status, stdout, stderr_exceeded_limit) = result?;
    if stdout.exceeded_limit || stderr_exceeded_limit {
        return Err(HttpEngineError::CLIOutputTooLarge);
    }
    if !status.success() {
        return Err(HttpEngineError::CLIConnectionFailed);
    }
    serde_json::from_slice(&stdout.bytes).map_err(HttpEngineError::InvalidCLIResponse)
}

fn cli_command(cli_path: &OsStr, arguments: Vec<OsString>) -> Command {
    let mut command = Command::new(cli_path);
    command
        .args(arguments)
        .env_remove("UQA_TOKEN")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

async fn capture_bounded<R>(mut reader: R) -> io::Result<CapturedOutput>
where
    R: AsyncRead + Unpin,
{
    let mut output = CapturedOutput {
        bytes: Vec::new(),
        exceeded_limit: false,
    };
    let mut buffer = Zeroizing::new(vec![0_u8; 8192]);
    loop {
        let count = reader.read(&mut buffer[..]).await?;
        if count == 0 {
            return Ok(output);
        }
        let remaining = MAX_CLI_OUTPUT_BYTES.saturating_sub(output.bytes.len());
        output
            .bytes
            .extend_from_slice(&buffer[..count.min(remaining)]);
        output.exceeded_limit |= count > remaining;
    }
}

async fn drain_bounded<R>(mut reader: R) -> io::Result<bool>
where
    R: AsyncRead + Unpin,
{
    let mut total = 0_usize;
    let mut exceeded_limit = false;
    let mut buffer = Zeroizing::new(vec![0_u8; 8192]);
    loop {
        let count = reader.read(&mut buffer[..]).await?;
        if count == 0 {
            return Ok(exceeded_limit);
        }
        total = total.saturating_add(count);
        exceeded_limit |= total > MAX_CLI_OUTPUT_BYTES;
        buffer[..count].zeroize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;

    #[cfg(unix)]
    use std::io::Write;
    #[cfg(unix)]
    use std::path::PathBuf;
    #[cfg(unix)]
    use std::process::{Command as StdCommand, Stdio as StdStdio};

    #[cfg(unix)]
    fn executable_script(source: &str) -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("uqa");
        // Create fixtures in a child so parallel forks cannot inherit writable executable descriptors and cause Linux ETXTBSY.
        let mut writer = StdCommand::new("/bin/sh")
            .args(["-c", "cat > \"$1\" && chmod 700 \"$1\"", "uqa-test-fixture"])
            .arg(&path)
            .stdin(StdStdio::piped())
            .spawn()
            .unwrap();
        writer
            .stdin
            .take()
            .unwrap()
            .write_all(source.as_bytes())
            .unwrap();
        assert!(writer.wait().unwrap().success());
        (directory, path)
    }

    #[test]
    fn connection_arguments_are_unambiguous() {
        assert_eq!(
            local_arguments("notes"),
            ["local", "connection", "notes", "--format", "json"]
        );
        assert_eq!(
            cloud_arguments("analytics", Some("acme")),
            [
                "cloud",
                "connection",
                "analytics",
                "--format",
                "json",
                "--org",
                "acme",
            ]
        );
        assert_eq!(
            cloud_arguments("analytics", None),
            ["cloud", "connection", "analytics", "--format", "json"]
        );
    }

    #[test]
    fn project_and_organization_names_must_not_be_empty() {
        assert!(matches!(
            validate_project("  "),
            Err(HttpEngineError::EmptyProjectName)
        ));
    }

    #[test]
    fn cli_child_explicitly_removes_the_ambient_project_token() {
        let command = cli_command(OsStr::new("uqa"), local_arguments("notes"));
        let environments = command.as_std().get_envs().collect::<Vec<_>>();

        assert!(environments
            .iter()
            .any(|(name, value)| *name == OsStr::new("UQA_TOKEN") && value.is_none()));
    }

    #[test]
    fn parses_shared_local_and_cloud_connection_fields() {
        let mut local = CapturedOutput {
            bytes: br#"{"url":"http://127.0.0.1:8432/","token":"uqa_db_local"}"#.to_vec(),
            exceeded_limit: false,
        };
        let connection: ResolvedConnection = serde_json::from_slice(&local.bytes).unwrap();
        local.bytes.zeroize();
        assert_eq!(connection.url, "http://127.0.0.1:8432/");
        assert_eq!(connection.token.expose_secret(), "uqa_db_local");

        let cloud = br#"{"organization_id":"org_1","project_id":"prj_1","url":"https://example.com/","token":"uqa_db_cloud"}"#;
        let connection: ResolvedConnection = serde_json::from_slice(cloud).unwrap();
        assert_eq!(connection.url, "https://example.com/");
        assert_eq!(connection.token.expose_secret(), "uqa_db_cloud");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn invokes_cli_without_a_shell_and_parses_connection_json() {
        let (_directory, cli) = executable_script(
            "#!/bin/sh\n\
             test \"$1\" = local && test \"$2\" = connection && test \"$3\" = notes && \
             test \"$4\" = --format && test \"$5\" = json || exit 19\n\
             printf '%s\\n' '{\"url\":\"http://127.0.0.1:8432/\",\"token\":\"uqa_db_test\"}'\n",
        );

        let connection = resolve(
            cli.as_os_str(),
            local_arguments("notes"),
            Duration::from_secs(2),
        )
        .await
        .unwrap();
        assert_eq!(connection.url, "http://127.0.0.1:8432/");
        assert_eq!(connection.token.expose_secret(), "uqa_db_test");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cli_failure_does_not_expose_stderr() {
        let secret = "uqa_db_stderr-secret";
        let (_directory, cli) = executable_script(&format!(
            "#!/bin/sh\nprintf '%s\\n' '{secret}' >&2\nexit 23\n"
        ));

        let error = resolve(
            cli.as_os_str(),
            local_arguments("notes"),
            Duration::from_secs(2),
        )
        .await
        .err()
        .expect("CLI must fail");
        assert!(
            matches!(error, HttpEngineError::CLIConnectionFailed),
            "unexpected error: {error:?}"
        );
        assert!(!error.to_string().contains(secret));
        assert!(!format!("{error:?}").contains(secret));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cli_stdout_and_runtime_are_bounded() {
        let oversized = "x".repeat(MAX_CLI_OUTPUT_BYTES + 1);
        let (_directory, cli) =
            executable_script(&format!("#!/bin/sh\nprintf '%s' '{oversized}'\n"));
        let error = resolve(
            cli.as_os_str(),
            local_arguments("notes"),
            Duration::from_secs(2),
        )
        .await
        .err()
        .expect("oversized output must fail");
        assert!(
            matches!(error, HttpEngineError::CLIOutputTooLarge),
            "unexpected error: {error:?}"
        );

        let (_directory, cli) = executable_script("#!/bin/sh\nwhile :; do :; done\n");
        let error = resolve(
            cli.as_os_str(),
            local_arguments("notes"),
            Duration::from_millis(20),
        )
        .await
        .err()
        .expect("slow CLI must fail");
        assert!(matches!(error, HttpEngineError::CLITimedOut));
    }
}
