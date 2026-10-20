//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Builtin scalar conversion kernels selected from resolved SQL identities.

use super::{numeric::NumericCast, Produced, ProductionControl, Result, SQLError, Value};
use crate::{expr::EngineHook, ColumnType};

/// A conversion's immutable selection, without retaining a row value or a catalog decision.
#[derive(Clone, Debug)]
pub struct ResolvedCast {
    source: Source,
    target: Target,
}

#[derive(Clone, Copy, Debug)]
enum Source {
    Numeric,
    Real,
    InternalChar,
    Scalar,
}

#[derive(Clone, Copy, Debug)]
enum Target {
    Numeric(NumericCast),
    Text,
    Varchar(Option<u32>),
    Character(u32),
    Bpchar,
    Boolean,
    Uuid,
    Json { binary: bool },
}

impl Target {
    fn column_type(self) -> ColumnType {
        match self {
            Self::Numeric(numeric) => numeric.target_type(),
            Self::Text => ColumnType::Text,
            Self::Varchar(length) => ColumnType::Varchar(length),
            Self::Character(length) => ColumnType::Character(length),
            Self::Bpchar => ColumnType::Bpchar,
            Self::Boolean => ColumnType::Boolean,
            Self::Uuid => ColumnType::Uuid,
            Self::Json { binary: false } => ColumnType::Json,
            Self::Json { binary: true } => ColumnType::JsonB,
        }
    }
}

impl ResolvedCast {
    /// Select builtin scalar kernels whose identities do not depend on a live catalog or session setting. The caller still invokes current domain adapters and value production on every row.
    pub fn select(source: &ColumnType, target: &ColumnType) -> Option<Self> {
        let source = scalar_source(source)?;
        let target = match target {
            ColumnType::Text => Target::Text,
            ColumnType::Varchar(length) => Target::Varchar(*length),
            ColumnType::Character(length) => Target::Character(*length),
            ColumnType::Bpchar => Target::Bpchar,
            ColumnType::Boolean => Target::Boolean,
            ColumnType::Uuid => Target::Uuid,
            ColumnType::Json => Target::Json { binary: false },
            ColumnType::JsonB => Target::Json { binary: true },
            _ => Target::Numeric(NumericCast::select(target, target)?),
        };
        Some(Self { source, target })
    }

    /// The narrower numeric selection exposed by the existing numeric cast API.
    pub fn numeric(&self) -> Option<NumericCast> {
        match (self.source, self.target) {
            (Source::Numeric | Source::Real, Target::Numeric(numeric)) => Some(numeric),
            _ => None,
        }
    }

    /// Catalog-bearing carriers keep their existing physical output and live label paths even when a caller supplies a builtin source schema.
    pub(crate) fn accepts(&self, value: &Value) -> bool {
        matches!(
            value,
            Value::Null
                | Value::Int(_)
                | Value::Float(_)
                | Value::Decimal(_)
                | Value::Bool(_)
                | Value::Str(_)
                | Value::FixedChar(_)
                | Value::Bytes(_)
                | Value::Json(_)
                | Value::JsonB(_)
        ) || (self.numeric().is_some() && matches!(value, Value::Datum(_)))
    }

    /// Reuse only type selection. The original source and target spellings must identify this selection; borrowing them preserves fallback callback/error order without constructing another name. Current callbacks, cancellation, values and allocation admission remain part of each evaluation.
    pub fn evaluate_with_catalog(
        &self,
        value: &Value,
        source_name: &str,
        target_name: &str,
        engine: Option<&dyn EngineHook>,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Value>> {
        control.check()?;
        if !self.accepts(value) {
            return super::super::cast_value_with_type_resolution_with_control(
                value,
                Some(source_name),
                target_name,
                engine,
                control,
            );
        }
        if let Some(engine) = engine {
            if let Some(output) =
                engine.cast_domain(value, Some(source_name), &self.target.column_type())?
            {
                return Ok(control.retain_external_value(output)?);
            }
        }
        self.evaluate(value, control)
    }

    /// Run after the ordinary catalog checks, using the same scalar value constructors as named casts.
    pub(crate) fn evaluate(
        &self,
        value: &Value,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Value>> {
        control.check()?;
        if let Target::Numeric(numeric) = self.target {
            if matches!(self.source, Source::InternalChar)
                && matches!(numeric, NumericCast::Integer)
                && !matches!(value, Value::Null)
            {
                return Ok(control.finish(
                    super::binary_oid::cast_integer_from(value, Some("\"char\""), control)?,
                    control.empty_reservation(),
                )?);
            }
            return numeric.evaluate(value, control);
        }
        if matches!(value, Value::Null) {
            return Ok(control.finish(Value::Null, control.empty_reservation())?);
        }
        match self.target {
            Target::Text | Target::Varchar(_) | Target::Bpchar | Target::Character(_) => {
                self.text(value, control)
            }
            Target::Boolean => {
                Ok(control.finish(super::cast_boolean(value)?, control.empty_reservation())?)
            }
            Target::Uuid => super::cast_uuid(value, control),
            Target::Json { binary } => {
                super::super::json::cast_json_value_with_control(value, binary, control)
            }
            Target::Numeric(_) => unreachable!("numeric conversion already executed"),
        }
    }

    fn text(&self, value: &Value, control: &ProductionControl<'_>) -> Result<Produced<Value>> {
        let source = match self.source {
            Source::Real => Some(ColumnType::Real),
            Source::InternalChar => Some(ColumnType::InternalChar),
            Source::Numeric | Source::Scalar => None,
        };
        let text = match (self.target, value) {
            (Target::Bpchar, Value::FixedChar(text)) => control.copy_text(text)?,
            _ => super::cast_text_from_type(value, source.as_ref(), control)?,
        };
        let (fixed, length) = match self.target {
            Target::Bpchar => (true, None),
            Target::Character(length) => (true, Some(length)),
            Target::Varchar(length) => (false, length),
            _ => (false, None),
        };
        match length {
            Some(0) if fixed => Err(SQLError::TypeMismatch(
                "CHARACTER length must be greater than zero".into(),
            )),
            Some(length) => super::character_value(text, length as usize, fixed, control),
            None => super::text_value(text, fixed, control),
        }
    }
}

fn scalar_source(source: &ColumnType) -> Option<Source> {
    Some(match source {
        ColumnType::SmallInteger
        | ColumnType::Integer
        | ColumnType::BigInteger
        | ColumnType::DoublePrecision
        | ColumnType::Numeric { .. } => Source::Numeric,
        ColumnType::Real => Source::Real,
        ColumnType::InternalChar => Source::InternalChar,
        ColumnType::Boolean
        | ColumnType::Text
        | ColumnType::Name
        | ColumnType::Varchar(_)
        | ColumnType::Bpchar
        | ColumnType::Character(_)
        | ColumnType::Uuid
        | ColumnType::Json
        | ColumnType::JsonB
        | ColumnType::Bytea => Source::Scalar,
        _ => return None,
    })
}

#[cfg(test)]
mod tests;
