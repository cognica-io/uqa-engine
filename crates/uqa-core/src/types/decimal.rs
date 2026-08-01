//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact base-10 decimal values and their stable serialization.

use super::{
    Decimal, Deserialize, Deserializer, FromStr, Ordering, RoundingStrategy, Serialize, Serializer,
};

/// Exact base-10 numeric value for `PostgreSQL` `NUMERIC` / `DECIMAL`.
///
/// The JSON representation is tagged so persisted document values do not
/// collide with ordinary JSON strings, numbers, or maps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecimalValue {
    inner: Decimal,
}

impl DecimalValue {
    pub fn new(inner: Decimal) -> Self {
        Self { inner }
    }

    pub fn parse(input: &str) -> Option<Self> {
        Decimal::from_str(input.trim()).ok().map(Self::new)
    }

    pub fn from_i64(value: i64) -> Self {
        Self::new(Decimal::from(value))
    }

    pub fn from_bool(value: bool) -> Self {
        Self::from_i64(i64::from(value))
    }

    pub fn from_f64_lossy(value: f64) -> Option<Self> {
        if !value.is_finite() {
            return None;
        }
        let parsed = Self::parse(&value.to_string())?;
        if value != 0.0 && parsed.is_zero() {
            return None;
        }
        Some(parsed)
    }

    pub fn is_zero(&self) -> bool {
        self.inner.is_zero()
    }

    pub fn checked_add(&self, rhs: &Self) -> Option<Self> {
        self.inner.checked_add(rhs.inner).map(Self::new)
    }

    pub fn checked_sub(&self, rhs: &Self) -> Option<Self> {
        self.inner.checked_sub(rhs.inner).map(Self::new)
    }

    pub fn checked_mul(&self, rhs: &Self) -> Option<Self> {
        self.inner.checked_mul(rhs.inner).map(Self::new)
    }

    pub fn checked_div(&self, rhs: &Self) -> Option<Self> {
        self.inner.checked_div(rhs.inner).map(Self::new)
    }

    pub fn checked_rem(&self, rhs: &Self) -> Option<Self> {
        self.inner.checked_rem(rhs.inner).map(Self::new)
    }

    pub fn abs(&self) -> Self {
        if self.inner < Decimal::from(0) {
            Self::new(-self.inner)
        } else {
            self.clone()
        }
    }

    pub fn ceil(&self) -> Self {
        Self::new(self.inner.ceil())
    }

    pub fn floor(&self) -> Self {
        Self::new(self.inner.floor())
    }

    pub fn trunc(&self) -> Self {
        Self::new(self.inner.trunc())
    }

    pub fn round_dp(&self, scale: u32) -> Self {
        Self::new(
            self.inner
                .round_dp_with_strategy(scale, RoundingStrategy::MidpointAwayFromZero),
        )
    }

    pub fn round_to_scale(&self, scale: i32) -> Option<Self> {
        if scale >= 0 {
            return Some(self.round_dp(u32::try_from(scale).ok()?));
        }
        let factor = decimal_pow10(scale.unsigned_abs())?;
        self.inner
            .checked_div(factor)
            .map(|value| value.round_dp_with_strategy(0, RoundingStrategy::MidpointAwayFromZero))
            .and_then(|value| value.checked_mul(factor))
            .map(Self::new)
    }

    pub fn trunc_to_scale(&self, scale: i32) -> Option<Self> {
        if scale >= 0 {
            return Some(Self::new(
                self.inner.trunc_with_scale(u32::try_from(scale).ok()?),
            ));
        }
        let factor = decimal_pow10(scale.unsigned_abs())?;
        self.inner
            .checked_div(factor)
            .map(|value| value.trunc())
            .and_then(|value| value.checked_mul(factor))
            .map(Self::new)
    }

    pub fn fits_precision(&self, precision: u32, scale: i32) -> bool {
        let Some(factor) = decimal_pow10(scale.unsigned_abs()) else {
            return false;
        };
        let scaled = if scale >= 0 {
            self.inner.checked_mul(factor)
        } else {
            self.inner.checked_div(factor)
        };
        let Some(scaled) = scaled else {
            return false;
        };
        let Ok(precision) = usize::try_from(precision) else {
            return false;
        };
        decimal_integer_digit_count(scaled) <= precision
    }

    pub fn to_sql_string(&self) -> String {
        self.inner.to_string()
    }

    pub fn to_canonical_string(&self) -> String {
        self.inner.normalize().to_string()
    }

    pub fn to_i64_trunc(&self) -> Option<i64> {
        self.inner.trunc().to_string().parse::<i64>().ok()
    }

    pub fn to_f64(&self) -> Option<f64> {
        self.inner.to_string().parse::<f64>().ok()
    }
}

fn decimal_pow10(power: u32) -> Option<Decimal> {
    let mut value = Decimal::from(1);
    for _ in 0..power {
        value = value.checked_mul(Decimal::from(10))?;
    }
    Some(value)
}

fn decimal_integer_digit_count(value: Decimal) -> usize {
    let text = value.abs().trunc().normalize().to_string();
    let digits = text.trim_start_matches('0');
    digits.len().max(1)
}

impl PartialOrd for DecimalValue {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for DecimalValue {
    fn cmp(&self, other: &Self) -> Ordering {
        self.inner.cmp(&other.inner)
    }
}

impl Serialize for DecimalValue {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        #[derive(Serialize)]
        struct TaggedDecimal<'a> {
            #[serde(rename = "$uqa_type")]
            kind: &'a str,
            value: String,
        }

        TaggedDecimal {
            kind: "decimal",
            value: self.to_sql_string(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for DecimalValue {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct TaggedDecimal {
            #[serde(rename = "$uqa_type")]
            kind: String,
            value: String,
        }

        let tagged = TaggedDecimal::deserialize(deserializer)?;
        if tagged.kind != "decimal" {
            return Err(serde::de::Error::custom("not a decimal value"));
        }
        Self::parse(&tagged.value).ok_or_else(|| serde::de::Error::custom("invalid decimal value"))
    }
}
