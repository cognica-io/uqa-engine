//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Descriptor values lend their original fields while native strings and collections retain progress.

use super::{Item, Read};
mod scalar;
use crate::{work::Work, AnalysisError, AnalysisResult, Analyzer};
use serde::de::{DeserializeOwned, Error as _, Unexpected};
use serde_json::Value;
use std::collections::BTreeMap;

enum Fields<'a> {
    Object {
        entries: serde_json::map::Iter<'a>,
        tag: Option<&'static str>,
        content: bool,
    },
    Sequence {
        entries: std::slice::Iter<'a, Value>,
        index: usize,
        content: bool,
    },
}

struct Input<'a, 'w, W> {
    current: &'a Value,
    content: bool,
    finished_sequence: Option<(usize, usize, &'static str)>,
    fields: Vec<Fields<'a>>,
    work: &'w mut W,
}

pub(crate) async fn decode(
    value: &Value,
    work: &mut impl Work<AnalysisError>,
) -> AnalysisResult<Analyzer> {
    super::analyzer(&mut Input {
        current: value,
        content: false,
        finished_sequence: None,
        fields: Vec::new(),
        work,
    })
    .await
}

impl<W: Work<AnalysisError>> Read for Input<'_, '_, W> {
    type Error = AnalysisError;

    fn error(&self, error: serde_json::Error) -> Self::Error {
        error.into()
    }
    fn implicit_defaults(&self) -> bool {
        true
    }
    fn missing(&self, name: &'static str) -> Self::Error {
        if let Some((length, fields, description)) = self.finished_sequence {
            let expected = format!(
                "{description} with {fields} element{}",
                if fields == 1 { "" } else { "s" }
            );
            serde_json::Error::invalid_length(length, &expected.as_str()).into()
        } else {
            serde_json::Error::missing_field(name).into()
        }
    }
    #[cfg(any(feature = "nori", feature = "kuromoji"))]
    fn untagged(&self, error: Self::Error, name: &'static str) -> Self::Error {
        match error {
            AnalysisError::Json(_) => serde_json::Error::custom(format!(
                "data did not match any variant of untagged enum {name}"
            ))
            .into(),
            error => error,
        }
    }
    #[cfg(any(feature = "nori", feature = "kuromoji"))]
    async fn is_text(&mut self) -> AnalysisResult<bool> {
        Ok(self.current.is_string())
    }

    async fn object(&mut self, name: &'static str) -> AnalysisResult<()> {
        self.work.checkpoint().await?;
        let fields = match self.current {
            Value::Object(object) => Fields::Object {
                entries: object.iter(),
                tag: None,
                content: self.content,
            },
            Value::Array(values) => Fields::Sequence {
                entries: values.iter(),
                index: 0,
                content: self.content,
            },
            value => {
                return Err(serde_json::Error::invalid_type(
                    unexpected(value, self.content),
                    &format!("struct {name}").as_str(),
                )
                .into())
            }
        };
        self.fields.push(fields);
        Ok(())
    }

    async fn tagged(
        &mut self,
        name: &'static str,
        tag: &'static str,
        variants: &'static [&'static str],
    ) -> AnalysisResult<&'static str> {
        self.work.checkpoint().await?;
        let mut selected = None;
        let fields = match self.current {
            Value::Object(object) => {
                for (key, value) in object {
                    self.work.checkpoint().await?;
                    if key == tag {
                        selected = Some(self.select_tag(value, variants)?);
                    } else {
                        scalar::admit(value, self.work).await?;
                    }
                }
                Fields::Object {
                    entries: object.iter(),
                    tag: Some(tag),
                    content: true,
                }
            }
            Value::Array(values) => {
                let (value, values) = values
                    .split_first()
                    .ok_or_else(|| serde_json::Error::missing_field(tag))?;
                selected = Some(self.select_tag(value, variants)?);
                for value in values {
                    scalar::admit(value, self.work).await?;
                }
                Fields::Sequence {
                    entries: values.iter(),
                    index: 0,
                    content: true,
                }
            }
            value => {
                let expected = format!("internally tagged enum {name}");
                return Err(serde::Deserializer::deserialize_any(
                    value,
                    scalar::Reject(&expected, tag),
                )
                .unwrap_err()
                .into());
            }
        };
        let selected = selected.ok_or_else(|| serde_json::Error::missing_field(tag))?;
        self.fields.push(fields);
        Ok(selected)
    }

    async fn field(
        &mut self,
        names: &'static [&'static str],
        _: &'static [bool],
        description: &'static str,
        deny: bool,
        _: &mut Option<usize>,
    ) -> AnalysisResult<Option<usize>> {
        loop {
            self.work.checkpoint().await?;
            match self
                .fields
                .last_mut()
                .expect("a native configuration object is open")
            {
                Fields::Object {
                    entries,
                    tag,
                    content,
                } => {
                    if let Some((key, value)) = entries.next() {
                        if Some(key.as_str()) == *tag {
                            continue;
                        }
                        if let Some(index) = names.iter().position(|name| *name == key) {
                            self.current = value;
                            self.content = *content;
                            return Ok(Some(index));
                        }
                        if deny {
                            return Err(serde_json::Error::unknown_field(key, names).into());
                        }
                        continue;
                    }
                }
                Fields::Sequence {
                    entries,
                    index,
                    content,
                } => {
                    if let Some(value) = entries.next() {
                        if *index == names.len() {
                            let expected = if *content {
                                format!("{index} elements in sequence")
                            } else {
                                "fewer elements in array".into()
                            };
                            return Err(serde_json::Error::invalid_length(
                                *index + 1 + entries.len(),
                                &expected.as_str(),
                            )
                            .into());
                        }
                        self.current = value;
                        self.content = *content;
                        let result = *index;
                        *index += 1;
                        return Ok(Some(result));
                    }
                    self.finished_sequence = Some((*index, names.len(), description));
                }
            }
            if matches!(self.fields.pop(), Some(Fields::Object { .. })) {
                self.finished_sequence = None;
            }
            return Ok(None);
        }
    }

    async fn text(&mut self, expected: &'static str) -> AnalysisResult<String> {
        let value = self.current.as_str().ok_or_else(|| {
            serde_json::Error::invalid_type(unexpected(self.current, self.content), &expected)
        })?;
        copy(value, self.work).await
    }

    async fn atom<T: DeserializeOwned + Send>(&mut self) -> AnalysisResult<T> {
        self.work.checkpoint().await?;
        if self.content {
            return Ok(T::deserialize(scalar::Scalar(self.current))?);
        }
        Ok(T::deserialize(self.current)?)
    }

    async fn null(&mut self) -> AnalysisResult<bool> {
        self.work.checkpoint().await?;
        Ok(self.current.is_null())
    }

    async fn sequence<T: Item>(&mut self) -> AnalysisResult<Vec<T>> {
        let Value::Array(values) = self.current else {
            return Err(serde_json::Error::invalid_type(
                unexpected(self.current, self.content),
                &"a sequence",
            )
            .into());
        };
        let mut result = Vec::new();
        let content = self.content;
        for value in values {
            self.work.checkpoint().await?;
            self.current = value;
            self.content = content;
            result.push(T::read(self).await?);
        }
        Ok(result)
    }

    async fn map<T: Item>(&mut self) -> AnalysisResult<BTreeMap<String, T>> {
        let projection = if self.content && self.current.is_number() {
            scalar::number_map(self.current)?
        } else {
            None
        };
        let values = if let Value::Object(values) = self.current {
            Some(values)
        } else {
            projection.as_ref()
        };
        let Some(values) = values else {
            return Err(serde_json::Error::invalid_type(
                unexpected(self.current, self.content),
                &"a map",
            )
            .into());
        };
        let mut result = BTreeMap::new();
        let content = self.content;
        for (key, value) in values {
            self.work.checkpoint().await?;
            let key = copy(key, self.work).await?;
            let mut child = Input {
                current: value,
                content,
                fields: Vec::new(),
                finished_sequence: None,
                work: self.work,
            };
            result.insert(key, T::read(&mut child).await?);
        }
        Ok(result)
    }
}

impl<W> Input<'_, '_, W> {
    fn select_tag(
        &self,
        value: &Value,
        variants: &'static [&'static str],
    ) -> AnalysisResult<&'static str> {
        let name = value.as_str().ok_or_else(|| {
            serde_json::Error::invalid_type(unexpected(value, self.content), &"variant identifier")
        })?;
        variants
            .iter()
            .copied()
            .find(|variant| *variant == name)
            .ok_or_else(|| serde_json::Error::unknown_variant(name, variants).into())
    }
}

async fn copy(value: &str, work: &mut impl Work<AnalysisError>) -> AnalysisResult<String> {
    let mut result = String::new();
    let mut position = 0;
    while position < value.len() {
        work.checkpoint().await?;
        let part = crate::json::fragment(value, position);
        result.push_str(part);
        position += part.len();
    }
    Ok(result)
}

fn unexpected(value: &Value, content: bool) -> Unexpected<'_> {
    match value {
        Value::Null => Unexpected::Unit,
        Value::Bool(value) => Unexpected::Bool(*value),
        Value::Number(value) if content => {
            if let Some(value) = value.as_u64() {
                Unexpected::Unsigned(value)
            } else if let Some(value) = value.as_i64() {
                Unexpected::Signed(value)
            } else if let Some(value) = value.as_f64() {
                Unexpected::Float(value)
            } else {
                Unexpected::Map
            }
        }
        Value::Number(_) => Unexpected::Other("number"),
        Value::String(value) => Unexpected::Str(value),
        Value::Array(_) => Unexpected::Seq,
        Value::Object(_) => Unexpected::Map,
    }
}
