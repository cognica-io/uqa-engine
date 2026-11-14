//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native identity spelling shares its grammar between ordinary and retained work.

use super::{UserTypeIdentity, UserTypeKind};
use std::future::Future;
use uqa_core::{
    image_io::{AsyncImageInput, ImageIoError},
    memory::{Produced, ProductionControl, ProductionString},
    ValueRetentionError,
};

#[derive(Debug, thiserror::Error)]
pub enum TypeIdentityInputError {
    #[error(transparent)]
    Native(#[from] ValueRetentionError),
    #[error(transparent)]
    Work(#[from] ImageIoError),
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
    type Error = TypeIdentityInputError;
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

impl UserTypeIdentity {
    /// Borrow an existing type identity without allocating or resolving it against a catalog. Whitespace, repeated array suffixes and decimal OIDs retain the ordinary grammar.
    pub async fn parse_from_input(
        name: &str,
        control: &ProductionControl<'_>,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<Option<Self>, TypeIdentityInputError> {
        parse(name, control, input).await
    }

    pub fn render_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, ValueRetentionError> {
        crate::catalog::work::inline(render(*self, control, &mut ()))
    }

    /// Admit the canonical scalar spelling and every original array suffix through the supplied work channel.
    pub async fn render_from_input(
        &self,
        control: &ProductionControl<'_>,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<Produced<String>, TypeIdentityInputError> {
        render(*self, control, input).await
    }
}

pub(super) async fn parse<W: Work>(
    name: &str,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<Option<UserTypeIdentity>, W::Error> {
    checkpoint(control, work).await?;
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
    let mut end = name.len();
    for (index, character) in name[start..].chars().rev().enumerate() {
        if index % 1024 == 0 {
            checkpoint(control, work).await?;
        }
        if !character.is_whitespace() {
            break;
        }
        end -= character.len_utf8();
    }
    let mut element = &name[start..end];
    let mut dimensions = 0;
    while let Some(inner) = element.strip_suffix("[]") {
        if dimensions % 1024 == 0 {
            checkpoint(control, work).await?;
        }
        element = inner;
        dimensions += 1;
    }
    let Some((kind, digits)) = [
        UserTypeKind::Enum,
        UserTypeKind::Composite,
        UserTypeKind::Range,
        UserTypeKind::Multirange,
        UserTypeKind::Domain,
    ]
    .into_iter()
    .find_map(|kind| {
        element
            .strip_prefix(kind.prefix())
            .map(|digits| (kind, digits))
    }) else {
        return Ok(None);
    };
    if digits.is_empty() {
        return Ok(None);
    }
    let mut oid = 0_u32;
    for (index, byte) in digits.bytes().enumerate() {
        if index % 1024 == 0 {
            checkpoint(control, work).await?;
        }
        if !byte.is_ascii_digit() {
            return Ok(None);
        }
        let Some(next) = oid
            .checked_mul(10)
            .and_then(|oid| oid.checked_add(u32::from(byte - b'0')))
        else {
            return Ok(None);
        };
        oid = next;
    }
    control.check()?;
    Ok(Some(UserTypeIdentity {
        kind,
        oid,
        dimensions,
    }))
}

async fn render<W: Work>(
    value: UserTypeIdentity,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<Produced<String>, W::Error> {
    checkpoint(control, work).await?;
    let scalar = control.format(format_args!("{}{}", value.kind.prefix(), value.oid))?;
    let mut text = ProductionString::from_produced(scalar, *control)?;
    for index in 0..value.dimensions {
        if index % 1024 == 0 {
            checkpoint(control, work).await?;
        }
        text.push_str("[]")?;
    }
    Ok(text.finish()?)
}

#[cfg(test)]
mod tests;
