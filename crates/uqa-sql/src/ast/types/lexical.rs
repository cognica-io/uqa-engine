//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Type identities and SQL type declarations share bounded whitespace and decimal scanning.

use std::future::Future;
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionString},
    ValueRetentionError,
};

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

pub(super) async fn checkpoint<W: Work>(
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<(), W::Error> {
    control.check()?;
    work.checkpoint().await?;
    control.check()?;
    Ok(())
}

pub(super) async fn trim_start<'a, W: Work>(
    name: &'a str,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<&'a str, W::Error> {
    let mut start = 0;
    for (index, character) in name.chars().enumerate() {
        if index % 1024 == 0 {
            checkpoint(control, work).await?;
        }
        if !character.is_whitespace() {
            break;
        }
        start += character.len_utf8();
    }
    Ok(&name[start..])
}

pub(super) async fn trim_end<'a, W: Work>(
    name: &'a str,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<&'a str, W::Error> {
    let mut end = name.len();
    for (index, character) in name.chars().rev().enumerate() {
        if index % 1024 == 0 {
            checkpoint(control, work).await?;
        }
        if !character.is_whitespace() {
            break;
        }
        end -= character.len_utf8();
    }
    Ok(&name[..end])
}

pub(super) async fn trim<'a, W: Work>(
    name: &'a str,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<&'a str, W::Error> {
    trim_end(trim_start(name, control, work).await?, control, work).await
}

/// The existing OID digit scanner also admits signed SQL modifiers with their original integer limits.
pub(super) async fn integer<W: Work>(
    text: &str,
    range: std::ops::RangeInclusive<i64>,
    sign: bool,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<Option<i64>, W::Error> {
    let negative = sign && text.starts_with('-');
    if negative && *range.start() >= 0 {
        return Ok(None);
    }
    let digits = if sign {
        text.strip_prefix(['-', '+']).unwrap_or(text)
    } else {
        text
    };
    if digits.is_empty() {
        return Ok(None);
    }
    let mut value = 0_i64;
    for (index, byte) in digits.bytes().enumerate() {
        if index % 1024 == 0 {
            checkpoint(control, work).await?;
        }
        if !byte.is_ascii_digit() {
            return Ok(None);
        }
        let Some(next) = value.checked_mul(10).and_then(|value| {
            if negative {
                value.checked_sub(i64::from(byte - b'0'))
            } else {
                value.checked_add(i64::from(byte - b'0'))
            }
        }) else {
            return Ok(None);
        };
        if !range.contains(&next) {
            return Ok(None);
        }
        value = next;
    }
    Ok(Some(value))
}

pub(super) async fn text<W: Work>(
    parts: &[&str],
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<Produced<String>, W::Error> {
    let mut output = ProductionString::new(*control);
    output.reserve(parts.iter().map(|part| part.len()).sum())?;
    for source in parts {
        let mut start = 0;
        while start < source.len() {
            checkpoint(control, work).await?;
            start += output.push_str_chunk(&source[start..])?;
        }
    }
    Ok(output.finish()?)
}
