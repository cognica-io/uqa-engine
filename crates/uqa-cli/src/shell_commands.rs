//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shell presentation settings and explicit filesystem/process commands.

use super::{
    contains_input_terminator, io, IsTerminal, OpenOptions, Path, PathBuf, Session, Write,
};
use crate::shell::{no_args, single_arg};
use crate::variables::meta_arguments;
use std::io::Read;

impl Session {
    pub(super) fn set_printing(
        &mut self,
        name: &str,
        args: &[String],
        out: &mut impl Write,
    ) -> Result<(), String> {
        let options = &mut self.shell.printing;
        if name == "pset" && args.is_empty() {
            for key in [
                "format",
                "expanded",
                "border",
                "columns",
                "tuples_only",
                "footer",
                "pager",
                "null",
                "fieldsep",
                "recordsep",
                "title",
                "csv_fieldsep",
            ] {
                let value = options.value(key)?;
                writeln!(out, "{key} = '{}'", crate::printing::terminal_text(&value))
                    .map_err(|e| e.to_string())?;
            }
            return Ok(());
        }
        let (option, value) = printing_argument(name, args, options)?;
        if name == "pset"
            && args.len() == 1
            && !matches!(
                option.as_str(),
                "expanded"
                    | "tuples_only"
                    | "footer"
                    | "pager"
                    | "fieldsep_zero"
                    | "recordsep_zero"
            )
        {
            let current = options.value(&option)?;
            writeln!(
                out,
                "{option} = {}",
                crate::printing::terminal_text(&current)
            )
            .map_err(|e| e.to_string())?;
            return Ok(());
        }
        options.set(&option, &value)?;
        let message = if option == "expanded" {
            format!("Expanded display is {}.", options.expanded)
        } else {
            format!("{option} = {}", crate::printing::terminal_text(&value))
        };
        writeln!(out, "{message}").map_err(|e| e.to_string())
    }

    pub(super) fn file_command(
        &mut self,
        name: &str,
        args: &[String],
        buffer: &mut String,
        out: &mut impl Write,
    ) -> Result<(), String> {
        if name == "s" {
            if args.is_empty() {
                for line in &self.history {
                    writeln!(out, "{}", crate::printing::terminal_text(line))
                        .map_err(|e| e.to_string())?;
                }
            } else {
                write_private(
                    Path::new(single_arg(args)?),
                    &(self.history.join("\n") + "\n"),
                )?;
            }
            return Ok(());
        }
        if name == "w" {
            return write_private(
                Path::new(single_arg(args)?),
                if buffer.trim().is_empty() {
                    &self.shell.last_query
                } else {
                    buffer
                },
            );
        }
        if args.len() > 2 {
            return Err("usage: \\e [file [line]]".into());
        }
        let mut temporary = None;
        let path = if let Some(path) = args.first() {
            PathBuf::from(path)
        } else {
            let mut file = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
            file.write_all(if buffer.trim().is_empty() {
                self.shell.last_query.as_bytes()
            } else {
                buffer.as_bytes()
            })
            .map_err(|e| e.to_string())?;
            let path = file.path().to_owned();
            temporary = Some(file);
            path
        };
        let editor = std::env::var("PSQL_EDITOR")
            .or_else(|_| std::env::var("EDITOR"))
            .or_else(|_| std::env::var("VISUAL"))
            .unwrap_or_else(|_| {
                if cfg!(windows) {
                    "notepad".into()
                } else {
                    "vi".into()
                }
            });
        let mut command = process_command(&editor)?;
        if let Some(line) = args.get(1) {
            let line = line
                .parse::<usize>()
                .map_err(|_| "invalid editor line number")?;
            command.arg(format!("+{line}"));
        }
        let status = command
            .arg(&path)
            .status()
            .map_err(|e| format!("editor: {e}"))?;
        if !status.success() {
            return Err(format!("editor exited with {status}"));
        }
        let mut edited = String::new();
        std::fs::File::open(&path)
            .map_err(|e| e.to_string())?
            .take(16 * 1024 * 1024 + 1)
            .read_to_string(&mut edited)
            .map_err(|e| e.to_string())?;
        if edited.len() > 16 * 1024 * 1024 {
            return Err("query buffer exceeds 16 MiB".into());
        }
        *buffer = edited;
        drop(temporary);
        if contains_input_terminator(buffer) {
            let query = self.shell.variables.expand(&std::mem::take(buffer));
            self.shell.last_query.clone_from(&query);
            self.execute_text(&query, out)?;
        }
        Ok(())
    }

    pub(super) fn print_rendered(&self, text: &str, out: &mut impl Write) -> Result<(), String> {
        let rows = std::env::var("LINES")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(24);
        let use_pager = io::stdout().is_terminal()
            && io::stdin().is_terminal()
            && self.output_path.is_none()
            && (self.shell.printing.pager == "always"
                || (self.shell.printing.pager == "on" && text.lines().count() >= rows));
        if use_pager {
            let pager = std::env::var("PSQL_PAGER")
                .or_else(|_| std::env::var("PAGER"))
                .unwrap_or_else(|_| "more".into());
            if !pager.trim().is_empty() {
                let mut child = process_command(&pager)?
                    .stdin(std::process::Stdio::piped())
                    .spawn()
                    .map_err(|e| format!("pager: {e}"))?;
                let written = child
                    .stdin
                    .take()
                    .ok_or("pager has no input")?
                    .write_all(text.as_bytes());
                let status = child.wait().map_err(|e| format!("pager: {e}"))?;
                if let Err(error) = written {
                    if error.kind() != io::ErrorKind::BrokenPipe {
                        return Err(error.to_string());
                    }
                }
                if !status.success() {
                    return Err(format!("pager exited with {status}"));
                }
                return Ok(());
            }
        }
        self.write_query_output(out, |writer| {
            let _ = write!(writer, "{text}");
        })
    }

    pub(super) fn watch_query(
        &mut self,
        args: &[String],
        buffer: &mut String,
        out: &mut impl Write,
    ) -> Result<(), String> {
        let mut interval = 2.0;
        let mut count = None;
        let mut min_rows = None;
        for (index, arg) in args.iter().enumerate() {
            if let Some(value) = arg.strip_prefix("i=") {
                interval = value.parse::<f64>().map_err(|_| "invalid watch interval")?;
            } else if let Some(value) = arg.strip_prefix("c=") {
                count = Some(value.parse::<usize>().map_err(|_| "invalid watch count")?);
            } else if let Some(value) = arg.strip_prefix("m=") {
                min_rows = Some(
                    value
                        .parse::<usize>()
                        .map_err(|_| "invalid watch minimum rows")?,
                );
            } else if index == 0 {
                interval = arg.parse::<f64>().map_err(|_| "invalid watch interval")?;
            } else {
                return Err("usage: \\watch [seconds] [i=seconds] [c=count] [m=rows]".into());
            }
        }
        if !interval.is_finite() || !(0.001..=86400.0).contains(&interval) || count == Some(0) {
            return Err("invalid watch interval or count".into());
        }
        let query = if buffer.trim().is_empty() {
            self.shell.last_query.clone()
        } else {
            std::mem::take(buffer)
        };
        if query.trim().is_empty() {
            return Err("query buffer is empty".into());
        }
        let query = self.shell.variables.expand(&query);
        self.shell.last_query.clone_from(&query);
        let generation = crate::interrupt::generation();
        let mut executed = 0;
        loop {
            let result = self.capture_query(&query, out)?;
            let rows = result.rows.len();
            self.render_result(&result, out)?;
            executed += 1;
            if count.is_some_and(|count| executed >= count)
                || min_rows.is_some_and(|min| rows < min)
            {
                break;
            }
            if crate::interrupt::wait(std::time::Duration::from_secs_f64(interval), generation) {
                break;
            }
        }
        Ok(())
    }
}

fn process_command(source: &str) -> Result<std::process::Command, String> {
    let args = meta_arguments(source)?;
    let program = args.first().ok_or("empty process command")?;
    let mut command = std::process::Command::new(program);
    command.args(&args[1..]);
    Ok(command)
}
fn write_private(path: &Path, text: &str) -> Result<(), String> {
    let mut options = OpenOptions::new();
    options.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .and_then(|mut file| file.write_all(text.as_bytes()))
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn printing_argument(
    name: &str,
    args: &[String],
    options: &crate::printing::PrintOptions,
) -> Result<(String, String), String> {
    Ok(match name {
        "pset" => {
            if args.len() > 2 {
                return Err("usage: \\pset [option [value]]".into());
            }
            (args[0].clone(), args.get(1).cloned().unwrap_or_default())
        }
        "x" | "expanded" => {
            if args.len() > 1 {
                return Err("usage: \\x [on|off|auto]".into());
            }
            (
                "expanded".into(),
                args.first().cloned().unwrap_or_else(|| {
                    if options.expanded == "off" {
                        "on".into()
                    } else {
                        "off".into()
                    }
                }),
            )
        }
        "a" | "H" => {
            no_args(args)?;
            let target = if name == "a" { "unaligned" } else { "html" };
            (
                "format".into(),
                if options.format == target {
                    "aligned".into()
                } else {
                    target.into()
                },
            )
        }
        "t" => {
            if args.len() > 1 {
                return Err("usage: \\t [on|off]".into());
            }
            (
                "tuples_only".into(),
                args.first().cloned().unwrap_or_default(),
            )
        }
        "f" | "C" => {
            if args.len() > 1 {
                return Err("expected at most one argument".into());
            }
            (
                if name == "f" {
                    "fieldsep".into()
                } else {
                    "title".into()
                },
                args.first().cloned().unwrap_or_default(),
            )
        }
        _ => return Err("invalid printing command".into()),
    })
}
