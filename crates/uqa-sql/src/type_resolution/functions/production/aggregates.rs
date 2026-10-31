//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Aggregate result projections retain dynamic enum and array metadata.

use super::{inline, ColumnType, Produced, ProductionControl, SQLError};

/// The result type of the `min` or `max` overload that `PostgreSQL` 18 selects for an argument type, or `None` when no overload accepts it. An aggregate's result carries no type modifier; `varchar`, `name`, `"char"` and `pg_node_tree` reach `min(text)` by implicit coercion to the preferred string type, and so does an unknown argument. The OID alias types reach `min(oid)` by their implicit casts, and `int2vector` and `oidvector` match `anyarray` as array types.
pub(super) fn min_max_result_type(
    argument: Option<&ColumnType>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    if let Some(ty @ (ColumnType::Enum(_) | ColumnType::Array(_))) = argument {
        return ty
            .without_type_modifiers_with_control(control)
            .map(Some)
            .map_err(Into::into);
    }
    let ty = match argument {
        None
        | Some(
            ColumnType::Text
            | ColumnType::Varchar(_)
            | ColumnType::Name
            | ColumnType::InternalChar
            | ColumnType::PgNodeTree,
        ) => ColumnType::Text,
        Some(
            ColumnType::Regproc
            | ColumnType::Regprocedure
            | ColumnType::Regclass
            | ColumnType::Regcollation
            | ColumnType::Regnamespace
            | ColumnType::Regrole
            | ColumnType::Regtype,
        ) => ColumnType::Oid,
        Some(ty @ (ColumnType::Int2Vector | ColumnType::OidVector)) => ty.clone(),
        Some(ColumnType::Bpchar | ColumnType::Character(_)) => ColumnType::Bpchar,
        Some(ColumnType::Numeric { .. }) => ColumnType::Numeric {
            precision: None,
            scale: None,
        },
        Some(ColumnType::Time | ColumnType::TimePrecision(_)) => ColumnType::Time,
        Some(ColumnType::TimeTz | ColumnType::TimeTzPrecision(_)) => ColumnType::TimeTz,
        Some(ColumnType::Timestamp | ColumnType::TimestampPrecision(_)) => ColumnType::Timestamp,
        Some(ColumnType::TimestampTz | ColumnType::TimestampTzPrecision(_)) => {
            ColumnType::TimestampTz
        }
        Some(ColumnType::Interval | ColumnType::IntervalWithFields { .. }) => ColumnType::Interval,
        Some(
            ty @ (ColumnType::SmallInteger
            | ColumnType::Integer
            | ColumnType::BigInteger
            | ColumnType::Oid
            | ColumnType::Real
            | ColumnType::DoublePrecision
            | ColumnType::Date
            | ColumnType::Bytea
            | ColumnType::Record),
        ) => ty.clone(),
        Some(_) => return Ok(None),
    };
    inline(ty, control)
}
