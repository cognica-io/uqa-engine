//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Static `PostgreSQL` type metadata and routine OID policy.

use crate::engine_user_functions::canonical_routine_type_name;
use uqa_sql::ast::{ColumnType, RangeSubtype};
use uqa_sql::SQLError;

use super::oids::stable_oid;

pub(in crate::sql::catalog) fn catalog_type_name(oid: i64) -> &'static str {
    match oid {
        16 => "boolean",
        17 => "bytea",
        20 => "bigint",
        21 => "smallint",
        23 => "integer",
        25 => "text",
        114 => "json",
        701 => "double precision",
        1184 => "timestamp with time zone",
        1186 => "interval",
        1700 => "numeric",
        2249 => "record",
        2277 => "anyarray",
        2950 => "uuid",
        3802 => "jsonb",
        _ => "USER-DEFINED",
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves catalog column and OID order"
)]
pub(in crate::sql::catalog) fn pg_type_oid(ty: &ColumnType) -> i64 {
    match ty {
        ColumnType::SmallInteger => 21,
        ColumnType::Integer => 23,
        ColumnType::BigInteger => 20,
        ColumnType::Oid => 26,
        ColumnType::Xid => 28,
        ColumnType::Boolean => 16,
        ColumnType::Text => 25,
        ColumnType::RefCursor => 1790,
        ColumnType::Name => 19,
        ColumnType::Uuid => 2950,
        ColumnType::Varchar(_) => 1043,
        ColumnType::Bpchar | ColumnType::Character(_) => 1042,
        ColumnType::Real => 700,
        ColumnType::DoublePrecision => 701,
        ColumnType::Numeric { .. } => 1700,
        ColumnType::Json => 114,
        ColumnType::JsonB => 3802,
        ColumnType::Bytea => 17,
        ColumnType::InternalChar => 18,
        ColumnType::Regproc => 24,
        ColumnType::Regprocedure => 2202,
        ColumnType::Regclass => 2205,
        ColumnType::Regnamespace => 4089,
        ColumnType::Regtype => 2206,
        ColumnType::PgNodeTree => 194,
        ColumnType::AclItem => 1033,
        ColumnType::Int2Vector => 22,
        ColumnType::OidVector => 30,
        ColumnType::AnyArray => 2277,
        ColumnType::Record => 2249,
        ColumnType::Range(subtype) => match subtype {
            RangeSubtype::Integer => 3904,
            RangeSubtype::Numeric => 3906,
            RangeSubtype::Timestamp => 3908,
            RangeSubtype::TimestampTz => 3910,
            RangeSubtype::Date => 3912,
            RangeSubtype::BigInteger => 3926,
        },
        ColumnType::Multirange(subtype) => match subtype {
            RangeSubtype::Integer => 4451,
            RangeSubtype::Numeric => 4532,
            RangeSubtype::Timestamp => 4533,
            RangeSubtype::TimestampTz => 4534,
            RangeSubtype::Date => 4535,
            RangeSubtype::BigInteger => 4536,
        },
        ColumnType::Array(element) => match element.as_ref() {
            ColumnType::SmallInteger => 1005,
            ColumnType::Integer => 1007,
            ColumnType::BigInteger => 1016,
            ColumnType::Oid => 1028,
            ColumnType::Xid => 1011,
            ColumnType::Boolean => 1000,
            ColumnType::Text => 1009,
            ColumnType::RefCursor => 2201,
            ColumnType::Name => 1003,
            ColumnType::Uuid => 2951,
            ColumnType::Varchar(_) => 1015,
            ColumnType::Bpchar | ColumnType::Character(_) => 1014,
            ColumnType::Real => 1021,
            ColumnType::DoublePrecision => 1022,
            ColumnType::Numeric { .. } => 1231,
            ColumnType::Json => 199,
            ColumnType::JsonB => 3807,
            ColumnType::Bytea => 1001,
            ColumnType::InternalChar => 1002,
            ColumnType::Regproc => 1008,
            ColumnType::Regprocedure => 2207,
            ColumnType::Regclass => 2210,
            ColumnType::Regnamespace => 4090,
            ColumnType::Regtype => 2211,
            ColumnType::PgNodeTree => 0,
            ColumnType::AclItem => 1034,
            ColumnType::Int2Vector => 1006,
            ColumnType::OidVector => 1013,
            ColumnType::AnyArray => 0,
            ColumnType::Record => 2287,
            ColumnType::Date => 1182,
            ColumnType::Time => 1183,
            ColumnType::TimeTz => 1270,
            ColumnType::Timestamp => 1115,
            ColumnType::TimestampTz => 1185,
            ColumnType::Interval => 1187,
            ColumnType::Vector(_) => 380_002,
            ColumnType::Tensor(_) => 380_003,
            ColumnType::Domain { oid, .. } => pg_domain_array_oid(*oid),
            ColumnType::Range(subtype) => match subtype {
                RangeSubtype::Integer => 3905,
                RangeSubtype::Numeric => 3907,
                RangeSubtype::Timestamp => 3909,
                RangeSubtype::TimestampTz => 3911,
                RangeSubtype::Date => 3913,
                RangeSubtype::BigInteger => 3927,
            },
            ColumnType::Multirange(subtype) => match subtype {
                RangeSubtype::Integer => 6150,
                RangeSubtype::Numeric => 6151,
                RangeSubtype::Timestamp => 6152,
                RangeSubtype::TimestampTz => 6153,
                RangeSubtype::Date => 6155,
                RangeSubtype::BigInteger => 6157,
            },
            ColumnType::Array(_) => pg_type_oid(element),
        },
        ColumnType::Date => 1082,
        ColumnType::Time => 1083,
        ColumnType::TimeTz => 1266,
        ColumnType::Timestamp => 1114,
        ColumnType::TimestampTz => 1184,
        ColumnType::Interval => 1186,
        ColumnType::Vector(_) => 380_000,
        ColumnType::Tensor(_) => 380_001,
        ColumnType::Domain { oid, .. } => i64::from(*oid),
    }
}

pub(in crate::sql::catalog) fn routine_type_oid(type_name: &str) -> i64 {
    let canonical = canonical_routine_type_name(type_name);
    let pseudo_type_oid = match canonical.as_str() {
        "record" => Some(2249),
        "cstring" => Some(2275),
        "any" => Some(2276),
        "anyarray" => Some(2277),
        "void" => Some(2278),
        "trigger" => Some(2279),
        "internal" => Some(2281),
        "anyelement" => Some(2283),
        "anynonarray" => Some(2776),
        "anyenum" => Some(3500),
        "anyrange" => Some(3831),
        "event_trigger" => Some(3838),
        "anymultirange" => Some(4537),
        "anycompatiblemultirange" => Some(4538),
        "anycompatible" => Some(5077),
        "anycompatiblearray" => Some(5078),
        "anycompatiblenonarray" => Some(5079),
        "anycompatiblerange" => Some(5080),
        _ => None,
    };
    if let Some(oid) = pseudo_type_oid {
        return oid;
    }
    if let Ok(column_type) = ColumnType::from_sql_name(&canonical) {
        return pg_type_oid(&column_type);
    }
    stable_oid("type", &canonical)
}

pub(in crate::sql::catalog) fn routine_variadic_element_oid(
    type_name: &str,
) -> Result<i64, SQLError> {
    let canonical = canonical_routine_type_name(type_name);
    match canonical.as_str() {
        "anyarray" => return Ok(2283),
        "anycompatiblearray" => return Ok(5077),
        "int2vector" => return Ok(21),
        "oidvector" => return Ok(26),
        _ => {}
    }
    let mut element = canonical.as_str();
    let Some(stripped) = element.strip_suffix("[]") else {
        return Err(SQLError::Internal(format!(
            "VARIADIC routine parameter `{type_name}` is not an array"
        )));
    };
    element = stripped;
    while let Some(stripped) = element.strip_suffix("[]") {
        element = stripped;
    }
    Ok(routine_type_oid(element))
}

#[cfg(test)]
mod routine_type_oid_tests {
    use super::{routine_type_oid, routine_variadic_element_oid};

    #[test]
    fn postgresql_18_routine_pseudo_type_oids_are_exact() {
        for (type_name, oid) in [
            ("record", 2249),
            ("cstring", 2275),
            ("any", 2276),
            ("anyarray", 2277),
            ("void", 2278),
            ("trigger", 2279),
            ("internal", 2281),
            ("anyelement", 2283),
            ("anynonarray", 2776),
            ("anyenum", 3500),
            ("anyrange", 3831),
            ("event_trigger", 3838),
            ("anymultirange", 4537),
            ("anycompatiblemultirange", 4538),
            ("anycompatible", 5077),
            ("anycompatiblearray", 5078),
            ("anycompatiblenonarray", 5079),
            ("anycompatiblerange", 5080),
        ] {
            assert_eq!(routine_type_oid(type_name), oid, "{type_name}");
        }
    }

    #[test]
    fn postgresql_18_variadic_element_oids_are_exact() {
        for (type_name, oid) in [
            ("integer[]", 23),
            ("integer[][]", 23),
            ("anyarray", 2283),
            ("anycompatiblearray", 5077),
            ("int2vector", 21),
            ("oidvector", 26),
        ] {
            assert_eq!(
                routine_variadic_element_oid(type_name).unwrap(),
                oid,
                "{type_name}"
            );
        }
        assert!(routine_variadic_element_oid("integer").is_err());
    }
}

pub(in crate::sql::catalog) fn pg_type_len(ty: &ColumnType) -> i64 {
    match ty {
        ColumnType::SmallInteger => 2,
        ColumnType::Integer
        | ColumnType::Oid
        | ColumnType::Xid
        | ColumnType::Regproc
        | ColumnType::Regprocedure
        | ColumnType::Regclass
        | ColumnType::Regnamespace
        | ColumnType::Regtype => 4,
        ColumnType::BigInteger => 8,
        ColumnType::Boolean | ColumnType::InternalChar => 1,
        ColumnType::Name => 64,
        ColumnType::Uuid | ColumnType::Interval | ColumnType::AclItem => 16,
        ColumnType::Real => 4,
        ColumnType::DoublePrecision | ColumnType::Timestamp | ColumnType::TimestampTz => 8,
        ColumnType::Date => 4,
        ColumnType::Time => 8,
        ColumnType::TimeTz => 12,
        ColumnType::Domain { base, .. } => pg_type_len(base),
        _ => -1,
    }
}

pub(in crate::sql::catalog) fn pg_type_by_value(ty: &ColumnType) -> bool {
    matches!(
        ty,
        ColumnType::SmallInteger
            | ColumnType::Integer
            | ColumnType::BigInteger
            | ColumnType::Oid
            | ColumnType::Xid
            | ColumnType::Boolean
            | ColumnType::InternalChar
            | ColumnType::Regproc
            | ColumnType::Regprocedure
            | ColumnType::Regclass
            | ColumnType::Regnamespace
            | ColumnType::Regtype
            | ColumnType::Real
            | ColumnType::DoublePrecision
            | ColumnType::Date
            | ColumnType::Time
            | ColumnType::Timestamp
            | ColumnType::TimestampTz
    ) || matches!(ty, ColumnType::Domain { base, .. } if pg_type_by_value(base))
}

pub(in crate::sql::catalog) fn pg_type_align(ty: &ColumnType) -> &'static str {
    match ty {
        ColumnType::Boolean | ColumnType::InternalChar | ColumnType::Name | ColumnType::Uuid => "c",
        ColumnType::SmallInteger => "s",
        ColumnType::BigInteger
        | ColumnType::DoublePrecision
        | ColumnType::AclItem
        | ColumnType::AnyArray
        | ColumnType::Time
        | ColumnType::TimeTz
        | ColumnType::Timestamp
        | ColumnType::TimestampTz
        | ColumnType::Interval
        | ColumnType::Range(
            RangeSubtype::BigInteger | RangeSubtype::Timestamp | RangeSubtype::TimestampTz,
        )
        | ColumnType::Multirange(
            RangeSubtype::BigInteger | RangeSubtype::Timestamp | RangeSubtype::TimestampTz,
        ) => "d",
        ColumnType::Array(element) if matches!(pg_type_align(element), "d") => "d",
        ColumnType::Domain { base, .. } => pg_type_align(base),
        _ => "i",
    }
}

pub(in crate::sql::catalog) fn pg_type_storage(ty: &ColumnType) -> &'static str {
    match ty {
        ColumnType::Numeric { .. } => "m",
        ColumnType::Text
        | ColumnType::RefCursor
        | ColumnType::Varchar(_)
        | ColumnType::Bpchar
        | ColumnType::Character(_)
        | ColumnType::Json
        | ColumnType::JsonB
        | ColumnType::Bytea
        | ColumnType::PgNodeTree
        | ColumnType::AnyArray
        | ColumnType::Array(_)
        | ColumnType::Range(_)
        | ColumnType::Multirange(_)
        | ColumnType::Vector(_)
        | ColumnType::Tensor(_) => "x",
        ColumnType::Domain { base, .. } => pg_type_storage(base),
        _ => "p",
    }
}

pub(in crate::sql::catalog) fn pg_type_array_oid(ty: &ColumnType) -> i64 {
    match ty {
        ColumnType::Array(_) => 0,
        ColumnType::Domain { oid, .. } => pg_domain_array_oid(*oid),
        other => pg_type_oid(&ColumnType::Array(Box::new(other.clone()))),
    }
}

fn pg_domain_array_oid(domain_oid: u32) -> i64 {
    match domain_oid {
        13_307 => 13_306,
        13_310 => 13_309,
        13_312 => 13_311,
        13_318 => 13_317,
        13_320 => 13_319,
        _ => 0,
    }
}

pub(in crate::sql::catalog) fn pg_type_element_oid(ty: &ColumnType) -> i64 {
    match ty {
        ColumnType::Name => 18,
        ColumnType::Int2Vector => 21,
        ColumnType::OidVector => 26,
        ColumnType::Array(element) => pg_type_oid(element),
        _ => 0,
    }
}

pub(in crate::sql::catalog) fn pg_type_collation_oid(ty: &ColumnType) -> i64 {
    let scalar = match ty {
        ColumnType::Array(element) => element.as_ref(),
        other => other,
    };
    match scalar {
        ColumnType::Name => 950,
        ColumnType::PgNodeTree => 100,
        ColumnType::Text
        | ColumnType::Varchar(_)
        | ColumnType::Bpchar
        | ColumnType::Character(_) => 100,
        ColumnType::Domain { oid, base, .. } => match oid {
            13_310 | 13_312 | 13_320 => 950,
            _ => pg_type_collation_oid(base),
        },
        _ => 0,
    }
}

pub(in crate::sql::catalog) fn pg_type_subscript_handler(ty: &ColumnType) -> &'static str {
    match ty {
        ColumnType::Array(_) => "array_subscript_handler",
        ColumnType::Int2Vector | ColumnType::OidVector => "array_subscript_handler",
        ColumnType::Name => "raw_array_subscript_handler",
        ColumnType::JsonB => "jsonb_subscript_handler",
        _ => "-",
    }
}

pub(in crate::sql::catalog) fn pg_type_modifier(ty: &ColumnType) -> i64 {
    match ty {
        // PostgreSQL stores varlena type modifiers with a four-byte header.
        ColumnType::Character(length) | ColumnType::Varchar(Some(length)) => i64::from(*length) + 4,
        _ => -1,
    }
}

#[derive(Debug, Clone, Copy)]
pub(in crate::sql::catalog) struct PgTypeRoutineOids {
    pub(in crate::sql::catalog) input: i64,
    pub(in crate::sql::catalog) output: i64,
    pub(in crate::sql::catalog) receive: i64,
    pub(in crate::sql::catalog) send: i64,
    pub(in crate::sql::catalog) modifier_input: i64,
    pub(in crate::sql::catalog) modifier_output: i64,
    pub(in crate::sql::catalog) analyze: i64,
}

impl PgTypeRoutineOids {
    const fn new(input: i64, output: i64, receive: i64, send: i64) -> Self {
        Self {
            input,
            output,
            receive,
            send,
            modifier_input: 0,
            modifier_output: 0,
            analyze: 0,
        }
    }

    const fn with_modifier(mut self, input: i64, output: i64) -> Self {
        self.modifier_input = input;
        self.modifier_output = output;
        self
    }
}

pub(in crate::sql::catalog) fn pg_type_routine_oids(ty: &ColumnType) -> PgTypeRoutineOids {
    if let ColumnType::Array(element) = ty {
        let mut routines = PgTypeRoutineOids::new(750, 751, 2400, 2401);
        routines.analyze = 3816;
        if !matches!(element.as_ref(), ColumnType::Domain { .. }) {
            let element_routines = pg_type_routine_oids(element);
            routines.modifier_input = element_routines.modifier_input;
            routines.modifier_output = element_routines.modifier_output;
        }
        return routines;
    }
    if let ColumnType::Domain { base, .. } = ty {
        let base = pg_type_routine_oids(base);
        return PgTypeRoutineOids::new(2597, base.output, 2598, base.send);
    }
    match ty {
        ColumnType::Boolean => PgTypeRoutineOids::new(1242, 1243, 2436, 2437),
        ColumnType::Bytea => PgTypeRoutineOids::new(1244, 31, 2412, 2413),
        ColumnType::InternalChar => PgTypeRoutineOids::new(1245, 33, 2434, 2435),
        ColumnType::Name => PgTypeRoutineOids::new(34, 35, 2422, 2423),
        ColumnType::BigInteger => PgTypeRoutineOids::new(460, 461, 2408, 2409),
        ColumnType::SmallInteger => PgTypeRoutineOids::new(38, 39, 2404, 2405),
        ColumnType::Int2Vector => PgTypeRoutineOids::new(40, 41, 2410, 2411),
        ColumnType::Integer => PgTypeRoutineOids::new(42, 43, 2406, 2407),
        ColumnType::Regproc => PgTypeRoutineOids::new(44, 45, 2444, 2445),
        ColumnType::Regprocedure => PgTypeRoutineOids::new(2212, 2213, 2446, 2447),
        ColumnType::Regclass => PgTypeRoutineOids::new(2218, 2219, 2452, 2453),
        ColumnType::Regnamespace => PgTypeRoutineOids::new(4084, 4085, 4087, 4088),
        ColumnType::Text => PgTypeRoutineOids::new(46, 47, 2414, 2415),
        ColumnType::RefCursor => PgTypeRoutineOids::new(46, 47, 2414, 2415),
        ColumnType::Oid => PgTypeRoutineOids::new(1798, 1799, 2418, 2419),
        ColumnType::Xid => PgTypeRoutineOids::new(50, 51, 2440, 2441),
        ColumnType::OidVector => PgTypeRoutineOids::new(54, 55, 2420, 2421),
        ColumnType::Json => PgTypeRoutineOids::new(321, 322, 323, 324),
        ColumnType::PgNodeTree => PgTypeRoutineOids::new(195, 196, 197, 198),
        ColumnType::Real => PgTypeRoutineOids::new(200, 201, 2424, 2425),
        ColumnType::DoublePrecision => PgTypeRoutineOids::new(214, 215, 2426, 2427),
        ColumnType::AclItem => PgTypeRoutineOids::new(1031, 1032, 0, 0),
        ColumnType::Bpchar | ColumnType::Character(_) => {
            PgTypeRoutineOids::new(1044, 1045, 2430, 2431).with_modifier(2913, 2914)
        }
        ColumnType::Varchar(_) => {
            PgTypeRoutineOids::new(1046, 1047, 2432, 2433).with_modifier(2915, 2916)
        }
        ColumnType::Date => PgTypeRoutineOids::new(1084, 1085, 2468, 2469),
        ColumnType::Time => {
            PgTypeRoutineOids::new(1143, 1144, 2470, 2471).with_modifier(2909, 2910)
        }
        ColumnType::Timestamp => {
            PgTypeRoutineOids::new(1312, 1313, 2474, 2475).with_modifier(2905, 2906)
        }
        ColumnType::TimestampTz => {
            PgTypeRoutineOids::new(1150, 1151, 2476, 2477).with_modifier(2907, 2908)
        }
        ColumnType::Interval => {
            PgTypeRoutineOids::new(1160, 1161, 2478, 2479).with_modifier(2903, 2904)
        }
        ColumnType::TimeTz => {
            PgTypeRoutineOids::new(1350, 1351, 2472, 2473).with_modifier(2911, 2912)
        }
        ColumnType::Numeric { .. } => {
            PgTypeRoutineOids::new(1701, 1702, 2460, 2461).with_modifier(2917, 2918)
        }
        ColumnType::Regtype => PgTypeRoutineOids::new(2220, 2221, 2454, 2455),
        ColumnType::AnyArray => PgTypeRoutineOids::new(2296, 2297, 2502, 2503),
        ColumnType::Record => PgTypeRoutineOids::new(2290, 2291, 2402, 2403),
        ColumnType::Uuid => PgTypeRoutineOids::new(2952, 2953, 2961, 2962),
        ColumnType::JsonB => PgTypeRoutineOids::new(3806, 3804, 3805, 3803),
        ColumnType::Range(_) => {
            let mut routines = PgTypeRoutineOids::new(3834, 3835, 3836, 3837);
            routines.analyze = 3916;
            routines
        }
        ColumnType::Multirange(_) => {
            let mut routines = PgTypeRoutineOids::new(4231, 4232, 4233, 4234);
            routines.analyze = 4242;
            routines
        }
        ColumnType::Vector(_) | ColumnType::Tensor(_) => PgTypeRoutineOids::new(0, 0, 0, 0),
        ColumnType::Array(_) | ColumnType::Domain { .. } => {
            unreachable!("array and domain type routines are handled before scalar dispatch")
        }
    }
}
