//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` binary result fields, distinct from native retained Datum storage.

use crate::{expr::SQLValueCatalog, ColumnType, SQLError};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    TemporalValue, Value,
};

mod containers;
mod numeric;
#[cfg(test)]
mod tests;
mod traversal;

/// Encode one non-NULL result field using its declared type and the invoking catalog. The caller owns the field length and SQL NULL framing.
pub fn format_postgres_binary_with_catalog_and_control(
    value: &Value,
    ty: &ColumnType,
    catalog: Option<&dyn SQLValueCatalog>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<u8>>, SQLError> {
    let mut output = Encoder {
        bytes: ProductionVec::new(*control),
        catalog,
        control: *control,
    };
    output.value(value, ty)?;
    Ok(output.bytes.finish()?)
}

/// Resolve the output capability before examining a field's nullness, as the result receiver does for its selected descriptor.
pub(super) fn validate_type(
    ty: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    let declared = ty;
    let mut ty = ty;
    while let ColumnType::Domain { base, .. } = ty {
        control.check()?;
        ty = base;
    }
    if matches!(ty, ColumnType::Named(_)) {
        return Err(carrier_error());
    }
    if matches!(
        ty,
        ColumnType::AclItem | ColumnType::Vector(_) | ColumnType::Tensor(_)
    ) {
        return Err(SQLError::Routine {
            sqlstate: "42883".into(),
            message: format!(
                "no binary output function available for type {}",
                crate::catalog::type_metadata::column_type_name(declared)
            ),
        });
    }
    Ok(())
}

struct Encoder<'a, 'c> {
    bytes: ProductionVec<'c, u8>,
    catalog: Option<&'a dyn SQLValueCatalog>,
    control: ProductionControl<'c>,
}

impl Encoder<'_, '_> {
    fn write(&mut self, bytes: &[u8]) -> Result<(), SQLError> {
        Ok(self.bytes.extend_from_slice(bytes)?)
    }

    fn length(&mut self, length: usize) -> Result<(), SQLError> {
        self.write(
            &i32::try_from(length)
                .map_err(|_| size_error())?
                .to_be_bytes(),
        )
    }

    fn decoded_value(&mut self, value: &Value, ty: &ColumnType) -> Result<(), SQLError> {
        if let Value::Temporal(temporal) = value {
            return self.temporal(temporal, ty);
        }
        match (ty, value) {
            (ColumnType::SmallInteger, Value::Int(value)) => self.write(
                &i16::try_from(*value)
                    .map_err(|_| carrier_error())?
                    .to_be_bytes(),
            ),
            (ColumnType::Integer, Value::Int(value)) => self.write(
                &i32::try_from(*value)
                    .map_err(|_| carrier_error())?
                    .to_be_bytes(),
            ),
            (ColumnType::BigInteger, Value::Int(value)) => self.write(&value.to_be_bytes()),
            (
                ColumnType::Oid
                | ColumnType::Xid
                | ColumnType::Regproc
                | ColumnType::Regprocedure
                | ColumnType::Regclass
                | ColumnType::Regnamespace
                | ColumnType::Regcollation
                | ColumnType::Regrole
                | ColumnType::Regtype,
                Value::Int(value),
            ) => self.write(
                &u32::try_from(*value)
                    .map_err(|_| carrier_error())?
                    .to_be_bytes(),
            ),
            (ColumnType::Boolean, Value::Bool(value)) => self.write(&[u8::from(*value)]),
            (ColumnType::Void, Value::Void) => Ok(()),
            (ColumnType::Real, Value::Float(value)) => self.write(&(*value as f32).to_be_bytes()),
            (ColumnType::DoublePrecision, Value::Float(value)) => self.write(&value.to_be_bytes()),
            (ColumnType::Numeric { .. }, Value::Decimal(value)) => self.numeric(value),
            (ColumnType::Bytea, Value::Bytes(value)) => self.write(value),
            (ColumnType::Uuid, Value::Str(value)) => self.write(
                &crate::expr::parse_uuid_bytes_with_control(value, &self.control)?,
            ),
            (ColumnType::Bit(_) | ColumnType::VarBit(_), Value::BitString(value)) => {
                self.write(&value.bit_len().to_be_bytes())?;
                self.write(value.as_bytes())
            }
            (ColumnType::InternalChar, value) => {
                let byte =
                    crate::expr::internal_char::value_byte(value).ok_or_else(carrier_error)?;
                self.write(&[byte])
            }
            (ColumnType::JsonB, Value::JsonB(text)) => {
                self.write(&[1])?;
                self.write(text.as_bytes())
            }
            (
                ColumnType::Text
                | ColumnType::RefCursor
                | ColumnType::Name
                | ColumnType::Varchar(_)
                | ColumnType::Bpchar
                | ColumnType::Character(_)
                | ColumnType::Json
                | ColumnType::Xml
                | ColumnType::PgNodeTree
                | ColumnType::Enum(_),
                _,
            ) => {
                let text = super::format_postgres_text_with_catalog_and_control(
                    value,
                    ty,
                    self.catalog,
                    &self.control,
                )?;
                self.write(text.as_bytes())
            }
            _ => Err(carrier_error()),
        }
    }

    fn temporal(&mut self, value: &TemporalValue, ty: &ColumnType) -> Result<(), SQLError> {
        match (ty, value) {
            (ColumnType::Date, TemporalValue::Date { days }) => {
                let days = if matches!(*days, i32::MIN | i32::MAX) {
                    *days
                } else {
                    days.checked_sub(uqa_core::POSTGRES_EPOCH_DAYS)
                        .ok_or_else(carrier_error)?
                };
                self.write(&days.to_be_bytes())
            }
            (ColumnType::Time | ColumnType::TimePrecision(_), TemporalValue::Time { micros }) => {
                self.write(&micros.to_be_bytes())
            }
            (
                ColumnType::TimeTz | ColumnType::TimeTzPrecision(_),
                TemporalValue::TimeTz {
                    micros,
                    offset_seconds,
                },
            ) => {
                self.write(&micros.to_be_bytes())?;
                self.write(
                    &offset_seconds
                        .checked_neg()
                        .ok_or_else(carrier_error)?
                        .to_be_bytes(),
                )
            }
            (
                ColumnType::Timestamp | ColumnType::TimestampPrecision(_),
                TemporalValue::Timestamp { postgres_micros },
            )
            | (
                ColumnType::TimestampTz | ColumnType::TimestampTzPrecision(_),
                TemporalValue::TimestampTz { postgres_micros },
            ) => self.write(&postgres_micros.to_be_bytes()),
            (
                ColumnType::Interval | ColumnType::IntervalWithFields { .. },
                TemporalValue::Interval {
                    months,
                    days,
                    micros,
                },
            ) => {
                self.write(&micros.to_be_bytes())?;
                self.write(&days.to_be_bytes())?;
                self.write(&months.to_be_bytes())
            }
            _ => Err(carrier_error()),
        }
    }
}

fn carrier_error() -> SQLError {
    SQLError::Internal("binary result value does not match its declared SQL type".into())
}

pub(super) fn size_error() -> SQLError {
    SQLError::Routine {
        sqlstate: "54000".into(),
        message: "binary result exceeds its protocol length limit".into(),
    }
}
