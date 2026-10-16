//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native range identity selects endpoint operations and optional canonicalization separately.

use super::{
    CanonicalRange, Ordering, Produced, ProductionControl, RangeSubtype, Result, SQLError,
};
use crate::ColumnType;
use uqa_core::{TemporalValue, Value};

type BoundComparison = fn(&Value, &Value, &ProductionControl<'_>) -> Result<Ordering>;
type BoundOutput = fn(&Value, &ProductionControl<'_>) -> Result<Produced<String>>;

pub(super) struct RangeSemantics {
    identity: RangeSubtype,
    endpoint: ColumnType,
    canonical: Option<DiscreteCanonicalizer>,
    compare: BoundComparison,
    output: BoundOutput,
}

static INTEGER: RangeSemantics =
    RangeSemantics::new(RangeSubtype::Integer, Some(DiscreteCanonicalizer::Integer));
static BIG_INTEGER: RangeSemantics = RangeSemantics::new(
    RangeSubtype::BigInteger,
    Some(DiscreteCanonicalizer::BigInteger),
);
static NUMERIC: RangeSemantics = RangeSemantics::new(RangeSubtype::Numeric, None);
static DATE: RangeSemantics =
    RangeSemantics::new(RangeSubtype::Date, Some(DiscreteCanonicalizer::Date));
static TIMESTAMP: RangeSemantics = RangeSemantics::new(RangeSubtype::Timestamp, None);
static TIMESTAMP_TZ: RangeSemantics = RangeSemantics::new(RangeSubtype::TimestampTz, None);

impl RangeSemantics {
    const fn new(identity: RangeSubtype, canonical: Option<DiscreteCanonicalizer>) -> Self {
        Self {
            identity,
            endpoint: identity.scalar_type(),
            canonical,
            compare: compare_native_bounds,
            output: output_native_bound,
        }
    }

    pub(super) const fn native(identity: RangeSubtype) -> &'static Self {
        // The range identity selects this operation; an integer endpoint alone does not imply it.
        match identity {
            RangeSubtype::Integer => &INTEGER,
            RangeSubtype::BigInteger => &BIG_INTEGER,
            RangeSubtype::Numeric => &NUMERIC,
            RangeSubtype::Date => &DATE,
            RangeSubtype::Timestamp => &TIMESTAMP,
            RangeSubtype::TimestampTz => &TIMESTAMP_TZ,
        }
    }

    pub(super) fn input(
        &self,
        value: &Value,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Value>> {
        let target = self.endpoint.sql_name_with_control(control)?;
        crate::expr::casting::cast_value_from_with_control(value, &target, None, control)
    }

    pub(super) fn compare(
        &self,
        left: &Value,
        right: &Value,
        control: &ProductionControl<'_>,
    ) -> Result<Ordering> {
        (self.compare)(left, right, control)
    }

    pub(super) fn output(
        &self,
        value: &Value,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>> {
        (self.output)(value, control)
    }
}

impl super::RangeConstruction for RangeSemantics {
    type Identity = RangeSubtype;

    fn identity(&self) -> RangeSubtype {
        self.identity
    }

    fn has_canonical_function(&self) -> bool {
        self.canonical.is_some()
    }

    fn input_bound(
        &self,
        value: &Value,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Value>> {
        self.input(value, control)
    }

    fn compare_bounds(
        &self,
        left: &Value,
        right: &Value,
        control: &ProductionControl<'_>,
    ) -> Result<Ordering> {
        self.compare(left, right, control)
    }

    fn canonicalize(
        &self,
        range: &CanonicalRange,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<CanonicalRange>>> {
        let Some(canonical) = &self.canonical else {
            return Ok(None);
        };
        // Only the selected discrete definitions reach this branch; their endpoints are inline values.
        let lower = super::construction::copy_bound(range.lower(), control)?;
        let upper = super::construction::copy_bound(range.upper(), control)?;
        let (lower, lower_memory) = lower.into_parts();
        let (upper, upper_memory) = upper.into_parts();
        let mut owned = super::construction::RangeWorkspace {
            value: CanonicalRange {
                subtype: range.subtype,
                lower,
                upper,
                lower_inclusive: range.lower_inclusive,
                upper_inclusive: range.upper_inclusive,
                empty: false,
            },
            memory: control.combine(lower_memory, upper_memory),
        };
        canonical.apply(&mut owned.value)?;
        if super::construction::bounds_are_empty(&owned.value, self, control)? {
            owned.value = CanonicalRange::empty(self.identity);
        }
        Ok(Some(control.finish(owned.value, owned.memory)?))
    }
}

fn compare_native_bounds(
    left: &Value,
    right: &Value,
    control: &ProductionControl<'_>,
) -> Result<Ordering> {
    control.check()?;
    match (left, right) {
        (Value::Decimal(left), Value::Decimal(right)) => Ok(left.cmp_with_control(right, control)?),
        // All other native range subtypes have inline integer or temporal carriers.
        _ => Ok(left.cmp(right)),
    }
}

fn output_native_bound(value: &Value, control: &ProductionControl<'_>) -> Result<Produced<String>> {
    match value {
        Value::Int(value) => Ok(control.format(format_args!("{value}"))?),
        Value::Decimal(value) => Ok(value.to_sql_string_with_control(control)?),
        Value::Temporal(value) => Ok(value.to_sql_string_with_control(control)?),
        _ => Err(SQLError::Internal(
            "incompatible canonical range bound".into(),
        )),
    }
}

enum DiscreteCanonicalizer {
    Integer,
    BigInteger,
    Date,
}

impl DiscreteCanonicalizer {
    fn apply(&self, range: &mut CanonicalRange) -> Result<()> {
        if !range.lower_inclusive {
            if let Some(value) = range.lower.as_ref().filter(|value| finite_bound(value)) {
                range.lower = Some(self.successor(value)?);
                range.lower_inclusive = true;
            }
        }
        if range.upper_inclusive {
            if let Some(value) = range.upper.as_ref().filter(|value| finite_bound(value)) {
                range.upper = Some(self.successor(value)?);
                range.upper_inclusive = false;
            }
        }
        Ok(())
    }

    fn successor(&self, value: &Value) -> Result<Value> {
        match (self, value) {
            (Self::Integer, Value::Int(value)) => i32::try_from(*value)
                .ok()
                .and_then(|value| value.checked_add(1))
                .map(|value| Value::Int(i64::from(value)))
                .ok_or_else(|| overflow("integer")),
            (Self::BigInteger, Value::Int(value)) => value
                .checked_add(1)
                .map(Value::Int)
                .ok_or_else(|| overflow("bigint")),
            (Self::Date, Value::Temporal(TemporalValue::Date { days })) => days
                .checked_add(1)
                .map(|days| Value::Temporal(TemporalValue::Date { days }))
                .ok_or_else(|| overflow("date")),
            _ => {
                let subtype = match self {
                    Self::Integer => RangeSubtype::Integer,
                    Self::BigInteger => RangeSubtype::BigInteger,
                    Self::Date => RangeSubtype::Date,
                };
                Err(SQLError::Internal(format!(
                    "range subtype {subtype:?} received incompatible bound {value:?}"
                )))
            }
        }
    }
}

fn finite_bound(value: &Value) -> bool {
    !matches!(value, Value::Temporal(temporal) if !temporal.is_finite())
}

fn overflow(type_name: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "22003".into(),
        message: format!("{type_name} out of range"),
    }
}
