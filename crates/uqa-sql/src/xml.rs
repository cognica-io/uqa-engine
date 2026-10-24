//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native XML input validation and declaration-aware result output.

use crate::SQLError;
use uqa_core::memory::{Produced, ProductionControl, ProductionString};

mod declaration;
#[allow(unsafe_code)]
mod parser;

/// SQL/XML input mode selected by the invoking session's `xmloption`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XmlOption {
    Content,
    Document,
}

/// Validate a UTF-8 XML datum without rewriting its stored spelling. Parser contexts own their error and external-resource handlers; no process-global callbacks are replaced.
pub fn validate(input: &str, option: XmlOption) -> Result<(), SQLError> {
    validate_with_notices(input, option, |_| {})
}

/// Validate with the invoking operation's notice sink. Notices are forwarded after the native parser has released its context, including when input ultimately fails.
pub fn validate_with_notices(
    input: &str,
    option: XmlOption,
    notice: impl FnMut(crate::SQLNotice),
) -> Result<(), SQLError> {
    let declaration = if option == XmlOption::Content {
        declaration::parse(input).map_err(|detail| error(option, true, Some(detail.into())))?
    } else {
        declaration::Declaration::default()
    };
    let body = &input[declaration.length..];
    let document = option == XmlOption::Document || declaration::has_doctype(body);
    parser::validate(
        if document { input } else { body },
        document,
        declaration.version.unwrap_or("1.0"),
        notice,
    )
    .map_err(|failure| match failure {
        parser::Failure::Invalid(detail) => error(option, false, detail),
        parser::Failure::Memory => SQLError::Routine {
            sqlstate: "53200".into(),
            message: "out of memory while parsing XML".into(),
        },
        parser::Failure::Limit => SQLError::Routine {
            sqlstate: "54000".into(),
            message: "XML input is too large".into(),
        },
    })
}

fn error(option: XmlOption, declaration: bool, detail: Option<String>) -> SQLError {
    let (sqlstate, message) = match (option, declaration) {
        (XmlOption::Content, true) => ("2200N", "invalid XML content: invalid XML declaration"),
        (XmlOption::Content, false) => ("2200N", "invalid XML content"),
        (XmlOption::Document, _) => ("2200M", "invalid XML document"),
    };
    SQLError::Diagnostic {
        sqlstate: sqlstate.into(),
        message: message.into(),
        detail,
        hint: None,
    }
}

/// Serialize an admitted XML value for UTF-8 text results. XML-to-text casts retain the stored bytes instead; serialization removes encoding and an unnecessary default declaration.
pub fn output_with_control(
    input: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, SQLError> {
    control.check()?;
    let declaration = declaration::parse(input).map_err(|detail| {
        SQLError::Internal(format!("invalid stored XML declaration: {detail}"))
    })?;
    let mut output = ProductionString::new(*control);
    let version = declaration.version.unwrap_or("1.0");
    let mut body = &input[declaration.length..];
    if version != "1.0" || declaration.standalone.is_some() {
        output.push_str("<?xml version=\"")?;
        output.push_str(version)?;
        output.push_str("\"")?;
        if let Some(standalone) = declaration.standalone {
            output.push_str(if standalone {
                " standalone=\"yes\""
            } else {
                " standalone=\"no\""
            })?;
        }
        output.push_str("?>")?;
    } else {
        body = body.strip_prefix('\n').unwrap_or(body);
    }
    output.push_str(body)?;
    Ok(output.finish()?)
}

#[cfg(test)]
mod tests;
