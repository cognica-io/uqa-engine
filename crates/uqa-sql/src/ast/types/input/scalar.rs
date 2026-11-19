//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, ColumnType, InputError};
use crate::ast::{IntervalFields, RangeAlignment, RangeSubtype};

pub(super) fn column(tag: &str) -> Option<ColumnType> {
    Some(match tag {
        "SmallInteger" => ColumnType::SmallInteger,
        "Integer" => ColumnType::Integer,
        "BigInteger" => ColumnType::BigInteger,
        "Oid" => ColumnType::Oid,
        "Xid" => ColumnType::Xid,
        "Boolean" => ColumnType::Boolean,
        "Void" => ColumnType::Void,
        "Text" => ColumnType::Text,
        "RefCursor" => ColumnType::RefCursor,
        "Name" => ColumnType::Name,
        "Uuid" => ColumnType::Uuid,
        "Bpchar" => ColumnType::Bpchar,
        "Real" => ColumnType::Real,
        "DoublePrecision" => ColumnType::DoublePrecision,
        "Json" => ColumnType::Json,
        "Xml" => ColumnType::Xml,
        "JsonB" => ColumnType::JsonB,
        "Bytea" => ColumnType::Bytea,
        "InternalChar" => ColumnType::InternalChar,
        "Regproc" => ColumnType::Regproc,
        "Regprocedure" => ColumnType::Regprocedure,
        "Regclass" => ColumnType::Regclass,
        "Regnamespace" => ColumnType::Regnamespace,
        "Regcollation" => ColumnType::Regcollation,
        "Regrole" => ColumnType::Regrole,
        "Regtype" => ColumnType::Regtype,
        "PgNodeTree" => ColumnType::PgNodeTree,
        "AclItem" => ColumnType::AclItem,
        "Int2Vector" => ColumnType::Int2Vector,
        "OidVector" => ColumnType::OidVector,
        "AnyArray" => ColumnType::AnyArray,
        "Record" => ColumnType::Record,
        "Date" => ColumnType::Date,
        "Time" => ColumnType::Time,
        "TimeTz" => ColumnType::TimeTz,
        "Timestamp" => ColumnType::Timestamp,
        "TimestampTz" => ColumnType::TimestampTz,
        "Interval" => ColumnType::Interval,
        _ => return None,
    })
}

pub(super) fn range(tag: &str) -> Result<RangeSubtype, InputError> {
    Ok(match tag {
        "Integer" => RangeSubtype::Integer,
        "BigInteger" => RangeSubtype::BigInteger,
        "Numeric" => RangeSubtype::Numeric,
        "Date" => RangeSubtype::Date,
        "Timestamp" => RangeSubtype::Timestamp,
        "TimestampTz" => RangeSubtype::TimestampTz,
        _ => return Err(invalid()),
    })
}

pub(super) fn alignment(tag: &str) -> Result<RangeAlignment, InputError> {
    Ok(match tag {
        "Integer" => RangeAlignment::Integer,
        "Double" => RangeAlignment::Double,
        _ => return Err(invalid()),
    })
}

pub(super) fn interval(tag: &str) -> Result<IntervalFields, InputError> {
    Ok(match tag {
        "All" => IntervalFields::All,
        "Year" => IntervalFields::Year,
        "Month" => IntervalFields::Month,
        "Day" => IntervalFields::Day,
        "Hour" => IntervalFields::Hour,
        "Minute" => IntervalFields::Minute,
        "Second" => IntervalFields::Second,
        "YearToMonth" => IntervalFields::YearToMonth,
        "DayToHour" => IntervalFields::DayToHour,
        "DayToMinute" => IntervalFields::DayToMinute,
        "DayToSecond" => IntervalFields::DayToSecond,
        "HourToMinute" => IntervalFields::HourToMinute,
        "HourToSecond" => IntervalFields::HourToSecond,
        "MinuteToSecond" => IntervalFields::MinuteToSecond,
        _ => return Err(invalid()),
    })
}
