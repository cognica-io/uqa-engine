//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::collections::BTreeMap;

use crate::lexer::{tokenize_sql, TokenKind};

/// Shell values are explicit user input. SQL quoting never expands inside a SQL literal.
#[derive(Default)]
pub(crate) struct Variables {
    values: BTreeMap<String, String>,
    conditions: Vec<Conditional>,
    condition_floors: Vec<usize>,
}

struct Conditional {
    parent: bool,
    matched: bool,
    active: bool,
    branch: ConditionalBranch,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ConditionalBranch {
    Condition,
    Else,
}

impl Variables {
    pub(crate) fn set(&mut self, key: &str, value: String) -> Result<(), String> {
        if !valid_key(key) || value.contains('\0') {
            return Err("invalid shell command argument".to_owned());
        }
        if matches!(key, "ON_ERROR_STOP" | "AUTOCOMMIT" | "HIGHLIGHT") {
            parse_boolean(&value)?;
        }
        if key == "HISTCONTROL"
            && !matches!(
                value.as_str(),
                "none" | "ignorespace" | "ignoredups" | "ignoreboth"
            )
        {
            return Err("invalid shell command argument".to_owned());
        }
        if key == "HISTSIZE" && !value.parse::<usize>().is_ok_and(|n| n <= 100_000) {
            return Err("invalid shell command argument".to_owned());
        }
        self.values.insert(key.into(), value);
        Ok(())
    }
    pub(crate) fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }
    pub(crate) fn entries(&self) -> impl Iterator<Item = (&String, &String)> {
        self.values.iter()
    }
    pub(crate) fn unset(&mut self, key: &str) -> Result<(), String> {
        if !valid_key(key) {
            return Err("invalid shell command argument".to_owned());
        }
        self.values.remove(key);
        Ok(())
    }
    pub(crate) fn flag(&self, key: &str, default: bool) -> bool {
        self.get(key)
            .and_then(|value| parse_boolean(value).ok())
            .unwrap_or(default)
    }
    pub(crate) fn active(&self) -> bool {
        self.conditions.last().is_none_or(|frame| frame.active)
    }
    pub(crate) fn depth(&self) -> usize {
        self.conditions.len()
    }

    pub(crate) fn begin_script(&mut self) -> usize {
        let depth = self.depth();
        self.condition_floors.push(depth);
        depth
    }

    pub(crate) fn end_script(&mut self) {
        if let Some(depth) = self.condition_floors.pop() {
            self.conditions.truncate(depth);
        }
    }

    pub(crate) fn conditional(&mut self, command: &str, argument: &str) -> Result<bool, String> {
        if matches!(command, "elif" | "else" | "endif")
            && self
                .condition_floors
                .last()
                .is_some_and(|floor| self.depth() <= *floor)
        {
            return Err("conditional cannot cross a script boundary".into());
        }
        match command {
            "if" => {
                if self.conditions.len() >= 64 {
                    return Err("invalid shell command argument".to_owned());
                }
                let parent = self.active();
                self.conditions.push(Conditional {
                    parent,
                    matched: false,
                    active: false,
                    branch: ConditionalBranch::Condition,
                });
                // Retain an inactive frame even if expression validation fails.
                // A script that continues after an error must never execute that branch.
                let active = parent && self.boolean_argument(argument)?;
                let frame = self.conditions.last_mut().expect("inserted conditional");
                frame.active = active;
                frame.matched = active;
            }
            "elif" => {
                let value = self.expand(argument);
                let frame = self
                    .conditions
                    .last_mut()
                    .ok_or("invalid shell command argument".to_owned())?;
                if frame.branch == ConditionalBranch::Else {
                    return Err("invalid shell command argument".to_owned());
                }
                frame.active = false;
                frame.active = frame.parent && !frame.matched && boolean_argument(&value)?;
                frame.matched |= frame.active;
            }
            "else" => {
                if !argument.is_empty() {
                    return Err("invalid shell command argument".to_owned());
                }
                let frame = self
                    .conditions
                    .last_mut()
                    .ok_or("invalid shell command argument".to_owned())?;
                if frame.branch == ConditionalBranch::Else {
                    return Err("invalid shell command argument".to_owned());
                }
                frame.branch = ConditionalBranch::Else;
                frame.active = frame.parent && !frame.matched;
                frame.matched = true;
            }
            "endif" => {
                if !argument.is_empty() || self.conditions.pop().is_none() {
                    return Err("invalid shell command argument".to_owned());
                }
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    pub(crate) fn expand(&self, source: &str) -> String {
        let mut result = String::with_capacity(source.len());
        for token in tokenize_sql(source) {
            let text = token.text(source);
            if token.kind != TokenKind::Variable || !token.closed {
                result.push_str(text);
                continue;
            }
            let name = &text[1..];
            if let Some(key) = name.strip_prefix("{?").and_then(|s| s.strip_suffix('}')) {
                result.push_str(if self.get(key).is_some() {
                    "TRUE"
                } else {
                    "FALSE"
                });
                continue;
            }
            let quoted = name.starts_with(['\'', '"']);
            let key = if quoted {
                &name[1..name.len() - 1]
            } else {
                name
            };
            let Some(value) = self.get(key) else {
                result.push_str(text);
                continue;
            };
            match name.as_bytes()[0] {
                b'\'' => {
                    if value.contains('\\') {
                        result.push('E');
                    }
                    result.push('\'');
                    result.push_str(&value.replace('\\', "\\\\").replace('\'', "''"));
                    result.push('\'');
                }
                b'"' => {
                    result.push('"');
                    result.push_str(&value.replace('"', "\"\""));
                    result.push('"');
                }
                _ => result.push_str(value),
            }
        }
        result
    }

    fn boolean_argument(&self, argument: &str) -> Result<bool, String> {
        boolean_argument(&self.expand(argument))
    }
}

fn boolean_argument(argument: &str) -> Result<bool, String> {
    let values = meta_arguments(argument)?;
    if values.len() != 1 {
        return Err("invalid shell command argument".to_owned());
    }
    parse_boolean(&values[0])
}

impl std::fmt::Debug for Variables {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Variables")
            .field("count", &self.values.len())
            .finish_non_exhaustive()
    }
}
pub(crate) fn parse_boolean(value: &str) -> Result<bool, String> {
    let value = value.trim().to_ascii_lowercase();
    if value == "1" {
        return Ok(true);
    }
    if value == "0" {
        return Ok(false);
    }
    if value.is_empty() {
        return Err("Boolean expected".into());
    }
    let yes = ["true", "yes", "on"]
        .iter()
        .any(|word| word.starts_with(&value));
    let no = ["false", "no", "off"]
        .iter()
        .any(|word| word.starts_with(&value));
    match (yes, no) {
        (true, false) => Ok(true),
        (false, true) => Ok(false),
        _ => Err(format!("unrecognized Boolean value: {value}")),
    }
}

fn valid_key(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// Meta-command argument quoting is lexical only; it never executes substitutions or a shell.
pub(crate) fn meta_arguments(source: &str) -> Result<Vec<String>, String> {
    let mut result = Vec::new();
    let mut value = String::new();
    let mut quote = None;
    let mut started = false;
    let mut escaped = false;
    for character in source.chars() {
        if escaped {
            value.push(character);
            escaped = false;
            started = true;
            continue;
        }
        if character == '\\' && quote != Some('\'') {
            escaped = true;
            started = true;
            continue;
        }
        if let Some(current) = quote {
            if character == current {
                quote = None;
            } else {
                value.push(character);
            }
        } else if matches!(character, '\'' | '"') {
            quote = Some(character);
            started = true;
        } else if character.is_whitespace() {
            if started {
                result.push(std::mem::take(&mut value));
                started = false;
            }
        } else {
            value.push(character);
            started = true;
        }
    }
    if escaped || quote.is_some() {
        return Err("invalid shell command argument".to_owned());
    }
    if started {
        result.push(value);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_expressions_leave_their_branch_inactive() {
        let mut v = Variables::default();
        assert!(v.conditional("if", "invalid").is_err());
        assert!(!v.active());
        v.conditional("elif", "'true'").unwrap();
        assert!(v.active());
        assert!(v.conditional("elif", "invalid").is_ok());
        assert!(!v.active());
        v.conditional("endif", "").unwrap();
        v.set("present", String::new()).unwrap();
        assert_eq!(
            v.expand("SELECT :{?present}, :{?absent}, ': { ? }'"),
            "SELECT TRUE, FALSE, ': { ? }'"
        );
        v.conditional("if", ":{?present}").unwrap();
        assert!(v.active());
    }
    #[test]
    fn substitutions_preserve_quoted_sql_comments_and_casts() {
        let mut variables = Variables::default();
        variables.set("value", "O'Reilly".into()).unwrap();
        variables.set("column", "a\"b".into()).unwrap();
        assert_eq!(
            variables
                .expand("SELECT :'value', :\"column\", ':value', $$:value$$, 1::int -- :value"),
            "SELECT 'O''Reilly', \"a\"\"b\", ':value', $$:value$$, 1::int -- :value"
        );
        assert_eq!(variables.expand("SELECT :missing"), "SELECT :missing");
        assert!(!format!("{variables:?}").contains("Reilly"));
    }
    #[test]
    fn skipped_nested_branches_never_enable_a_command() {
        let mut v = Variables::default();
        assert!(v.conditional("if", "false").unwrap());
        assert!(!v.active());
        v.conditional("if", "garbage").unwrap();
        assert!(!v.active());
        v.conditional("else", "").unwrap();
        assert!(!v.active());
        v.conditional("endif", "").unwrap();
        v.conditional("elif", "true").unwrap();
        assert!(v.active());
        v.conditional("else", "").unwrap();
        assert!(!v.active());
        assert!(v.conditional("else", "").is_err());
        v.conditional("endif", "").unwrap();
        assert!(v.active());
        assert!(v.conditional("endif", "").is_err());
    }
    #[test]
    fn quoted_empty_arguments_and_spaces_are_preserved_without_evaluation() {
        assert_eq!(
            meta_arguments("a 'b c' \"\" '$(whoami)' \"x\\\"y\"").unwrap(),
            ["a", "b c", "", "$(whoami)", "x\"y"]
        );
        assert!(meta_arguments("'unfinished").is_err());
    }

    #[test]
    fn included_scripts_cannot_modify_the_callers_conditional_frame() {
        let mut variables = Variables::default();
        variables.conditional("if", "true").unwrap();
        assert_eq!(variables.begin_script(), 1);
        for command in ["else", "elif", "endif"] {
            assert!(variables.conditional(command, "").is_err());
            assert!(variables.active());
            assert_eq!(variables.depth(), 1);
        }
        variables.conditional("if", "false").unwrap();
        variables.end_script();
        assert!(variables.active());
        variables.conditional("endif", "").unwrap();
        assert_eq!(variables.depth(), 0);
    }
}
