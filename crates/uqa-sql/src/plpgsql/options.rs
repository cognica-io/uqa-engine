//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The compile options of a `PL/pgSQL` body: what a body declares before its first block, and what a session's settings supply when it compiles the body.

/// How a name in an embedded statement that is both a `PL/pgSQL` variable and a column or relation the statement can see resolves, as `plpgsql.variable_conflict` and `#variable_conflict` choose.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VariableConflict {
    /// The statement fails as ambiguous, `PostgreSQL`'s default.
    #[default]
    Error,
    /// The variable takes the name.
    UseVariable,
    /// The column takes the name.
    UseColumn,
}

impl VariableConflict {
    /// The option a setting or a directive names: `error`, `use_variable` or `use_column`, in any case.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "error" => Some(Self::Error),
            "use_variable" => Some(Self::UseVariable),
            "use_column" => Some(Self::UseColumn),
            _ => None,
        }
    }
}

/// The options a body declares before its first block, which take precedence over the session's settings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CompileOptions {
    /// `#variable_conflict error | use_variable | use_column`.
    pub variable_conflict: Option<VariableConflict>,
    /// `#print_strict_params on | off`.
    pub print_strict_params: Option<bool>,
}

/// Read the options a body declares before its first block, as `pl_gram.y`'s `comp_options` does: each `#` starts an option, between whitespace and comments. The parser has accepted the body, so an option this reader meets is well formed.
#[must_use]
pub fn compile_options(body: &str) -> CompileOptions {
    let mut options = CompileOptions::default();
    let mut rest = skip_space_and_comments(body);
    while let Some(option) = rest.strip_prefix('#') {
        let (name, after_name) = read_word(skip_space_and_comments(option));
        let (value, after_value) = read_word(skip_space_and_comments(after_name));
        match name.to_ascii_lowercase().as_str() {
            "variable_conflict" => {
                options.variable_conflict = VariableConflict::from_name(value);
            }
            "print_strict_params" => {
                options.print_strict_params = match value.to_ascii_lowercase().as_str() {
                    "on" => Some(true),
                    "off" => Some(false),
                    _ => options.print_strict_params,
                };
            }
            // `#option dump` prints the compiled function in PostgreSQL's log.
            _ => {}
        }
        rest = skip_space_and_comments(after_value);
    }
    options
}

/// The text after any whitespace, `--` comments and nested `/* */` comments at its start.
fn skip_space_and_comments(text: &str) -> &str {
    let mut rest = text.trim_start();
    loop {
        if let Some(comment) = rest.strip_prefix("--") {
            rest = comment
                .find('\n')
                .map_or("", |end| &comment[end..])
                .trim_start();
        } else if rest.starts_with("/*") {
            rest = skip_block_comment(rest).trim_start();
        } else {
            return rest;
        }
    }
}

/// The text after the block comment at its start, which may contain nested block comments.
fn skip_block_comment(text: &str) -> &str {
    let mut depth = 0usize;
    let mut index = 0;
    let bytes = text.as_bytes();
    while index + 1 < bytes.len() {
        match (bytes[index], bytes[index + 1]) {
            (b'/', b'*') => {
                depth += 1;
                index += 2;
            }
            (b'*', b'/') => {
                depth -= 1;
                index += 2;
                if depth == 0 {
                    return &text[index..];
                }
            }
            _ => index += 1,
        }
    }
    ""
}

/// The identifier at the start of `text` and the text after it.
fn read_word(text: &str) -> (&str, &str) {
    let end = text
        .find(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .unwrap_or(text.len());
    text.split_at(end)
}

#[cfg(test)]
mod tests;
