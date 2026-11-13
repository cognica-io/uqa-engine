//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed array and unit-variant operations own structural admission and completion.

use super::{
    decode_json_string_from, Frame, JsonDiagnostic, JsonEvent, JsonExpected, JsonInputError,
    JsonReadError, JsonReadWork, JsonReader, JsonSyntaxKind, JsonToken, ReadStep,
};
use crate::image_io::AsyncImageInput;

mod numbers;

impl<'a> JsonReader<'a, '_> {
    pub(super) async fn array_step<W: JsonReadWork>(
        &mut self,
        work: &mut W,
    ) -> Result<Option<JsonEvent<'a, &'a [u8]>>, W::Error> {
        let frame = self.stack.frames().last().copied();
        self.expected = match frame {
            Some(Frame::ArrayFirst) => JsonExpected::ArrayValueOrEnd,
            Some(Frame::ArrayAfter) => JsonExpected::ArraySeparator,
            _ => return Err(JsonReadError::InvalidJson.into()),
        };
        match (frame, self.peek()) {
            (Some(Frame::ArrayFirst | Frame::ArrayAfter), Some(b']')) => {
                return self.close(JsonToken::EndArray).map_err(Into::into)
            }
            (Some(Frame::ArrayFirst | Frame::ArrayAfter), None) => {
                return Err(self.reject(JsonSyntaxKind::EndOfArray, false).into())
            }
            (Some(Frame::ArrayAfter), Some(b',')) => {
                self.advance(work).await?;
                self.skip_whitespace(work).await?;
                self.expected = JsonExpected::Value;
            }
            (Some(Frame::ArrayAfter), _) => {
                return Err(self
                    .reject(JsonSyntaxKind::ExpectedArraySeparator, true)
                    .into())
            }
            (Some(Frame::ArrayFirst), _) => (),
            _ => return Err(JsonReadError::InvalidJson.into()),
        }
        self.replace(Frame::ArrayValue);
        Ok(None)
    }

    /// Admit the next array element without consuming it under a value-specific policy. A closing delimiter completes the array; another call cannot skip the admitted value. The subsequent typed or raw operation owns its original string and scalar rules.
    pub async fn next_array_value_with_work<W: JsonReadWork>(
        &mut self,
        work: &mut W,
    ) -> Result<bool, W::Error> {
        work.checkpoint().await?;
        self.check()?;
        self.skip_whitespace(work).await?;
        if self.array_step(work).await?.is_some() {
            return Ok(false);
        }
        match self.peek() {
            Some(b']') => Err(self.reject(JsonSyntaxKind::TrailingComma, true).into()),
            None => Err(self.reject(JsonSyntaxKind::EndOfValue, false).into()),
            Some(_) => Ok(true),
        }
    }

    /// Complete a positional struct after its final declared field. Reject extra values at the original sequence boundary without parsing their payloads.
    pub async fn finish_array_with_work<W: JsonReadWork>(
        &mut self,
        work: &mut W,
    ) -> Result<(), W::Error> {
        work.checkpoint().await?;
        self.check()?;
        if !matches!(
            self.stack.frames().last(),
            Some(Frame::ArrayFirst | Frame::ArrayAfter)
        ) {
            return Err(JsonReadError::InvalidJson.into());
        }
        self.skip_whitespace(work).await?;
        let kind = match self.peek() {
            Some(b']') => {
                self.close(JsonToken::EndArray)?;
                return Ok(());
            }
            Some(b',') => {
                self.advance(work).await?;
                self.skip_whitespace(work).await?;
                if self.peek() == Some(b']') {
                    JsonSyntaxKind::TrailingComma
                } else {
                    JsonSyntaxKind::TrailingCharacters
                }
            }
            None => JsonSyntaxKind::EndOfArray,
            Some(_) => JsonSyntaxKind::TrailingCharacters,
        };
        Err(self.reject(kind, true).into())
    }

    /// Decode a closed unit enum in its original string or single-member object form. Unknown names fail before the payload; the map completion error remains distinct from ordinary scalar-object completion.
    pub async fn read_unit_variant_from(
        &mut self,
        variants: &'static [&'static str],
        work: &mut AsyncImageInput<'_>,
    ) -> Result<usize, JsonInputError> {
        if !matches!(
            self.next_step(work, false)
                .await
                .map_err(|error| JsonInputError::from_reader(self, error))?,
            ReadStep::Value
        ) {
            return Err(JsonInputError::Invalid);
        }
        let object = match self.peek() {
            Some(b'{') => {
                self.value(work, false)
                    .await
                    .map_err(|error| JsonInputError::from_reader(self, error))?;
                self.skip_whitespace(work).await?;
                true
            }
            Some(b'"') => false,
            token => {
                let error = self.reject(
                    if token.is_none() {
                        JsonSyntaxKind::EndOfValue
                    } else {
                        JsonSyntaxKind::ExpectedValue
                    },
                    true,
                );
                return Err(JsonInputError::from_reader(self, error));
            }
        };
        if self.peek() != Some(b'"') {
            return Err(self
                .type_error_from("variant identifier", work)
                .await?
                .into());
        }
        let event = self
            .value(work, false)
            .await
            .map_err(|error| JsonInputError::from_reader(self, error))?;
        let JsonToken::String(encoded) = event.token else {
            unreachable!("validated variant string")
        };
        let name = decode_json_string_from(encoded, work).await?;
        let Some(index) = variants
            .iter()
            .position(|variant| *variant == name.as_str())
        else {
            return Err(JsonDiagnostic::unknown_variant(name, variants, self.position()).into());
        };
        drop(name);
        if object {
            self.replace(Frame::ObjectValue);
            if !matches!(
                self.next_step(work, false)
                    .await
                    .map_err(|error| JsonInputError::from_reader(self, error))?,
                ReadStep::Value
            ) {
                return Err(JsonInputError::Invalid);
            }
            if self.peek() != Some(b'n') {
                return Err(self.type_error_from("unit", work).await?.into());
            }
            self.value(work, false)
                .await
                .map_err(|error| JsonInputError::from_reader(self, error))?;
            self.skip_whitespace(work).await?;
            if self.peek() != Some(b'}') {
                let error = self.reject(
                    if self.peek().is_none() {
                        JsonSyntaxKind::EndOfObject
                    } else {
                        JsonSyntaxKind::ExpectedValue
                    },
                    false,
                );
                return Err(JsonInputError::from_reader(self, error));
            }
            self.close(JsonToken::EndObject)?;
        }
        Ok(index)
    }
}
