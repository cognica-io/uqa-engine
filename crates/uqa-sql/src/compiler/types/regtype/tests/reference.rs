//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Unchanged type-name projection from 15b534ad17f241009bcce85d1f0b0d12a97eec75; only the test entry-point name and imports differ.

use super::super::{scanner_isspace, ParsedRegtypeName};
use crate::error::{Result, SQLError};
use pg_query::NodeEnum;
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

pub(super) fn parse(
    input: &str,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ParsedRegtypeName>>> {
    control.check()?;
    if input.bytes().all(scanner_isspace) {
        return Ok(None);
    }
    let parsed = crate::parser::parse_with_mode(input, pg_query::ParseMode::TypeName)?;
    let [raw] = parsed.protobuf.stmts.as_slice() else {
        return Ok(None);
    };
    let Some(NodeEnum::List(names)) = raw.stmt.as_ref().and_then(|node| node.node.as_ref()) else {
        return Ok(None);
    };
    let mut projected_names = ProductionVec::new(*control);
    projected_names.reserve(names.items.len())?;
    for name in &names.items {
        let Some(NodeEnum::String(name)) = name.node.as_ref() else {
            return Err(SQLError::Internal("expected type-name string node".into()));
        };
        projected_names.push_produced(control.copy_text(&name.sval)?)?;
    }
    if projected_names.is_empty() {
        return Ok(None);
    }
    let scanned = crate::parser::scan(input)?;
    let mut bracket_dimensions = 0;
    let mut array = false;
    let mut has_type_modifiers = false;
    for token in &scanned.tokens {
        control.check()?;
        match pg_query::protobuf::Token::try_from(token.token).ok() {
            Some(pg_query::protobuf::Token::Setof) => return Ok(None),
            Some(pg_query::protobuf::Token::Ascii91) => bracket_dimensions += 1,
            Some(pg_query::protobuf::Token::Array) => array = true,
            Some(pg_query::protobuf::Token::Ascii40) => has_type_modifiers = true,
            _ => {}
        }
    }
    let array_dimensions = bracket_dimensions.max(usize::from(array));
    let (names, memory) = projected_names.finish()?.into_parts();
    Ok(Some(control.finish(
        ParsedRegtypeName {
            names,
            array_dimensions,
            has_type_modifiers,
        },
        memory,
    )?))
}
