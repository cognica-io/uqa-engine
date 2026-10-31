//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Volatility of the coercion step a cast adds, as `contain_mutable_functions` sees it. A binary-coercible cast calls nothing, a function cast has its function's volatility, and an I/O conversion calls the source type's output function and the target type's input function. Domains coerce through their base types; `CoerceToDomain` itself counts as immutable, as in `PostgreSQL`.

use super::{array_element, base_type, CastMethod};

mod production;
use crate::ast::{ColumnType, FunctionVolatility};
pub use production::{cast_volatility_with_control, pg_catalog_builtin_type_with_control};

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
    cast_volatility_with_control(
        source,
        target,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .expect("ordinary cast volatility")
}

/// Targets whose conversion from a constant reads neither a catalog descriptor nor a session setting. Together with an immutable `cast_volatility`, the conversion can run once instead of at every evaluation.
#[must_use]
pub fn immutable_cast_type(ty: &ColumnType) -> bool {
    match ty {
        ColumnType::Array(element) => immutable_cast_type(element),
        ColumnType::SmallInteger
        | ColumnType::Integer
        | ColumnType::BigInteger
        | ColumnType::Oid
        | ColumnType::Xid
        | ColumnType::Boolean
        | ColumnType::Text
        | ColumnType::Name
        | ColumnType::Uuid
        | ColumnType::Varchar(_)
        | ColumnType::Bpchar
        | ColumnType::Character(_)
        | ColumnType::Real
        | ColumnType::DoublePrecision
        | ColumnType::Numeric { .. }
        | ColumnType::Json
        | ColumnType::JsonB
        | ColumnType::Bytea
        | ColumnType::InternalChar => true,
        _ => false,
    }
}

/// The builtin type named with an explicit `pg_catalog` namespace. Only such a spelling identifies its type without the live catalog; a free identifier can select a domain or another type through `search_path`.
#[must_use]
pub fn pg_catalog_builtin_type(name: &str) -> Option<ColumnType> {
    pg_catalog_builtin_type_with_control(name, &uqa_core::memory::ProductionControl::uncontrolled())
        .expect("ordinary builtin type identity")
        .map(|ty| ty.into_uncontrolled().expect("ordinary builtin type"))
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
            | ColumnType::Regcollation
            | ColumnType::Regnamespace
            | ColumnType::Regrole
            | ColumnType::Regtype
            | ColumnType::AclItem
            | ColumnType::Record
            | ColumnType::Composite(_)
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
                | ColumnType::Xml
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
        let record = ColumnType::Composite(crate::ast::CompositeTypeReference {
            schema: "public".into(),
            name: "pair".into(),
            oid: 20_002,
            array_oid: 20_003,
            relation_oid: 20_004,
        });
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
        assert!(stable(&record, &ColumnType::Text));
        assert!(stable(&ColumnType::Text, &record));
        assert!(!stable(&record, &record));
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
