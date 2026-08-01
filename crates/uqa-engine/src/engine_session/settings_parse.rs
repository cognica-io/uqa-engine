//! Runtime `search_path` parsing.

use super::SQLError;

pub(super) fn parse_search_path_list(value: &str) -> Result<Vec<String>, SQLError> {
    let chars = value.chars().collect::<Vec<_>>();
    let mut index = 0;
    let mut schemas = Vec::new();
    while index < chars.len() {
        while index < chars.len() && chars[index].is_whitespace() {
            index += 1;
        }
        if index == chars.len() {
            break;
        }
        let mut schema = String::new();
        if matches!(chars[index], '"' | '\'') {
            let quote = chars[index];
            index += 1;
            let mut terminated = false;
            while index < chars.len() {
                if chars[index] != quote {
                    schema.push(chars[index]);
                    index += 1;
                } else if chars.get(index + 1) == Some(&quote) {
                    schema.push(quote);
                    index += 2;
                } else {
                    index += 1;
                    terminated = true;
                    break;
                }
            }
            if !terminated {
                return Err(SQLError::TypeMismatch(format!(
                    "unterminated quoted schema in search_path `{value}`"
                )));
            }
        } else {
            while index < chars.len() && chars[index] != ',' {
                schema.push(chars[index]);
                index += 1;
            }
            schema = schema.trim().to_string();
        }
        while index < chars.len() && chars[index].is_whitespace() {
            index += 1;
        }
        if schema.is_empty() || (index < chars.len() && chars[index] != ',') {
            return Err(SQLError::TypeMismatch(format!(
                "invalid schema list in search_path `{value}`"
            )));
        }
        schemas.push(schema);
        if index < chars.len() {
            index += 1;
            if index == chars.len() {
                return Err(SQLError::TypeMismatch(format!(
                    "trailing comma in search_path `{value}`"
                )));
            }
        }
    }
    Ok(schemas)
}
