//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! File COPY delegates stream encoding, schema validation and mutation to Engine.

use super::{io, Path, PathBuf, Session, Write};
use crate::lexer::{tokenize_sql, TokenKind};

impl Session {
    pub(super) fn copy_file(&mut self, argument: &str, out: &mut impl Write) -> Result<(), String> {
        let (sql, from, path) = parse_copy(argument)?;
        let _interrupt = crate::interrupt::QueryInterrupt::new(self.engine.cancellation_token())?;
        self.prepare_statement_transaction(&sql)?;
        let count = if from {
            let file =
                std::fs::File::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            self.engine
                .copy_from(&sql, io::BufReader::new(file))
                .map_err(|e| crate::diagnostics::sql_error_text(&e))?
        } else {
            let parent = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            let mut temporary =
                tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
            let count = self
                .engine
                .copy_to(&sql, temporary.as_file_mut())
                .map_err(|e| crate::diagnostics::sql_error_text(&e))?;
            temporary.flush().map_err(|e| e.to_string())?;
            temporary
                .persist(&path)
                .map_err(|e| format!("{}: {e}", path.display()))?;
            count
        };
        writeln!(out, "COPY {count}").map_err(|e| e.to_string())
    }
}

fn parse_copy(argument: &str) -> Result<(String, bool, PathBuf), String> {
    let tokens = tokenize_sql(argument);
    let mut depth = 0usize;
    for token in &tokens {
        let text = token.text(argument);
        if token.kind == TokenKind::Operator {
            match text {
                "(" => depth += 1,
                ")" => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
        if depth != 0 || token.kind != TokenKind::Word {
            continue;
        }
        let from = text.eq_ignore_ascii_case("FROM");
        if !from && !text.eq_ignore_ascii_case("TO") {
            continue;
        }
        let tail = argument[token.span.end..].trim_start();
        let first = tokenize_sql(tail)
            .into_iter()
            .next()
            .ok_or("COPY requires a filename")?;
        let (path, end) = if matches!(first.kind, TokenKind::String | TokenKind::Identifier) {
            if !first.closed {
                return Err("unterminated COPY filename".into());
            }
            let quoted = first.text(tail);
            if !quoted.starts_with(['\'', '"']) {
                return Err("COPY filename must use ordinary quotes".into());
            }
            let quote = &quoted[..1];
            (
                quoted[1..quoted.len() - 1].replace(&quote.repeat(2), quote),
                first.span.end,
            )
        } else {
            let end = tail.find(char::is_whitespace).unwrap_or(tail.len());
            let path = tail[..end].trim_end_matches(';');
            if ["PROGRAM", "STDIN", "STDOUT", "PSTDIN", "PSTDOUT"]
                .iter()
                .any(|word| path.eq_ignore_ascii_case(word))
            {
                return Err("\\copy requires a local filename".into());
            }
            (path.to_owned(), end)
        };
        if path.is_empty() || path.contains('\0') {
            return Err("invalid COPY filename".into());
        }
        let sql = format!(
            "COPY {} {} {}{}",
            argument[..token.span.start].trim(),
            if from { "FROM" } else { "TO" },
            if from { "STDIN" } else { "STDOUT" },
            &tail[end..]
        );
        return Ok((sql, from, PathBuf::from(path)));
    }
    Err("usage: \\copy relation [(columns)] FROM|TO 'file' [WITH (options)]".into())
}
