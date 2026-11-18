//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native identity spelling shares its grammar between ordinary and retained work.

use super::super::lexical::{self, checkpoint, Work};
use super::{UserTypeIdentity, UserTypeKind};
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

impl Work for AsyncImageInput<'_> {
    type Error = TypeIdentityInputError;
    async fn checkpoint(&mut self) -> Result<(), Self::Error> {
        Ok(AsyncImageInput::checkpoint(self).await?)
    }
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
    let mut element = lexical::trim(name, control, work).await?;
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
    let Some(oid) = lexical::integer(digits, 0..=i64::from(u32::MAX), false, control, work).await?
    else {
        return Ok(None);
    };
    let oid = u32::try_from(oid).expect("validated OID range");
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
