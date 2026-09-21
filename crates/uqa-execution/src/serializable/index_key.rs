//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed scalar index addresses and predicate boundaries, independent of physical posting keys.

use std::{cmp::Ordering, ops::Bound};

use uqa_core::{memory::BudgetedVec, DecimalValue, Predicate, Value};
use uqa_sql::{ast::ColumnType, SQLError};
use uqa_storage::{read_control::StorageReadControl, StorageBackendError};

use crate::storage_errors::storage_error;

pub type IndexKey = BudgetedVec<u8>;

mod temporal;
pub use temporal::TemporalIndexDomain;

/// The stored column's comparison domain determines its key order. Numeric predicate bounds are projected into that domain; heterogeneous numeric values do not share an assumed universal byte order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalarIndexDomain {
    Boolean,
    Integer,
    Float,
    Decimal,
    Text,
    FixedChar,
    Bytes,
    JsonText,
    Temporal(TemporalIndexDomain),
}

pub struct IndexKeyRange {
    pub lower: Bound<IndexKey>,
    pub upper: Bound<IndexKey>,
}

impl IndexKeyRange {
    pub fn bounds(&self) -> (Bound<&[u8]>, Bound<&[u8]>) {
        (borrow_bound(&self.lower), borrow_bound(&self.upper))
    }
}

impl ScalarIndexDomain {
    pub fn from_column_type(ty: &ColumnType) -> Option<Self> {
        Some(match ty {
            ColumnType::Domain { base, .. } => return Self::from_column_type(base),
            ColumnType::Boolean => Self::Boolean,
            ColumnType::SmallInteger
            | ColumnType::Integer
            | ColumnType::BigInteger
            | ColumnType::Oid
            | ColumnType::Xid
            | ColumnType::Regproc
            | ColumnType::Regprocedure
            | ColumnType::Regclass
            | ColumnType::Regnamespace
            | ColumnType::Regrole
            | ColumnType::Regtype => Self::Integer,
            ColumnType::Real | ColumnType::DoublePrecision => Self::Float,
            ColumnType::Numeric { .. } => Self::Decimal,
            ColumnType::Text
            | ColumnType::RefCursor
            | ColumnType::Name
            | ColumnType::Uuid
            | ColumnType::Varchar(_)
            | ColumnType::InternalChar
            | ColumnType::PgNodeTree
            | ColumnType::AclItem
            | ColumnType::Range(_)
            | ColumnType::Multirange(_) => Self::Text,
            ColumnType::Bpchar | ColumnType::Character(_) => Self::FixedChar,
            ColumnType::Bytea => Self::Bytes,
            ColumnType::Json => Self::JsonText,
            _ => return TemporalIndexDomain::from_column_type(ty).map(Self::Temporal),
        })
    }

    fn tag(self) -> u8 {
        match self {
            Self::Boolean => 0,
            Self::Integer => 1,
            Self::Float => 2,
            Self::Decimal => 3,
            Self::Text => 4,
            Self::FixedChar => 5,
            Self::Bytes => 6,
            Self::JsonText => 7,
            Self::Temporal(_) => 8,
        }
    }

    /// Encode an already evaluated stored key. NULL is separate from the non-NULL B-tree, matching the query index's null posting set.
    pub fn encode(self, value: &Value, control: &StorageReadControl) -> Result<IndexKey, SQLError> {
        check(control)?;
        if matches!(value, Value::Null) {
            return bytes(control, &[0]);
        }
        match (self, value) {
            (Self::Boolean, Value::Bool(value)) => self.rank_key(u64::from(*value), control),
            (Self::Integer, Value::Int(value)) => {
                self.rank_key((*value as u64) ^ (1 << 63), control)
            }
            (Self::Float, Value::Float(value)) => self.rank_key(float_rank(*value), control),
            (Self::Decimal, Value::Decimal(value)) => self.decimal_key(value, control),
            (Self::Text, Value::Str(value)) | (Self::JsonText, Value::Json(value)) => {
                self.text_key(value.as_bytes(), control)
            }
            (Self::FixedChar, Value::FixedChar(value)) => {
                self.text_key(value.trim_end_matches(' ').as_bytes(), control)
            }
            (Self::Bytes, Value::Bytes(value)) => self.text_key(value, control),
            (Self::Temporal(_), Value::Temporal(value)) => {
                let mut key = self.start_key(control)?;
                value.write_comparison_key(|part| extend(&mut key, part))?;
                Ok(key)
            }
            _ => Err(SQLError::Internal(
                "stored index value does not match its comparison domain".into(),
            )),
        }
    }

    /// Visit the complete logical intervals selected by an eligible B-tree predicate, including intervals containing no stored rows. Index selection must precede this call; planner estimates and hydration do not call it.
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
            Predicate::IsNotNull => visitor(self.full_range(control)?),
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
        check(control)?;
        if let Some((start, end)) = self.rank_limits() {
            let lower = lower.map_or(Ok(start), |(value, inclusive)| {
                self.first_rank(value, !inclusive, control)
            })?;
            let upper = upper.map_or(Ok(end), |(value, inclusive)| {
                self.first_rank(value, inclusive, control)
            })?;
            if lower >= upper {
                return Ok(());
            }
            return visitor(IndexKeyRange {
                lower: Bound::Included(
                    self.rank_key(u64::try_from(lower).map_err(|_| invalid_rank())?, control)?,
                ),
                upper: Bound::Excluded(if upper == end {
                    self.end_key(control)?
                } else {
                    self.rank_key(u64::try_from(upper).map_err(|_| invalid_rank())?, control)?
                }),
            });
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

    fn full_range(self, control: &StorageReadControl) -> Result<IndexKeyRange, SQLError> {
        Ok(IndexKeyRange {
            lower: Bound::Included(self.start_key(control)?),
            upper: Bound::Excluded(self.end_key(control)?),
        })
    }

    fn start_key(self, control: &StorageReadControl) -> Result<IndexKey, SQLError> {
        bytes(control, &[1, self.tag()])
    }
    fn end_key(self, control: &StorageReadControl) -> Result<IndexKey, SQLError> {
        bytes(control, &[1, self.tag() + 1])
    }

    fn rank_key(self, rank: u64, control: &StorageReadControl) -> Result<IndexKey, SQLError> {
        let mut key = self.start_key(control)?;
        extend(&mut key, &rank.to_be_bytes())?;
        Ok(key)
    }

    fn rank_limits(self) -> Option<(u128, u128)> {
        match self {
            Self::Boolean => Some((0, 2)),
            Self::Integer => Some((0, 1_u128 << 64)),
            Self::Float => Some((
                u128::from(float_rank(f64::NEG_INFINITY)),
                u128::from(float_rank(f64::NAN)) + 1,
            )),
            _ => None,
        }
    }

    /// The finite scalar domain permits a bounded binary search using the same comparator as the selected index. This preserves fractional bounds and large mixed-numeric comparisons without rounding a predicate into a stored integer or float.
    fn first_rank(
        self,
        target: &Value,
        strict: bool,
        control: &StorageReadControl,
    ) -> Result<u128, SQLError> {
        let exact = match (self, target) {
            (Self::Boolean, Value::Bool(value)) => Some(u64::from(*value)),
            (Self::Integer, Value::Int(value)) => Some((*value as u64) ^ (1 << 63)),
            (Self::Float, Value::Float(value)) => Some(float_rank(*value)),
            _ => None,
        };
        if let Some(rank) = exact {
            return Ok(u128::from(rank) + u128::from(strict));
        }
        let _comparison = match target {
            Value::Decimal(value) => {
                let bytes = value
                    .retained_bytes()
                    .checked_mul(4)
                    .and_then(|bytes| {
                        bytes.checked_add(
                            (value.display_scale().unwrap_or(0) as usize + 1024).checked_mul(4)?,
                        )
                    })
                    .and_then(|bytes| bytes.checked_add(512))
                    .ok_or_else(|| resource_error(uqa_core::memory::MemoryError::SizeOverflow))?;
                Some(control.memory().reserve(bytes).map_err(resource_error)?)
            }
            _ => None,
        };
        let (mut low, mut high) = self.rank_limits().ok_or_else(invalid_rank)?;
        while low < high {
            check(control)?;
            let middle = low + (high - low) / 2;
            let rank = u64::try_from(middle).map_err(|_| invalid_rank())?;
            let value = match self {
                Self::Boolean => Value::Bool(rank != 0),
                Self::Integer => Value::Int((rank ^ (1 << 63)) as i64),
                Self::Float => Value::Float(rank_float(rank)),
                _ => return Err(invalid_rank()),
            };
            let ordering = value.cmp(target);
            if ordering == Ordering::Less || (strict && ordering == Ordering::Equal) {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        Ok(low)
    }

    fn bound(
        self,
        value: &Value,
        lower: bool,
        inclusive: bool,
        control: &StorageReadControl,
    ) -> Result<Bound<IndexKey>, SQLError> {
        let parsed = match (self, value) {
            (Self::Temporal(domain), Value::Str(text)) => {
                domain.parse(text, control)?.map(Value::Temporal)
            }
            _ => None,
        };
        let value = parsed.as_ref().unwrap_or(value);
        let key = match (self, value) {
            (Self::Decimal, Value::Decimal(value)) => self.decimal_key(value, control)?,
            (Self::Decimal, Value::Int(_) | Value::Bool(_) | Value::Float(_)) => {
                let _conversion = control.memory().reserve(2048).map_err(resource_error)?;
                let value = match value {
                    Value::Int(value) => DecimalValue::from_i64(*value),
                    Value::Bool(value) => DecimalValue::from_bool(*value),
                    Value::Float(value) => DecimalValue::from_f64_lossy(*value).ok_or_else(|| SQLError::Internal("numeric index predicate cannot be converted into its comparison domain".into()))?,
                    _ => unreachable!(),
                };
                self.decimal_key(&value, control)?
            }
            (Self::Text, Value::Str(_))
            | (Self::FixedChar, Value::FixedChar(_))
            | (Self::Bytes, Value::Bytes(_))
            | (Self::JsonText, Value::Json(_))
            | (Self::Temporal(_), Value::Temporal(_)) => self.encode(value, control)?,
            _ => {
                let sample = match self {
                    Self::Decimal => Value::Decimal(DecimalValue::from_i64(0)),
                    Self::Text => Value::Str(String::new()),
                    Self::FixedChar => Value::FixedChar(String::new()),
                    Self::Bytes => Value::Bytes(Vec::new()),
                    Self::JsonText => Value::Json(String::new()),
                    Self::Temporal(domain) => Value::Temporal(domain.sample()),
                    _ => return Err(invalid_rank()),
                };
                return match sample.cmp(value) {
                    Ordering::Greater => self.start_key(control).map(if lower {
                        Bound::Included
                    } else {
                        Bound::Excluded
                    }),
                    Ordering::Less => self.end_key(control).map(if lower {
                        Bound::Included
                    } else {
                        Bound::Excluded
                    }),
                    Ordering::Equal => Err(SQLError::Internal(
                        "index predicate comparison domain is ambiguous".into(),
                    )),
                };
            }
        };
        Ok(if inclusive {
            Bound::Included(key)
        } else {
            Bound::Excluded(key)
        })
    }

    fn text_key(self, value: &[u8], control: &StorageReadControl) -> Result<IndexKey, SQLError> {
        let mut key = self.start_key(control)?;
        for (index, byte) in value.iter().copied().enumerate() {
            if index % 256 == 0 {
                check(control)?;
            }
            if byte == 0 {
                extend(&mut key, &[0, 255])?;
            } else {
                extend(&mut key, &[byte])?;
            }
        }
        extend(&mut key, &[0, 0])?;
        Ok(key)
    }

    fn decimal_key(
        self,
        value: &DecimalValue,
        control: &StorageReadControl,
    ) -> Result<IndexKey, SQLError> {
        let mut key = self.start_key(control)?;
        let special = if value.is_negative_infinity() {
            Some(0)
        } else if value.is_zero() {
            Some(2)
        } else if value.is_positive_infinity() {
            Some(4)
        } else if value.is_nan() {
            Some(5)
        } else {
            None
        };
        if let Some(tag) = special {
            extend(&mut key, &[tag])?;
            return Ok(key);
        }
        let workspace = value
            .retained_bytes()
            .checked_mul(4)
            .and_then(|bytes| {
                bytes.checked_add((value.display_scale().unwrap_or(0) as usize).checked_mul(3)?)
            })
            .and_then(|bytes| bytes.checked_add(128))
            .ok_or_else(|| resource_error(uqa_core::memory::MemoryError::SizeOverflow))?;
        let _workspace = control
            .memory()
            .reserve(workspace)
            .map_err(resource_error)?;
        let text = value.to_sql_string();
        check(control)?;
        let negative = text.starts_with('-');
        let unsigned = text.strip_prefix('-').unwrap_or(&text);
        let decimal = unsigned.find('.').unwrap_or(unsigned.len());
        let first = unsigned
            .bytes()
            .position(|byte| byte != b'0' && byte != b'.')
            .ok_or_else(invalid_rank)?;
        let last = unsigned
            .bytes()
            .rposition(|byte| byte != b'0' && byte != b'.')
            .ok_or_else(invalid_rank)?;
        let preceding_digits = first - usize::from(decimal < first);
        let exponent = i64::try_from(decimal).map_err(|_| invalid_rank())?
            - i64::try_from(preceding_digits).map_err(|_| invalid_rank())?;
        let digits = unsigned[first..=last].bytes().filter(|byte| *byte != b'.');
        extend(&mut key, &[if negative { 1 } else { 3 }])?;
        for byte in ((exponent as u64) ^ (1 << 63))
            .to_be_bytes()
            .into_iter()
            .chain(digits)
            .chain(std::iter::once(0))
        {
            check(control)?;
            extend(&mut key, &[if negative { !byte } else { byte }])?;
        }
        Ok(key)
    }
}

fn float_rank(value: f64) -> u64 {
    if value.is_nan() {
        return float_rank(f64::INFINITY) + 1;
    }
    let bits = if value == 0.0 {
        0.0_f64.to_bits()
    } else {
        value.to_bits()
    };
    if bits >> 63 == 1 {
        !bits
    } else {
        bits ^ (1 << 63)
    }
}

fn rank_float(rank: u64) -> f64 {
    if rank == float_rank(f64::NAN) {
        return f64::NAN;
    }
    f64::from_bits(if rank >> 63 == 0 {
        !rank
    } else {
        rank ^ (1 << 63)
    })
}

fn nonempty(lower: &Bound<IndexKey>, upper: &Bound<IndexKey>) -> bool {
    match (lower, upper) {
        (Bound::Included(left), Bound::Included(right)) => left.as_ref() <= right.as_ref(),
        (
            Bound::Included(left) | Bound::Excluded(left),
            Bound::Included(right) | Bound::Excluded(right),
        ) => left.as_ref() < right.as_ref(),
        _ => true,
    }
}

fn borrow_bound(bound: &Bound<IndexKey>) -> Bound<&[u8]> {
    match bound {
        Bound::Included(value) => Bound::Included(value),
        Bound::Excluded(value) => Bound::Excluded(value),
        Bound::Unbounded => Bound::Unbounded,
    }
}

fn bytes(control: &StorageReadControl, value: &[u8]) -> Result<IndexKey, SQLError> {
    let mut result = BudgetedVec::new(control.memory());
    extend(&mut result, value)?;
    Ok(result)
}

fn extend(output: &mut IndexKey, value: &[u8]) -> Result<(), SQLError> {
    output.extend_from_slice(value).map_err(resource_error)
}
fn resource_error(error: uqa_core::memory::MemoryError) -> SQLError {
    storage_error(
        "encode serializable index key",
        &StorageBackendError::from(error),
    )
}
fn check(control: &StorageReadControl) -> Result<(), SQLError> {
    control
        .check()
        .map_err(|error| storage_error("encode serializable index key", &error))
}
fn invalid_rank() -> SQLError {
    SQLError::Internal("invalid scalar index rank".into())
}

#[cfg(test)]
mod tests;
