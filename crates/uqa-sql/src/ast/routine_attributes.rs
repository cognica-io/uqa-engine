//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The attribute clauses a `CREATE FUNCTION` or `ALTER FUNCTION` statement writes, kept in written order for the checks `PostgreSQL` makes once the routine's schema, or the altered routine, is known.

use serde::{Deserialize, Serialize};

/// The clauses of a routine statement that `compute_function_attributes` and `compute_common_attribute` examine, and what they found that a later stage rejects. A statement carries them until registration checks them; stored definitions carry none.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutineAttributeClauses {
    /// Each attribute clause in written order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clauses: Vec<RoutineAttributeClause>,
    /// A PARALLEL value other than SAFE, RESTRICTED or UNSAFE, which `interpret_func_parallel` rejects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invalid_parallel: Option<String>,
    /// The types `TRANSFORM FOR TYPE` names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transform_types: Vec<RoutineTransformType>,
    /// The body form `interpret_AS_clause` rejects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_error: Option<RoutineBodyError>,
}

impl RoutineAttributeClauses {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// One attribute clause of a routine statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoutineAttributeClause {
    As,
    Language,
    Transform,
    Window,
    Volatility,
    Strict,
    Security,
    Leakproof,
    Set,
    Cost,
    Rows,
    Support,
    Parallel,
}

impl RoutineAttributeClause {
    /// Whether a procedure cannot have the attribute (`invalid attribute in procedure definition`).
    pub const fn rejected_by_procedures(self) -> bool {
        matches!(
            self,
            Self::Window
                | Self::Volatility
                | Self::Strict
                | Self::Leakproof
                | Self::Cost
                | Self::Rows
                | Self::Support
                | Self::Parallel
        )
    }

    /// Whether the clause may be written more than once; every SET clause applies.
    pub const fn repeatable(self) -> bool {
        matches!(self, Self::Set)
    }
}

/// A type that a `TRANSFORM FOR TYPE` clause names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutineTransformType {
    /// The name the catalog resolves.
    pub type_name: String,
    /// The name as written, as `TypeNameToString` spells it.
    pub written: String,
}

/// A body that `interpret_AS_clause` rejects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoutineBodyError {
    /// Neither an AS body nor a SQL-standard body.
    Missing,
    /// Both an AS body and a SQL-standard body.
    Duplicate,
    /// An AS clause with more than the one item the routine's language takes.
    ExtraAsItems,
}

/// Serde for a COST or ROWS estimate, a `float4` that overflows to infinity and that the JSON catalog cannot hold as a number: a finite estimate is a number, and an infinite one the text `PostgreSQL` prints for it.
pub mod routine_estimate {
    use serde::{Deserialize, Deserializer, Serializer};

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Estimate {
        Number(f32),
        Text(String),
    }

    pub fn serialize<S: Serializer>(value: &Option<f32>, serializer: S) -> Result<S::Ok, S::Error> {
        match value {
            None => serializer.serialize_none(),
            Some(value) if value.is_finite() => serializer.serialize_some(value),
            Some(value) if value.is_nan() => serializer.serialize_some("NaN"),
            Some(value) if value.is_sign_positive() => serializer.serialize_some("Infinity"),
            Some(_) => serializer.serialize_some("-Infinity"),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<f32>, D::Error> {
        Ok(match Option::<Estimate>::deserialize(deserializer)? {
            None => None,
            Some(Estimate::Number(value)) => Some(value),
            Some(Estimate::Text(text)) => Some(match text.as_str() {
                "Infinity" => f32::INFINITY,
                "-Infinity" => f32::NEG_INFINITY,
                "NaN" => f32::NAN,
                other => {
                    return Err(serde::de::Error::custom(format!(
                        "invalid routine estimate `{other}`"
                    )))
                }
            }),
        })
    }
}
