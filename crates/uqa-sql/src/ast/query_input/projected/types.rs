//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::ast::{
    ColumnType, CompositeRowBinding, CompositeRowLayout, CompositeTypeReference, EnumTypeReference,
    IntervalFields, RangeAlignment, RangeSubtype, RangeTypeReference,
};

projected_enum!(ColumnType {
    Named(value),
    SmallInteger,
    Integer,
    BigInteger,
    Oid,
    Xid,
    Boolean,
    Void,
    Text,
    RefCursor,
    Name,
    Uuid,
    Varchar(value),
    Bpchar,
    Character(value),
    Real,
    DoublePrecision,
    Numeric { precision: optional, scale: optional },
    Json,
    Xml,
    JsonB,
    Bytea,
    Bit(value),
    VarBit(value),
    InternalChar,
    Regproc,
    Regprocedure,
    Regclass,
    Regnamespace,
    Regcollation,
    Regrole,
    Regtype,
    PgNodeTree,
    AclItem,
    Int2Vector,
    OidVector,
    AnyArray,
    Record,
    Array(value),
    Date,
    Time,
    TimePrecision(value),
    TimeTz,
    TimeTzPrecision(value),
    Timestamp,
    TimestampPrecision(value),
    TimestampTz,
    TimestampTzPrecision(value),
    Interval,
    IntervalWithFields { fields: required, precision: optional },
    Range(value),
    Multirange(value),
    DeclaredRange(value),
    DeclaredMultirange(value),
    Vector(value),
    Tensor(value),
    Domain { schema: required, name: required, oid: required, array_oid: defaulted, collation_oid: defaulted, base: required },
    Enum(value),
    Composite(value)
});

projected_enum!(RangeSubtype {
    Integer,
    BigInteger,
    Numeric,
    Date,
    Timestamp,
    TimestampTz
});

projected_enum!(RangeAlignment { Integer, Double });

projected_struct!(RangeTypeReference {
    schema: required,
    name: required,
    oid: required,
    array_oid: required,
    alignment: required
});

projected_struct!(EnumTypeReference {
    schema: required,
    name: required,
    oid: required,
    array_oid: required
});

projected_struct!(CompositeTypeReference {
    schema: required,
    name: required,
    oid: required,
    array_oid: required,
    relation_oid: required
});

projected_struct!(CompositeRowBinding {
    ty: required,
    attributes: required,
    layout: defaulted,
    argument_types: defaulted
} unknown(deny_unknown));

projected_struct!(CompositeRowLayout {
    width: required,
    implicit: required
} unknown(deny_unknown));

projected_enum!(IntervalFields {
    All,
    Year,
    Month,
    Day,
    Hour,
    Minute,
    Second,
    YearToMonth,
    DayToHour,
    DayToMinute,
    DayToSecond,
    HourToMinute,
    HourToSecond,
    MinuteToSecond
});
