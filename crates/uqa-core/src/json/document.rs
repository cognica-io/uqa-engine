//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Generic JSON values retain admitted native nodes without recursive ownership.

use super::{JsonInputError, JsonReadError, JsonReadWork, JsonScalarProtocol};
use crate::{
    image_io::AsyncImageInput,
    memory::{Budgeted, BudgetedMap, BudgetedVec, MemoryBudget, OwnedMap, ProductionControl},
    CancellationToken,
};

mod project;
mod read;
mod write;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsonValueKind {
    Null,
    Boolean,
    Number,
    String,
    Array,
    Object,
}

enum Value {
    Null,
    Boolean(bool),
    Number(Budgeted<String>),
    String(Budgeted<String>),
    Array(BudgetedVec<usize>),
    Object(OwnedMap<String, usize>),
}

struct Node {
    value: Value,
    next: Option<usize>,
}

/// An owned generic JSON generation. Object fields keep their last value in name order; node references keep destruction independent of input nesting.
pub struct JsonDocument {
    nodes: BudgetedMap<usize, Node>,
    next: usize,
    root: usize,
}

/// A borrowed value in one immutable JSON generation.
#[derive(Clone, Copy)]
pub struct JsonValue<'a> {
    document: &'a JsonDocument,
    index: usize,
}

impl JsonDocument {
    /// Construct the ordinary generic value representation under the original allowance and both cancellation owners. The control must carry allocation admission.
    pub fn read_with_control(
        bytes: &[u8],
        control: &ProductionControl<'_>,
        protocol: JsonScalarProtocol,
    ) -> Result<Self, JsonInputError> {
        super::work::inline(Self::read_with_work(bytes, control, &mut (), protocol))
    }

    /// Resume complete generic-value construction through the enclosing byte/poll scheduler. Every overwritten value is validated before its nodes retire.
    pub async fn read_from(
        bytes: &[u8],
        memory: &MemoryBudget,
        original: &CancellationToken,
        input: &mut AsyncImageInput<'_>,
        protocol: JsonScalarProtocol,
    ) -> Result<Self, JsonInputError> {
        let control = ProductionControl::new(memory, original, input.cancellation);
        Self::read_with_work(bytes, &control, input, protocol).await
    }

    pub async fn read_with_work<W: JsonReadWork<Error = JsonReadError>>(
        bytes: &[u8],
        control: &ProductionControl<'_>,
        work: &mut W,
        protocol: JsonScalarProtocol,
    ) -> Result<Self, JsonInputError> {
        let mut document = Self {
            nodes: BudgetedMap::new(control.budget().expect("admitted JSON document")),
            next: 0,
            root: 0,
        };
        document.root = read::text(&mut document, bytes, control, work, protocol).await?;
        control.check().map_err(JsonReadError::from)?;
        Ok(document)
    }

    pub fn root(&self) -> JsonValue<'_> {
        JsonValue {
            document: self,
            index: self.root,
        }
    }

    fn insert(&mut self, value: Value) -> Result<usize, JsonInputError> {
        let index = self.next;
        self.next = self.next.checked_add(1).ok_or(JsonInputError::Invalid)?;
        self.nodes.insert(index, Node { value, next: None })?;
        Ok(index)
    }

    async fn retire<W: JsonReadWork<Error = JsonReadError>>(
        &mut self,
        index: usize,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<(), JsonInputError> {
        let mut pending = Some(index);
        while let Some(index) = pending {
            work.checkpoint().await?;
            control.check().map_err(JsonReadError::from)?;
            let Node { value, next } = self.nodes.remove(&index).expect("private JSON subtree");
            pending = next;
            match value {
                Value::Array(children) => {
                    for &child in children.iter() {
                        work.checkpoint().await?;
                        control.check().map_err(JsonReadError::from)?;
                        self.nodes.get_mut(&child).expect("JSON array child").next = pending;
                        pending = Some(child);
                    }
                }
                Value::Object(children) => {
                    for (_, child) in children {
                        work.checkpoint().await?;
                        control.check().map_err(JsonReadError::from)?;
                        self.nodes.get_mut(&child).expect("JSON object child").next = pending;
                        pending = Some(child);
                    }
                }
                _ => (),
            }
        }
        Ok(())
    }
}

impl<'a> JsonValue<'a> {
    /// Reproduce generic-value deserialization after physical input has completed. Sorted scalar-protocol fields and number visitor conversion are interpreted at this distinct boundary.
    pub async fn project_with_work<W: JsonReadWork<Error = JsonReadError>>(
        self,
        control: &ProductionControl<'_>,
        work: &mut W,
        protocol: JsonScalarProtocol,
    ) -> Result<JsonDocument, JsonInputError> {
        project::value(self, control, work, protocol).await
    }

    /// Read a typed unsigned field from a complete generic value, retaining its unlocated numeric/type diagnostics.
    pub async fn read_u32_with_work<W: JsonReadWork<Error = JsonReadError>>(
        self,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<u32, JsonInputError> {
        control.check().map_err(JsonReadError::from)?;
        work.checkpoint().await?;
        if let Some(text) = self.number_text() {
            if text.len() <= 10 {
                if let Ok(value) = text.parse() {
                    return Ok(value);
                }
            }
            return Err(super::JsonDiagnostic::from(super::JsonSyntaxError {
                kind: super::JsonSyntaxKind::InvalidNumber,
                position: super::JsonPosition::unlocated(),
            })
            .into());
        }
        Err(
            super::JsonDiagnostic::projected_type_with_work(self, "u32", control, work)
                .await?
                .into(),
        )
    }
    fn value(self) -> &'a Value {
        &self
            .document
            .nodes
            .get(&self.index)
            .expect("published JSON value")
            .value
    }

    pub fn kind(self) -> JsonValueKind {
        match self.value() {
            Value::Null => JsonValueKind::Null,
            Value::Boolean(_) => JsonValueKind::Boolean,
            Value::Number(_) => JsonValueKind::Number,
            Value::String(_) => JsonValueKind::String,
            Value::Array(_) => JsonValueKind::Array,
            Value::Object(_) => JsonValueKind::Object,
        }
    }

    pub fn as_bool(self) -> Option<bool> {
        match self.value() {
            Value::Boolean(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_str(self) -> Option<&'a str> {
        match self.value() {
            Value::String(value) => Some(value),
            _ => None,
        }
    }

    pub fn number_text(self) -> Option<&'a str> {
        match self.value() {
            Value::Number(value) => Some(value),
            _ => None,
        }
    }

    pub fn get(self, name: &str) -> Option<Self> {
        let Value::Object(fields) = self.value() else {
            return None;
        };
        Some(Self {
            document: self.document,
            index: *fields.get(name)?,
        })
    }

    pub fn elements(self) -> Option<impl ExactSizeIterator<Item = Self> + 'a> {
        let Value::Array(values) = self.value() else {
            return None;
        };
        Some(values.iter().map(move |&index| Self {
            document: self.document,
            index,
        }))
    }

    pub fn fields(self) -> Option<impl ExactSizeIterator<Item = (&'a str, Self)> + 'a> {
        let Value::Object(values) = self.value() else {
            return None;
        };
        Some(values.iter().map(move |(name, &index)| {
            (
                name.as_str(),
                Self {
                    document: self.document,
                    index,
                },
            )
        }))
    }

    /// Serialize this projected value with admitted output and explicit traversal frames, preserving field order and generic numeric representation.
    pub fn to_json_with_control(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<Budgeted<String>, JsonInputError> {
        super::work::inline(self.to_json_with_work(control, &mut ()))
    }

    pub async fn to_json_with_work<W: JsonReadWork<Error = JsonReadError>>(
        self,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<Budgeted<String>, JsonInputError> {
        write::value(self, control, work).await
    }
}

#[cfg(test)]
mod tests;
