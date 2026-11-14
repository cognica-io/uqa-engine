//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed reference parsing shares its grammar across ordinary and retained work.

use super::{InvalidReference, RelationName, RelationReference, RelationReferenceError};
use crate::{
    image_io::{AsyncImageInput, ImageIoError},
    CancellationToken, RelationIdentity,
};
use std::{
    future::Future,
    task::{Context, Poll, Waker},
};

/// Original borrowed syntax diagnostics and typed input failures remain distinct.
#[derive(Debug)]
pub enum RelationReferenceInputError<'a> {
    Reference(RelationReferenceError<'a>),
    Input(ImageIoError),
}

impl std::fmt::Display for RelationReferenceInputError<'_> {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Reference(error) => std::fmt::Display::fmt(error, output),
            Self::Input(error) => std::fmt::Display::fmt(error, output),
        }
    }
}

impl std::error::Error for RelationReferenceInputError<'_> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Reference(_) => None,
            Self::Input(error) => Some(error),
        }
    }
}

impl RelationIdentity {
    /// Parse the original reference grammar without copying names or reading source bytes. Both cancellation owners remain active while lexical work is suspended.
    pub async fn try_borrowed_reference_from_input<'a>(
        value: &'a str,
        original: &CancellationToken,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<RelationReference<'a>, RelationReferenceInputError<'a>> {
        parse(value, &mut Input { original, input })
            .await
            .map_err(|error| match error {
                Failure::Reference(kind) => {
                    RelationReferenceInputError::Reference(RelationReferenceError {
                        kind,
                        input: value,
                    })
                }
                Failure::Input(error) => RelationReferenceInputError::Input(error),
            })
    }
}

trait Work: Send {
    type Error;
    fn checkpoint(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send;
}

impl Work for () {
    type Error = std::convert::Infallible;
    async fn checkpoint(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

struct Input<'a, 'b> {
    original: &'a CancellationToken,
    input: &'a mut AsyncImageInput<'b>,
}

impl Work for Input<'_, '_> {
    type Error = ImageIoError;
    async fn checkpoint(&mut self) -> Result<(), Self::Error> {
        self.original.check()?;
        self.input.checkpoint().await?;
        self.original.check()?;
        self.input.cancellation.check()?;
        Ok(())
    }
}

enum Failure<E> {
    Reference(InvalidReference),
    Input(E),
}
impl<E> From<InvalidReference> for Failure<E> {
    fn from(error: InvalidReference) -> Self {
        Self::Reference(error)
    }
}

pub(super) fn ordinary(value: &str) -> Result<RelationReference<'_>, InvalidReference> {
    let mut work = ();
    let mut future = std::pin::pin!(parse(value, &mut work));
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(Ok(value)) => Ok(value),
        Poll::Ready(Err(Failure::Reference(error))) => Err(error),
        Poll::Ready(Err(Failure::Input(never))) => match never {},
        Poll::Pending => unreachable!("ordinary reference parsing has no suspended work"),
    }
}

async fn parse<'a, W: Work>(
    value: &'a str,
    work: &mut W,
) -> Result<RelationReference<'a>, Failure<W::Error>> {
    work.checkpoint().await.map_err(Failure::Input)?;
    if value.is_empty() {
        return Err(InvalidReference::Empty.into());
    }
    let mut remaining = value;
    let first = component(&mut remaining, work).await?;
    if remaining.is_empty() {
        return Ok(RelationReference {
            schema: None,
            name: first,
        });
    }
    let name = component(&mut remaining, work).await?;
    if !remaining.is_empty() {
        return Err(InvalidReference::Invalid.into());
    }
    Ok(RelationReference {
        schema: Some(first),
        name,
    })
}

async fn component<'a, W: Work>(
    remaining: &mut &'a str,
    work: &mut W,
) -> Result<RelationName<'a>, Failure<W::Error>> {
    let input = *remaining;
    let quoted = input.starts_with('"');
    let start = usize::from(quoted);
    let bytes = input.as_bytes();
    let mut index = start;
    let mut next_work = 0;
    let end;
    if quoted {
        loop {
            if index >= next_work {
                work.checkpoint().await.map_err(Failure::Input)?;
                next_work = index.saturating_add(1024);
            }
            match bytes.get(index) {
                None => return Err(InvalidReference::Unterminated.into()),
                Some(b'"') if bytes.get(index + 1) == Some(&b'"') => index += 2,
                Some(b'"') => {
                    end = index;
                    index += 1;
                    break;
                }
                Some(_) => index += 1,
            }
        }
    } else {
        while let Some(byte) = bytes.get(index) {
            if index >= next_work {
                work.checkpoint().await.map_err(Failure::Input)?;
                next_work = index.saturating_add(1024);
            }
            match byte {
                b'.' => break,
                b'"' => return Err(InvalidReference::Invalid.into()),
                _ => index += 1,
            }
        }
        end = index;
    }
    if end == start {
        return Err(InvalidReference::Invalid.into());
    }
    *remaining = match bytes.get(index) {
        None => "",
        Some(b'.') if index + 1 < bytes.len() => &input[index + 1..],
        _ => return Err(InvalidReference::Invalid.into()),
    };
    Ok(RelationName {
        text: &input[start..end],
        escaped: quoted,
    })
}

#[cfg(test)]
mod tests;
