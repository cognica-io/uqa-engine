//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The field syntax of `record_in`: parentheses around comma-separated fields, where an empty field is NULL, double quotes protect separators and whitespace, a doubled quote inside quotes is one quote, and a backslash takes the next character literally.

use super::{Result, SQLError};

fn malformed(text: &str, detail: &str) -> SQLError {
    SQLError::Diagnostic {
        sqlstate: "22P02".into(),
        message: format!("malformed record literal: \"{text}\""),
        detail: Some(detail.into()),
        hint: None,
    }
}

/// Split `text` into `columns` fields as `record_in` does, handing each field to `field` as soon as it is read: `None` is a NULL field. Leading and trailing whitespace around the parentheses is allowed; whitespace inside an unquoted field belongs to the field.
pub fn parse_record_fields(
    text: &str,
    columns: usize,
    mut field: impl FnMut(usize, Option<String>) -> Result<()>,
) -> Result<()> {
    let bytes = text.as_bytes();
    let mut position = 0;
    while position < bytes.len() && bytes[position].is_ascii_whitespace() {
        position += 1;
    }
    if bytes.get(position) != Some(&b'(') {
        return Err(malformed(text, "Missing left parenthesis."));
    }
    position += 1;
    for column in 0..columns {
        if column > 0 {
            if bytes.get(position) == Some(&b',') {
                position += 1;
            } else {
                return Err(malformed(text, "Too few columns."));
            }
        }
        if matches!(bytes.get(position), Some(b',' | b')')) {
            field(column, None)?;
            continue;
        }
        let mut data = Vec::new();
        let mut quoted = false;
        loop {
            let Some(&byte) = bytes.get(position) else {
                return Err(malformed(text, "Unexpected end of input."));
            };
            if !quoted && matches!(byte, b',' | b')') {
                break;
            }
            position += 1;
            match byte {
                b'\\' => {
                    let Some(&escaped) = bytes.get(position) else {
                        return Err(malformed(text, "Unexpected end of input."));
                    };
                    data.push(escaped);
                    position += 1;
                }
                b'"' if !quoted => quoted = true,
                b'"' if bytes.get(position) == Some(&b'"') => {
                    data.push(b'"');
                    position += 1;
                }
                b'"' => quoted = false,
                other => data.push(other),
            }
        }
        let data = String::from_utf8(data)
            .map_err(|_| SQLError::Internal("record field split a UTF-8 character".into()))?;
        field(column, Some(data))?;
    }
    if bytes.get(position) != Some(&b')') {
        return Err(malformed(text, "Too many columns."));
    }
    position += 1;
    while position < bytes.len() && bytes[position].is_ascii_whitespace() {
        position += 1;
    }
    if position < bytes.len() {
        return Err(malformed(text, "Junk after right parenthesis."));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parse_record_fields;

    fn fields(text: &str, columns: usize) -> Result<Vec<Option<String>>, String> {
        let mut fields = Vec::new();
        parse_record_fields(text, columns, |_, field| {
            fields.push(field);
            Ok(())
        })
        .map_err(|error| match error {
            crate::SQLError::Diagnostic { detail, .. } => detail.unwrap_or_default(),
            other => other.to_string(),
        })?;
        Ok(fields)
    }

    #[test]
    fn fields_follow_record_in_quoting() {
        assert_eq!(
            fields(r#" (1,"a ""quoted"", name","{x,""y z""}") "#, 3).unwrap(),
            vec![
                Some("1".into()),
                Some(r#"a "quoted", name"#.into()),
                Some(r#"{x,"y z"}"#.into())
            ]
        );
        assert_eq!(
            fields("(,\"\",)", 3).unwrap(),
            vec![None, Some(String::new()), None]
        );
        assert_eq!(
            fields(r"(b\\c, x )", 2).unwrap(),
            vec![Some(r"b\c".into()), Some(" x ".into())]
        );
        assert_eq!(fields("()", 0).unwrap(), Vec::<Option<String>>::new());
        assert_eq!(fields("()", 1).unwrap(), vec![None]);
    }

    #[test]
    fn malformed_literals_report_record_in_details() {
        for (text, columns, detail) in [
            ("1,2)", 2, "Missing left parenthesis."),
            ("(1)", 2, "Too few columns."),
            ("(1,2,3)", 2, "Too many columns."),
            ("(1,2) x", 2, "Junk after right parenthesis."),
            ("(1,\"2)", 2, "Unexpected end of input."),
            ("(1,2\\", 2, "Unexpected end of input."),
            ("(1", 1, "Unexpected end of input."),
        ] {
            assert_eq!(fields(text, columns).unwrap_err(), detail, "{text}");
        }
    }
}
