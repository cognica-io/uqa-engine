//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordinary and retained signature names share every normalization step.

use super::{Produced, ProductionControl, ValueRetentionError};
use std::future::Future;
use uqa_core::{image_io::AsyncImageInput, memory::ProductionString};

/// Native admission failures and work-channel failures retain their original typed causes.
#[derive(Debug, thiserror::Error)]
pub enum RoutineSignatureInputError {
    #[error(transparent)]
    Native(#[from] ValueRetentionError),
    #[error(transparent)]
    Work(#[from] uqa_core::image_io::ImageIoError),
}

pub(super) trait Work: Send {
    type Error: From<ValueRetentionError>;
    fn checkpoint(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send;
}

impl Work for () {
    type Error = ValueRetentionError;
    async fn checkpoint(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl Work for AsyncImageInput<'_> {
    type Error = RoutineSignatureInputError;
    async fn checkpoint(&mut self) -> Result<(), Self::Error> {
        Ok(AsyncImageInput::checkpoint(self).await?)
    }
}

async fn checkpoint<W: Work>(
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<(), W::Error> {
    control.check()?;
    work.checkpoint().await?;
    control.check()?;
    Ok(())
}

pub(super) async fn normalize<W: Work>(
    type_name: &str,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<Produced<String>, W::Error> {
    checkpoint(control, work).await?;
    let mut compact = ProductionString::new(*control);
    let mut pending_space = false;
    for (index, character) in type_name.chars().enumerate() {
        if index % 1024 == 0 {
            checkpoint(control, work).await?;
        }
        push_compacted(
            &mut compact,
            character.to_ascii_lowercase(),
            &mut pending_space,
        )?;
    }
    let compact = compact.finish()?;
    let element = array_element_spelling(&compact, control, work).await?;
    // Compaction is idempotent, and removing array suffixes leaves the compacted prefix. The former recursive normalization therefore has this same scalar input without another copy.
    let scalar = element.unwrap_or(&compact);
    let without_catalog = scalar
        .strip_prefix("pg_catalog.")
        .or_else(|| scalar.strip_prefix("\"pg_catalog\"."))
        .unwrap_or(scalar);
    let base = strip_type_modifiers(without_catalog, control, work).await?;
    let base = if scalar.starts_with("pg_catalog.") || scalar.starts_with("\"pg_catalog\".") {
        match base
            .strip_prefix('"')
            .and_then(|name| name.strip_suffix('"'))
            .and_then(crate::catalog::type_metadata::builtin_scalar_type_by_name)
        {
            Some(ty) => ty.regtype_name_with_control(control)?,
            None => base,
        }
    } else {
        base
    };
    let canonical = match base.as_str() {
        "smallint" | "int2" => Some("int2"),
        "integer" | "int" | "int4" | "serial" | "serial4" => Some("int4"),
        "bigint" | "int8" | "bigserial" | "serial8" => Some("int8"),
        "real" | "float4" => Some("float4"),
        "double" | "double precision" | "float8" => Some("float8"),
        "decimal" | "numeric" => Some("numeric"),
        "character varying" | "varchar" => Some("varchar"),
        "character" | "char" | "bpchar" => Some("bpchar"),
        "bit" | "\"bit\"" => Some("bit"),
        "bit varying" | "varbit" => Some("varbit"),
        "bool" | "boolean" => Some("bool"),
        "timestamp without time zone" | "timestamp" => Some("timestamp"),
        "timestamp with time zone" | "timestamptz" => Some("timestamptz"),
        "time without time zone" | "time" => Some("time"),
        "time with time zone" | "timetz" => Some("timetz"),
        _ => None,
    };
    let base = match canonical {
        Some(canonical) if canonical != base.as_str() => control.copy_text(canonical)?,
        _ => base,
    };
    if element.is_some() {
        checkpoint(control, work).await?;
        let mut result = ProductionString::from_produced(base, *control)?;
        result.push_str("[]")?;
        Ok(result.finish()?)
    } else {
        control.check()?;
        Ok(base)
    }
}

pub(super) async fn array_element_spelling<'a, W: Work>(
    name: &'a str,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<Option<&'a str>, W::Error> {
    let Some(mut element) = name.strip_suffix("[]") else {
        return Ok(None);
    };
    let mut steps = 0usize;
    loop {
        if steps.is_multiple_of(1024) {
            checkpoint(control, work).await?;
        }
        control.check()?;
        steps += 1;
        if let Some(character) = element.chars().next_back().filter(|c| c.is_whitespace()) {
            element = &element[..element.len() - character.len_utf8()];
            continue;
        }
        let Some(inner) = element.strip_suffix("[]") else {
            return Ok(Some(element));
        };
        element = inner;
    }
}

async fn strip_type_modifiers<W: Work>(
    type_name: &str,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<Produced<String>, W::Error> {
    let mut stripped = ProductionString::new(*control);
    let mut pending_space = false;
    let mut modifier_depth = 0usize;
    let mut quoted = false;
    let mut characters = type_name.chars().enumerate().peekable();
    let mut next_work = 0;
    while let Some((index, character)) = characters.next() {
        if index >= next_work {
            checkpoint(control, work).await?;
            next_work = index.saturating_add(1024);
        }
        control.check()?;
        if character == '"' && modifier_depth == 0 {
            push_compacted(&mut stripped, character, &mut pending_space)?;
            if quoted
                && characters
                    .peek()
                    .is_some_and(|(_, character)| *character == '"')
            {
                let (_, escaped) = characters.next().expect("peeked quoted identifier escape");
                push_compacted(&mut stripped, escaped, &mut pending_space)?;
            } else {
                quoted = !quoted;
            }
            continue;
        }
        if !quoted {
            if character == '(' {
                modifier_depth += 1;
                continue;
            }
            if character == ')' && modifier_depth > 0 {
                modifier_depth -= 1;
                continue;
            }
        }
        if modifier_depth == 0 {
            push_compacted(&mut stripped, character, &mut pending_space)?;
        }
    }
    Ok(stripped.finish()?)
}

fn push_compacted(
    output: &mut ProductionString<'_>,
    character: char,
    pending_space: &mut bool,
) -> Result<(), ValueRetentionError> {
    if character.is_whitespace() {
        *pending_space = !output.is_empty();
        return Ok(());
    }
    if *pending_space {
        output.push(' ')?;
        *pending_space = false;
    }
    output.push(character)
}
