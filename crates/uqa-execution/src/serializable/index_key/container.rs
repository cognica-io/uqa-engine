//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Column-domain keys preserve array shape and nested vector ordering without coercing predicate leaves into stored values.

use std::ops::Bound;

use uqa_core::{ArrayTraversalError, ArrayValue, Predicate, Value};
use uqa_sql::{ast::ColumnType, SQLError};
use uqa_storage::read_control::StorageReadControl;

use super::{
    bytes, check, extend, invalid_rank, nonempty, rank_float, resource_error, IndexKey,
    IndexKeyRange, ScalarIndexDomain,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexDomain {
    Scalar(ScalarIndexDomain),
    Array(ScalarIndexDomain),
    List(ScalarIndexDomain),
    LegacyVector,
    Tensor,
}

impl From<ScalarIndexDomain> for IndexDomain {
    fn from(domain: ScalarIndexDomain) -> Self {
        Self::Scalar(domain)
    }
}

impl IndexDomain {
    pub fn from_column_type(ty: &ColumnType) -> Option<Self> {
        match ty {
            ColumnType::Domain { base, .. } => Self::from_column_type(base),
            ColumnType::Array(element) => {
                let leaf = match Self::from_column_type(element)? {
                    Self::Scalar(leaf) | Self::Array(leaf) | Self::List(leaf) => leaf,
                    Self::Tensor => ScalarIndexDomain::Float,
                    Self::LegacyVector => ScalarIndexDomain::Integer,
                };
                Some(Self::Array(leaf))
            }
            ColumnType::Int2Vector | ColumnType::OidVector => Some(Self::LegacyVector),
            ColumnType::Vector(_) => Some(Self::List(ScalarIndexDomain::Float)),
            ColumnType::Tensor(_) => Some(Self::Tensor),
            _ => ScalarIndexDomain::from_column_type(ty).map(Self::Scalar),
        }
    }

    pub fn encode(self, value: &Value, control: &StorageReadControl) -> Result<IndexKey, SQLError> {
        check(control)?;
        if let Self::Scalar(domain) = self {
            return domain.encode(value, control);
        }
        if matches!(value, Value::Null) {
            return bytes(control, &[0]);
        }
        if self == Self::LegacyVector {
            return self.container_variant(value).encode(value, control);
        }
        let mut key = self.start_key(control)?;
        match (self, value) {
            (Self::Array(leaf), Value::Array(array)) => {
                let mut elements = array
                    .budgeted_elements(control.memory(), control.cancellation())
                    .map_err(traversal_error)?;
                while let Some(value) = elements.next_element().map_err(traversal_error)? {
                    append_leaf(leaf, value, &mut key, control)?;
                }
                append_shape(array, &mut key, control)?;
            }
            (Self::List(_) | Self::Tensor, Value::List(values)) => {
                self.append_list(values, &mut key, control)?;
            }
            _ => return Err(invalid_container()),
        }
        Ok(key)
    }

    pub fn visit_predicate(
        self,
        predicate: &Predicate,
        control: &StorageReadControl,
        visitor: &mut dyn FnMut(IndexKeyRange) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        check(control)?;
        match predicate {
            Predicate::IsNull => visitor(IndexKeyRange {
                lower: Bound::Included(bytes(control, &[0])?),
                upper: Bound::Included(bytes(control, &[0])?),
            }),
            Predicate::IsNotNull => visitor(IndexKeyRange {
                lower: Bound::Included(self.start_key(control)?),
                upper: Bound::Excluded(self.end_key(control)?),
            }),
            Predicate::InSet(values) => {
                for value in values {
                    self.visit_comparison(
                        Some((value, true)),
                        Some((value, true)),
                        control,
                        visitor,
                    )?;
                }
                Ok(())
            }
            Predicate::Equals(value) => {
                self.visit_comparison(Some((value, true)), Some((value, true)), control, visitor)
            }
            Predicate::GreaterThan(value) => {
                self.visit_comparison(Some((value, false)), None, control, visitor)
            }
            Predicate::GreaterThanOrEqual(value) => {
                self.visit_comparison(Some((value, true)), None, control, visitor)
            }
            Predicate::LessThan(value) => {
                self.visit_comparison(None, Some((value, false)), control, visitor)
            }
            Predicate::LessThanOrEqual(value) => {
                self.visit_comparison(None, Some((value, true)), control, visitor)
            }
            Predicate::Between { low, high } => {
                self.visit_comparison(Some((low, true)), Some((high, true)), control, visitor)
            }
            Predicate::NotEquals(_) => Err(SQLError::Internal(
                "complement predicate was not selected as a value-index scan".into(),
            )),
        }
    }

    fn visit_comparison(
        self,
        lower: Option<(&Value, bool)>,
        upper: Option<(&Value, bool)>,
        control: &StorageReadControl,
        visitor: &mut dyn FnMut(IndexKeyRange) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        if let Self::Scalar(domain) = self {
            return domain.visit_comparison(lower, upper, control, visitor);
        }
        let lower = lower.map_or_else(
            || self.start_key(control).map(Bound::Included),
            |(value, inclusive)| self.bound(value, true, inclusive, control),
        )?;
        let upper = upper.map_or_else(
            || self.end_key(control).map(Bound::Excluded),
            |(value, inclusive)| self.bound(value, false, inclusive, control),
        )?;
        if nonempty(&lower, &upper) {
            visitor(IndexKeyRange { lower, upper })?;
        }
        Ok(())
    }

    fn start_key(self, control: &StorageReadControl) -> Result<IndexKey, SQLError> {
        match self {
            Self::Scalar(domain) => domain.start_key(control),
            Self::Array(_) | Self::LegacyVector => bytes(control, &[1, 10]),
            Self::List(_) | Self::Tensor => bytes(control, &[1, 11]),
        }
    }

    fn end_key(self, control: &StorageReadControl) -> Result<IndexKey, SQLError> {
        match self {
            Self::Scalar(domain) => domain.end_key(control),
            Self::Array(_) => bytes(control, &[1, 11]),
            Self::List(_) | Self::Tensor | Self::LegacyVector => bytes(control, &[1, 12]),
        }
    }

    fn bound(
        self,
        value: &Value,
        lower: bool,
        inclusive: bool,
        control: &StorageReadControl,
    ) -> Result<Bound<IndexKey>, SQLError> {
        let domain = self.container_variant(value);
        let mut key = domain.start_key(control)?;
        let exact = domain.append_bound(value, &mut key, control)?;
        Ok(if (exact && inclusive) || (!exact && lower) {
            Bound::Included(key)
        } else {
            Bound::Excluded(key)
        })
    }

    /// Stop at the first leaf with no equal stored value. The resulting cut selects all suffixes on the appropriate side, so a fractional bound is never rounded into an array element.
    fn append_bound(
        self,
        value: &Value,
        key: &mut IndexKey,
        control: &StorageReadControl,
    ) -> Result<bool, SQLError> {
        check(control)?;
        match (self, value) {
            (Self::Array(leaf), Value::Array(array)) => {
                let mut elements = array
                    .budgeted_elements(control.memory(), control.cancellation())
                    .map_err(traversal_error)?;
                while let Some(value) = elements.next_element().map_err(traversal_error)? {
                    if !append_leaf_bound(leaf, value, key, control)? {
                        return Ok(false);
                    }
                }
                append_shape(array, key, control)?;
                Ok(true)
            }
            (Self::List(_) | Self::Tensor, Value::List(values)) => {
                let leaf = self.list_leaf()?;
                for value in values {
                    check(control)?;
                    if self == Self::Tensor && !matches!(value, Value::Null) {
                        extend(key, &[1, 1, 11])?;
                        if !Self::List(ScalarIndexDomain::Float)
                            .append_bound(value, key, control)?
                        {
                            return Ok(false);
                        }
                    } else if !append_leaf_bound(leaf, value, key, control)? {
                        return Ok(false);
                    }
                }
                extend(key, &[0])?;
                Ok(true)
            }
            _ => {
                // Other native value kinds lie wholly before or after this container domain.
                let after = match value {
                    Value::List(_) => matches!(self, Self::Array(_)),
                    Value::Row(_) | Value::Record(_) | Value::Map(_) => true,
                    _ => false,
                };
                if after {
                    let last = key.last_mut().ok_or_else(invalid_container)?;
                    *last += 1;
                }
                Ok(false)
            }
        }
    }

    // Legacy-vector inputs retain both native Array and List carriers; key order follows the existing index for either representation.
    fn container_variant(self, value: &Value) -> Self {
        if self != Self::LegacyVector {
            return self;
        }
        match value {
            Value::List(_) | Value::Row(_) | Value::Record(_) | Value::Map(_) => {
                Self::List(ScalarIndexDomain::Integer)
            }
            _ => Self::Array(ScalarIndexDomain::Integer),
        }
    }

    fn list_leaf(self) -> Result<ScalarIndexDomain, SQLError> {
        match self {
            Self::List(leaf) => Ok(leaf),
            Self::Tensor => Ok(ScalarIndexDomain::Float),
            _ => Err(invalid_container()),
        }
    }

    fn append_list(
        self,
        values: &[Value],
        key: &mut IndexKey,
        control: &StorageReadControl,
    ) -> Result<(), SQLError> {
        let leaf = self.list_leaf()?;
        for value in values {
            check(control)?;
            if self == Self::Tensor && !matches!(value, Value::Null) {
                let Value::List(values) = value else {
                    return Err(invalid_container());
                };
                extend(key, &[1, 1, 11])?;
                Self::List(ScalarIndexDomain::Float).append_list(values, key, control)?;
            } else {
                append_leaf(leaf, value, key, control)?;
            }
        }
        extend(key, &[0])
    }
}

fn append_leaf(
    domain: ScalarIndexDomain,
    value: &Value,
    key: &mut IndexKey,
    control: &StorageReadControl,
) -> Result<(), SQLError> {
    if matches!(value, Value::Null) {
        return extend(key, &[2]);
    }
    extend(key, &[1])?;
    extend(key, &domain.encode(value, control)?)
}

fn append_leaf_bound(
    domain: ScalarIndexDomain,
    value: &Value,
    key: &mut IndexKey,
    control: &StorageReadControl,
) -> Result<bool, SQLError> {
    if matches!(value, Value::Null) {
        extend(key, &[2])?;
        return Ok(true);
    }
    extend(key, &[1])?;
    if let Some((_, end)) = domain.rank_limits() {
        let rank = domain.first_rank(value, false, control)?;
        if rank == end {
            extend(key, &domain.end_key(control)?)?;
            return Ok(false);
        }
        let exact = domain.first_rank(value, true, control)? > rank;
        let rank = u64::try_from(rank).map_err(|_| invalid_rank())?;
        let candidate = match domain {
            ScalarIndexDomain::Boolean => Value::Bool(rank != 0),
            ScalarIndexDomain::Integer => Value::Int((rank ^ (1 << 63)) as i64),
            ScalarIndexDomain::Float => Value::Float(rank_float(rank)),
            _ => return Err(invalid_rank()),
        };
        // Re-encoding canonicalizes the two zero ranks before appending a suffix.
        extend(key, &domain.encode(&candidate, control)?)?;
        return Ok(exact);
    }
    let Bound::Included(bound) = domain.native_bound(value, true, true, control)? else {
        return Err(invalid_container());
    };
    let exact = bound.len() > 2;
    extend(key, &bound)?;
    Ok(exact)
}

fn append_shape(
    array: &ArrayValue,
    key: &mut IndexKey,
    control: &StorageReadControl,
) -> Result<(), SQLError> {
    extend(key, &[0])?;
    for length in
        std::iter::once(array.dimensions().len()).chain(array.dimensions().iter().copied())
    {
        check(control)?;
        extend(
            key,
            &u64::try_from(length)
                .map_err(|_| invalid_container())?
                .to_be_bytes(),
        )?;
    }
    for lower in array.lower_bounds() {
        check(control)?;
        extend(key, &((*lower as u32) ^ (1 << 31)).to_be_bytes())?;
    }
    Ok(())
}

fn traversal_error(error: ArrayTraversalError) -> SQLError {
    match error {
        ArrayTraversalError::Memory(error) => resource_error(error),
        ArrayTraversalError::Cancelled(error) => SQLError::Cancelled(error),
    }
}

fn invalid_container() -> SQLError {
    SQLError::Internal("index value does not match its container comparison domain".into())
}
