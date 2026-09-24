//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Atomic `PostgreSQL` catalog vectors retain their underlying array metadata.

use super::{ArrayValue, Value};
use crate::{
    memory::{Produced, ProductionControl, ProductionVec},
    ValueRetentionError,
};
use std::cmp::Ordering;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LegacyVectorKind {
    SmallInteger,
    Oid,
}

impl LegacyVectorKind {
    pub const fn type_name(self) -> &'static str {
        match self {
            Self::SmallInteger => "int2vector",
            Self::Oid => "oidvector",
        }
    }

    pub(super) fn accepts(self, value: &Value) -> bool {
        match value {
            Value::Int(value) => match self {
                Self::SmallInteger => i16::try_from(*value).is_ok(),
                Self::Oid => u32::try_from(*value).is_ok(),
            },
            _ => false,
        }
    }
}

/// An `int2vector` or `oidvector` remains one scalar element inside a SQL array. Text input uses one zero-based dimension, including an empty vector; polymorphic array functions may return other bounds or a dimensionless empty array. OID operators ignore bounds and compare length first; small-integer vectors use array ordering.
#[derive(Debug, Clone)]
pub struct LegacyVectorValue {
    kind: LegacyVectorKind,
    array: ArrayValue,
}

impl LegacyVectorValue {
    pub fn try_from_array(kind: LegacyVectorKind, array: ArrayValue) -> Option<Self> {
        let control = ProductionControl::uncontrolled();
        Self::try_from_array_with_control(kind, control.finish(array, None).ok()?, &control)
            .ok()??
            .into_uncontrolled()
            .ok()
    }

    /// Preserve array-function results without rewriting their dimensions or lower bounds.
    pub fn try_from_array_with_control(
        kind: LegacyVectorKind,
        array: Produced<ArrayValue>,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Self>>, ValueRetentionError> {
        control.check()?;
        if array.dimensions().len() > 1 {
            return Ok(None);
        }
        for value in array.elements() {
            control.check()?;
            if !kind.accepts(value) {
                return Ok(None);
            }
        }
        let (array, memory) = array.into_parts();
        control.finish(Self { kind, array }, memory).map(Some)
    }

    pub fn try_new(kind: LegacyVectorKind, elements: Vec<Value>) -> Option<Self> {
        let control = ProductionControl::uncontrolled();
        Self::try_new_with_control(kind, control.finish(elements, None).ok()?, &control)
            .ok()??
            .into_uncontrolled()
            .ok()
    }

    pub fn try_new_with_control(
        kind: LegacyVectorKind,
        elements: Produced<Vec<Value>>,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Self>>, ValueRetentionError> {
        control.check()?;
        for value in &*elements {
            control.check()?;
            if !kind.accepts(value) {
                return Ok(None);
            }
        }
        let mut bounds = ProductionVec::new(*control);
        bounds.push_copy(0)?;
        let Some(array) =
            ArrayValue::with_lower_bounds_with_control(elements, bounds.finish()?, control)?
        else {
            return Ok(None);
        };
        let (array, memory) = array.into_parts();
        control.finish(Self { kind, array }, memory).map(Some)
    }

    pub const fn kind(&self) -> LegacyVectorKind {
        self.kind
    }

    pub fn elements(&self) -> &[Value] {
        self.array.elements()
    }

    /// Owned payload size, excluding this value's inline layout, saturated if its buffers exceed the addressable sum. Elements are inline integers and have no additional retained payload.
    pub fn retained_bytes(&self) -> usize {
        self.array.retained_buffer_bytes().unwrap_or(usize::MAX)
    }

    pub const fn as_array(&self) -> &ArrayValue {
        &self.array
    }

    /// Vector output and OID-vector operators require one dimension; generic array functions can consume dimensionless results too.
    pub fn has_vector_layout(&self) -> bool {
        self.array.dimensions().len() == 1
    }

    /// Array-polymorphic MIN/MAX use array ordering even for OID vectors.
    pub fn compare_as_array(&self, other: &Self) -> Ordering {
        super::value::compare_postgres_arrays(&self.array, &other.array)
    }

    pub fn into_array(self) -> ArrayValue {
        self.array
    }

    /// Tagged decoding and copying validate or preserve the element kind before transferring their already admitted array buffers.
    pub(super) fn from_validated_array(kind: LegacyVectorKind, array: ArrayValue) -> Self {
        debug_assert!(array.dimensions().len() <= 1);
        Self { kind, array }
    }

    pub(super) fn compare_prefix(&self, other: &Self) -> Ordering {
        self.kind.cmp(&other.kind).then_with(|| match self.kind {
            LegacyVectorKind::SmallInteger => Ordering::Equal,
            LegacyVectorKind::Oid => self
                .array
                .dimensions()
                .len()
                .cmp(&other.array.dimensions().len())
                .then_with(|| self.elements().len().cmp(&other.elements().len())),
        })
    }

    /// Emit a collision-free key whose byte order agrees with native ordering. The sink owns allocation and cancellation; each call writes at most eight bytes.
    pub fn write_comparison_key<E>(
        &self,
        mut write: impl FnMut(&[u8]) -> Result<(), E>,
    ) -> Result<(), E> {
        match self.kind {
            LegacyVectorKind::SmallInteger => write(&[0])?,
            LegacyVectorKind::Oid => {
                write(&[1])?;
                write(&[u8::from(self.has_vector_layout())])?;
                write(&(self.elements().len() as u64).to_be_bytes())?;
            }
        }
        for value in self.elements() {
            let Value::Int(value) = value else {
                unreachable!("validated legacy vector element");
            };
            write(&[1])?;
            write(&((*value as u64) ^ (1_u64 << 63)).to_be_bytes())?;
        }
        write(&[0])?;
        if self.kind == LegacyVectorKind::SmallInteger {
            write(&[u8::from(self.has_vector_layout())])?;
            if let Some(lower) = self.array.lower_bound(0) {
                write(&((lower as u32) ^ (1_u32 << 31)).to_be_bytes())?;
            }
        }
        Ok(())
    }
}

impl Ord for LegacyVectorValue {
    fn cmp(&self, other: &Self) -> Ordering {
        self.compare_prefix(other).then_with(|| match self.kind {
            LegacyVectorKind::SmallInteger => self.compare_as_array(other),
            LegacyVectorKind::Oid => self.elements().cmp(other.elements()),
        })
    }
}

impl PartialEq for LegacyVectorValue {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}

impl Eq for LegacyVectorValue {}

impl PartialOrd for LegacyVectorValue {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl serde::Serialize for LegacyVectorValue {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(serde::Serialize)]
        struct Encoded<'a> {
            #[serde(rename = "$uqa_type")]
            kind: &'static str,
            values: &'a [Value],
            #[serde(skip_serializing_if = "Option::is_none")]
            lower_bounds: Option<&'a [i32]>,
        }
        Encoded {
            kind: self.kind.type_name(),
            values: self.elements(),
            lower_bounds: (self.array.lower_bounds() != [0]).then_some(self.array.lower_bounds()),
        }
        .serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for LegacyVectorValue {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match <Value as serde::Deserialize>::deserialize(deserializer)? {
            Value::LegacyVector(vector) => Ok(vector),
            _ => Err(serde::de::Error::custom("invalid PostgreSQL legacy vector")),
        }
    }
}

#[cfg(test)]
mod tests;
