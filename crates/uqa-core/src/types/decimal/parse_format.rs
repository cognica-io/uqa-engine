//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Decimal parsing, display formatting, canonical text, and serde representation.

use num_bigint::BigInt;
use num_traits::Signed;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::{canonical_finite_parts, DecimalRepr, DecimalValue};

impl DecimalValue {
    pub fn to_sql_string(&self) -> String {
        match self.repr() {
            DecimalRepr::Finite { coefficient, scale } => format_finite(coefficient, *scale),
            DecimalRepr::NegativeInfinity => "-Infinity".into(),
            DecimalRepr::PositiveInfinity => "Infinity".into(),
            DecimalRepr::NaN => "NaN".into(),
        }
    }

    pub fn to_canonical_string(&self) -> String {
        match self.repr() {
            DecimalRepr::Finite { coefficient, scale } => {
                let (coefficient, scale) = canonical_finite_parts(coefficient, *scale);
                format_finite(&coefficient, scale)
            }
            _ => self.to_sql_string(),
        }
    }

    /// Normalized base-10 coefficient and scale. The coefficient is returned as text because `PostgreSQL` numeric coefficients exceed primitive integer widths.
    pub fn canonical_parts(&self) -> (String, u32) {
        match self.repr() {
            DecimalRepr::Finite { coefficient, scale } => {
                let (coefficient, scale) = canonical_finite_parts(coefficient, *scale);
                (coefficient.to_string(), scale)
            }
            _ => (self.to_sql_string(), 0),
        }
    }

    pub fn sql_string_len(&self) -> usize {
        self.to_sql_string().len()
    }
}

fn format_finite(coefficient: &BigInt, scale: u32) -> String {
    let negative = coefficient.is_negative();
    let digits = coefficient.abs().to_str_radix(10);
    let sign = if negative { "-" } else { "" };
    if scale == 0 {
        return format!("{sign}{digits}");
    }
    let scale = scale as usize;
    if digits.len() > scale {
        let split = digits.len() - scale;
        format!("{sign}{}.{}", &digits[..split], &digits[split..])
    } else {
        format!("{sign}0.{}{digits}", "0".repeat(scale - digits.len()))
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
