//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Type-name projection shares public parser/scanner observations across ordinary and retained work.

use super::{scanner_isspace, ParsedRegtypeName};
use crate::{catalog::work::Work, SQLError};
use pg_query::NodeEnum;
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{Produced, ProductionControl, ProductionVec},
};

#[cfg(test)]
mod tests;

/// Project one type name without consulting or changing the process cache. Public dependency parsing and scanning keep their existing atomic boundary; owned name copies and token visits consume the supplied work channel.
pub async fn parse_regtype_name_from_input(
    source: &str,
    control: &ProductionControl<'_>,
    input: &mut AsyncImageInput<'_>,
) -> Result<Option<Produced<ParsedRegtypeName>>, SQLError> {
    parse(source, control, input).await
}

async fn checkpoint(control: &ProductionControl<'_>, work: &mut impl Work) -> Result<(), SQLError> {
    control.check()?;
    work.checkpoint().await?;
    control.check()?;
    Ok(())
}

pub(super) async fn parse(
    source: &str,
    control: &ProductionControl<'_>,
    work: &mut impl Work,
) -> Result<Option<Produced<ParsedRegtypeName>>, SQLError> {
    checkpoint(control, work).await?;
    let mut empty = true;
    for chunk in source.as_bytes().chunks(4096) {
        checkpoint(control, work).await?;
        if !chunk.iter().copied().all(scanner_isspace) {
            empty = false;
            break;
        }
    }
    if empty {
        return Ok(None);
    }
    let parsed = crate::parser::parse_with_mode(source, pg_query::ParseMode::TypeName)?;
    control.check()?;
    let [raw] = parsed.protobuf.stmts.as_slice() else {
        return Ok(None);
    };
    let Some(NodeEnum::List(names)) = raw.stmt.as_ref().and_then(|node| node.node.as_ref()) else {
        return Ok(None);
    };
    let mut projected = ProductionVec::new(*control);
    projected.reserve(names.items.len())?;
    for name in &names.items {
        checkpoint(control, work).await?;
        let Some(NodeEnum::String(name)) = name.node.as_ref() else {
            return Err(SQLError::Internal("expected type-name string node".into()));
        };
        projected.push_produced(work.text(&name.sval, control).await?)?;
    }
    if projected.is_empty() {
        return Ok(None);
    }
    let scanned = crate::parser::scan(source)?;
    control.check()?;
    let mut brackets = 0;
    let mut array = false;
    let mut modifiers = false;
    for token in &scanned.tokens {
        checkpoint(control, work).await?;
        match pg_query::protobuf::Token::try_from(token.token).ok() {
            Some(pg_query::protobuf::Token::Setof) => return Ok(None),
            Some(pg_query::protobuf::Token::Ascii91) => brackets += 1,
            Some(pg_query::protobuf::Token::Array) => array = true,
            Some(pg_query::protobuf::Token::Ascii40) => modifiers = true,
            _ => {}
        }
    }
    let (names, memory) = projected.finish()?.into_parts();
    Ok(Some(control.finish(
        ParsedRegtypeName {
            names,
            array_dimensions: brackets.max(usize::from(array)),
            has_type_modifiers: modifiers,
        },
        memory,
    )?))
}
