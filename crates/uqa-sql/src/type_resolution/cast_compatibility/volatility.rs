//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Volatility of the coercion step a cast adds, as `contain_mutable_functions` sees it. A binary-coercible cast calls nothing, a function cast has its function's volatility, and an I/O conversion calls the source type's output function and the target type's input function. Domains coerce through their base types; `CoerceToDomain` itself counts as immutable, as in `PostgreSQL`.

use super::{array_element, base_type, cast_catalog_entry, CastMethod};
use crate::ast::{ColumnType, FunctionVolatility};

/// `pg_cast` functions of `PostgreSQL` 18.4 that are not immutable; every one is stable because it reads a session setting or the catalog.
const STABLE_CAST_FUNCTIONS: [i64; 13] = [
    1079, // regclass(text)
    1174, // timestamptz(date)
    1178, // date(timestamptz)
    1388, // timetz(timestamptz)
    2019, // time(timestamptz)
    2027, // timestamp(timestamptz)
    2028, // timestamptz(timestamp)
    2047, // timetz(time)
    2896, // xml(text)
    3811, // money(integer)
    3812, // money(bigint)
    3823, // numeric(money)
    3824, // money(numeric)
];

/// Volatility of evaluating `source::target` at run time. A literal of unknown type is converted during parse analysis instead and adds no run-time call.
#[must_use]
pub fn cast_volatility(source: &ColumnType, target: &ColumnType) -> FunctionVolatility {
    let source = base_type(source).without_type_modifiers();
    let target = base_type(target).without_type_modifiers();
    if source == target {
        return FunctionVolatility::Immutable;
    }
    match (&source, &target) {
        (ColumnType::Array(source), ColumnType::Array(target)) => {
            return cast_volatility(array_element(source), array_element(target));
        }
        (ColumnType::Array(_), _) | (_, ColumnType::Array(_)) => {
            return input_output_volatility(&source, &target);
        }
        _ => {}
    }
    match cast_catalog_entry(&source, &target).map(|entry| entry.method) {
        Some(CastMethod::Binary) => FunctionVolatility::Immutable,
        Some(CastMethod::Function { oid, .. }) if STABLE_CAST_FUNCTIONS.contains(&oid) => {
            FunctionVolatility::Stable
        }
        Some(CastMethod::Function { .. }) => FunctionVolatility::Immutable,
        Some(CastMethod::InputOutput) | None => input_output_volatility(&source, &target),
    }
}

fn input_output_volatility(source: &ColumnType, target: &ColumnType) -> FunctionVolatility {
    if output_is_stable(source) || input_is_stable(target) {
        FunctionVolatility::Stable
    } else {
        FunctionVolatility::Immutable
    }
}

/// Types whose `typoutput` depends on a session setting or the catalog.
fn output_is_stable(ty: &ColumnType) -> bool {
    matches!(
        ty,
        ColumnType::Date
            | ColumnType::Timestamp
            | ColumnType::TimestampPrecision(_)
            | ColumnType::TimestampTz
            | ColumnType::TimestampTzPrecision(_)
            | ColumnType::Interval
            | ColumnType::IntervalWithFields { .. }
            | ColumnType::Regproc
            | ColumnType::Regprocedure
            | ColumnType::Regclass
            | ColumnType::Regnamespace
            | ColumnType::Regrole
            | ColumnType::Regtype
            | ColumnType::AclItem
            | ColumnType::Record
            | ColumnType::Range(_)
            | ColumnType::Multirange(_)
            | ColumnType::Enum(_)
            | ColumnType::Array(_)
            | ColumnType::AnyArray
    )
}

/// Types whose `typinput` depends on a session setting or the catalog.
fn input_is_stable(ty: &ColumnType) -> bool {
    output_is_stable(ty)
        || matches!(
            ty,
            ColumnType::Time
                | ColumnType::TimePrecision(_)
                | ColumnType::TimeTz
                | ColumnType::TimeTzPrecision(_)
        )
}

#[cfg(test)]
mod tests {
    use super::cast_volatility;
    use crate::ast::{ColumnType, EnumTypeReference, FunctionVolatility};

    fn stable(source: &ColumnType, target: &ColumnType) -> bool {
        cast_volatility(source, target) == FunctionVolatility::Stable
    }

    #[test]
    fn casts_follow_postgresql_function_and_input_output_volatility() {
        let mood = ColumnType::Enum(EnumTypeReference {
            schema: "public".into(),
            name: "mood".into(),
            oid: 20_000,
            array_oid: 20_001,
        });
        assert!(!stable(&ColumnType::Integer, &ColumnType::Text));
        assert!(!stable(&ColumnType::Integer, &ColumnType::BigInteger));
        assert!(!stable(&ColumnType::Text, &ColumnType::Integer));
        assert!(stable(&mood, &ColumnType::Text));
        assert!(stable(&ColumnType::Text, &mood));
        assert!(!stable(&mood, &mood));
        assert!(stable(&ColumnType::TimestampTz, &ColumnType::Text));
        assert!(stable(&ColumnType::Date, &ColumnType::TimestampTz));
        assert!(stable(&ColumnType::Text, &ColumnType::Date));
        assert!(!stable(&ColumnType::Time, &ColumnType::Text));
        assert!(stable(&ColumnType::Text, &ColumnType::Time));
        assert!(stable(
            &ColumnType::Array(Box::new(ColumnType::Integer)),
            &ColumnType::Text
        ));
        assert!(!stable(
            &ColumnType::Array(Box::new(ColumnType::Integer)),
            &ColumnType::Array(Box::new(ColumnType::BigInteger))
        ));
        assert!(stable(
            &ColumnType::Array(Box::new(mood.clone())),
            &ColumnType::Array(Box::new(ColumnType::Text))
        ));
    }
}
