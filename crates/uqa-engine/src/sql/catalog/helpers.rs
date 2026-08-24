//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared `PostgreSQL` catalog identifiers, type mappings, and row constructors.

use super::{
    canonical_routine_type_name, column_type_name, ColumnType, Engine, Expr, RelationIdentity,
    ResultRow, SQLColumnDef, SQLError, Value,
};
use uqa_core::ArrayValue;
use uqa_sql::ast::{
    ForeignKey, ForeignKeyAction, ForeignKeyMatch, TableKeyConstraintKind, WindowFrame, WindowSpec,
};

pub(super) use super::expression_text::{default_expr_text, schema_expr_text};

pub(super) fn catalog_name() -> Value {
    Value::Str("uqa".into())
}

pub(super) fn catalog_usize(value: usize, label: &str) -> Result<i64, SQLError> {
    i64::try_from(value).map_err(|_| {
        SQLError::Internal(format!(
            "{label} exceeds the SQL catalog BIGINT representation"
        ))
    })
}

pub(super) fn catalog_ordinal(index: usize, label: &str) -> Result<i64, SQLError> {
    let ordinal = index
        .checked_add(1)
        .ok_or_else(|| SQLError::Internal(format!("{label} ordinal overflow")))?;
    catalog_usize(ordinal, label)
}

pub(super) fn str_value(value: impl Into<String>) -> Value {
    Value::Str(value.into())
}

pub(super) fn int_value(value: i64) -> Value {
    Value::Int(value)
}

pub(super) fn bool_value(value: bool) -> Value {
    Value::Bool(value)
}

pub(super) fn list_int(values: &[i64]) -> Value {
    Value::List(values.iter().copied().map(Value::Int).collect())
}

pub(super) fn catalog_array(values: Vec<Value>, label: &str) -> Result<Value, SQLError> {
    ArrayValue::try_new(values)
        .map(Value::Array)
        .ok_or_else(|| SQLError::Internal(format!("{label} has non-rectangular dimensions")))
}

pub(super) fn row(entries: impl IntoIterator<Item = (&'static str, Value)>) -> ResultRow {
    let mut out = ResultRow::new();
    for (key, value) in entries {
        out.insert(key.to_string(), value);
    }
    out
}
pub(super) fn catalog_type_name(oid: i64) -> &'static str {
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

pub(super) fn split_schema_name(name: &str) -> Result<(String, String), SQLError> {
    let relation = RelationIdentity::from_legacy_name(name).map_err(|error| {
        SQLError::Internal(format!("invalid catalog relation `{name}`: {error}"))
    })?;
    Ok((relation.schema, relation.name))
}

pub(super) fn split_index_name(
    index_name: &str,
    table_schema: &str,
) -> Result<(String, String), SQLError> {
    let (schema, name) = RelationIdentity::parse_reference(index_name).map_err(|error| {
        SQLError::Internal(format!("invalid catalog index `{index_name}`: {error}"))
    })?;
    Ok((schema.unwrap_or_else(|| table_schema.to_string()), name))
}

pub(super) fn stable_oid(kind: &str, name: &str) -> i64 {
    let mut hash = 14_695_981_039_346_656_037_u64;
    for byte in kind.bytes().chain(*b":").chain(name.bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(1_099_511_628_211);
    }
    10_000 + i64::try_from(hash % 2_000_000_000).unwrap_or(0)
}

pub(super) fn schema_oid(schema: &str) -> i64 {
    match schema {
        "pg_catalog" => 11,
        "public" => 2200,
        "information_schema" => 13_293,
        other => stable_oid("namespace", other),
    }
}

pub(super) fn relation_oid(kind: &str, schema: &str, name: &str) -> i64 {
    stable_oid(kind, &format!("{schema}.{name}"))
}

pub(super) fn current_user_oid() -> i64 {
    10
}

pub(super) fn current_user_name() -> &'static str {
    "uqa"
}

pub(super) fn all_schema_names(engine: &Engine) -> Result<Vec<String>, SQLError> {
    let mut schemas = vec!["pg_catalog".to_string(), "information_schema".to_string()];
    schemas.extend(
        engine
            .list_schemas()
            .map_err(|err| SQLError::Internal(format!("read schema catalog: {err}")))?,
    );
    schemas.extend(super::ag_catalog::age_namespace_names(engine)?);
    schemas.sort();
    schemas.dedup();
    Ok(schemas)
}

pub(super) fn table_columns_for(
    engine: &Engine,
    table: &str,
) -> Result<Vec<SQLColumnDef>, SQLError> {
    Ok(engine
        .describe_table(table)
        .map_err(|err| SQLError::Internal(format!("read table schema: {err}")))?
        .unwrap_or_default())
}

pub(super) fn view_columns_for(engine: &Engine, view: &str) -> Result<Vec<SQLColumnDef>, SQLError> {
    let schema = engine.view_schema(view)?.ok_or_else(|| {
        SQLError::Internal(format!(
            "view `{view}` disappeared while reading its catalog schema"
        ))
    })?;
    Ok(schema
        .columns()
        .iter()
        .enumerate()
        .map(|(position, name)| SQLColumnDef {
            name: schema.public_name(position).unwrap_or(name).to_string(),
            ty: schema
                .column_type(position)
                .cloned()
                .unwrap_or(ColumnType::Text),
            primary_key: false,
            not_null: false,
            not_null_explicit: false,
            not_null_name: None,
            auto_increment: false,
            unique: false,
            default: None,
            generated: None,
            check: None,
            check_name: None,
            check_enforced: true,
            references: None,
        })
        .collect())
}

pub(super) fn pg_type_oid(ty: &ColumnType) -> i64 {
    match ty {
        ColumnType::SmallInteger => 21,
        ColumnType::Integer => 23,
        ColumnType::BigInteger => 20,
        ColumnType::Oid => 26,
        ColumnType::Xid => 28,
        ColumnType::Boolean => 16,
        ColumnType::Text => 25,
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
        ColumnType::Regclass => 2205,
        ColumnType::Regnamespace => 4089,
        ColumnType::Regtype => 2206,
        ColumnType::PgNodeTree => 194,
        ColumnType::AclItem => 1033,
        ColumnType::Int2Vector => 22,
        ColumnType::OidVector => 30,
        ColumnType::AnyArray => 2277,
        ColumnType::Record => 2249,
        ColumnType::Array(element) => match element.as_ref() {
            ColumnType::SmallInteger => 1005,
            ColumnType::Integer => 1007,
            ColumnType::BigInteger => 1016,
            ColumnType::Oid => 1028,
            ColumnType::Xid => 1011,
            ColumnType::Boolean => 1000,
            ColumnType::Text => 1009,
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

pub(super) fn routine_type_oid(type_name: &str) -> i64 {
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

pub(super) fn routine_variadic_element_oid(type_name: &str) -> Result<i64, SQLError> {
    let canonical = canonical_routine_type_name(type_name);
    match canonical.as_str() {
        "anyarray" => return Ok(2283),
        "anycompatiblearray" => return Ok(5077),
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

pub(super) fn pg_type_len(ty: &ColumnType) -> i64 {
    match ty {
        ColumnType::SmallInteger => 2,
        ColumnType::Integer
        | ColumnType::Oid
        | ColumnType::Xid
        | ColumnType::Regproc
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

pub(super) fn pg_type_by_value(ty: &ColumnType) -> bool {
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

pub(super) fn pg_type_align(ty: &ColumnType) -> &'static str {
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
        | ColumnType::Interval => "d",
        ColumnType::Array(element) if matches!(pg_type_align(element), "d") => "d",
        ColumnType::Domain { base, .. } => pg_type_align(base),
        _ => "i",
    }
}

pub(super) fn pg_type_storage(ty: &ColumnType) -> &'static str {
    match ty {
        ColumnType::Numeric { .. } => "m",
        ColumnType::Text
        | ColumnType::Varchar(_)
        | ColumnType::Bpchar
        | ColumnType::Character(_)
        | ColumnType::Json
        | ColumnType::JsonB
        | ColumnType::Bytea
        | ColumnType::PgNodeTree
        | ColumnType::AnyArray
        | ColumnType::Array(_)
        | ColumnType::Vector(_)
        | ColumnType::Tensor(_) => "x",
        ColumnType::Domain { base, .. } => pg_type_storage(base),
        _ => "p",
    }
}

pub(super) fn pg_type_array_oid(ty: &ColumnType) -> i64 {
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

pub(super) fn pg_type_element_oid(ty: &ColumnType) -> i64 {
    match ty {
        ColumnType::Name => 18,
        ColumnType::Int2Vector => 21,
        ColumnType::OidVector => 26,
        ColumnType::Array(element) => pg_type_oid(element),
        _ => 0,
    }
}

pub(super) fn pg_type_collation_oid(ty: &ColumnType) -> i64 {
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

pub(super) fn pg_type_subscript_handler(ty: &ColumnType) -> &'static str {
    match ty {
        ColumnType::Array(_) => "array_subscript_handler",
        ColumnType::Int2Vector | ColumnType::OidVector => "array_subscript_handler",
        ColumnType::Name => "raw_array_subscript_handler",
        ColumnType::JsonB => "jsonb_subscript_handler",
        _ => "-",
    }
}

pub(super) fn pg_type_modifier(ty: &ColumnType) -> i64 {
    match ty {
        // PostgreSQL stores varlena type modifiers with a four-byte header.
        ColumnType::Character(length) | ColumnType::Varchar(Some(length)) => i64::from(*length) + 4,
        _ => -1,
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct PgTypeRoutineOids {
    pub(super) input: i64,
    pub(super) output: i64,
    pub(super) receive: i64,
    pub(super) send: i64,
    pub(super) modifier_input: i64,
    pub(super) modifier_output: i64,
    pub(super) analyze: i64,
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

pub(super) fn pg_type_routine_oids(ty: &ColumnType) -> PgTypeRoutineOids {
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
        ColumnType::Regclass => PgTypeRoutineOids::new(2218, 2219, 2452, 2453),
        ColumnType::Regnamespace => PgTypeRoutineOids::new(4084, 4085, 4087, 4088),
        ColumnType::Text => PgTypeRoutineOids::new(46, 47, 2414, 2415),
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
        ColumnType::Vector(_) | ColumnType::Tensor(_) => PgTypeRoutineOids::new(0, 0, 0, 0),
        ColumnType::Array(_) | ColumnType::Domain { .. } => {
            unreachable!("array and domain type routines are handled before scalar dispatch")
        }
    }
}

pub(super) fn info_datetime_precision(ty: &ColumnType) -> Value {
    match ty {
        ColumnType::Time | ColumnType::TimeTz | ColumnType::Timestamp | ColumnType::TimestampTz => {
            Value::Int(6)
        }
        _ => Value::Null,
    }
}

pub(super) fn info_character_maximum_length(ty: &ColumnType) -> Value {
    match ty {
        ColumnType::Character(length) | ColumnType::Varchar(Some(length)) => {
            Value::Int(i64::from(*length))
        }
        _ => Value::Null,
    }
}

pub(super) fn info_character_octet_length(ty: &ColumnType) -> Value {
    match ty {
        // The engine catalog advertises UTF8, whose maximum encoded scalar
        // width is four bytes, matching PostgreSQL's information_schema.
        ColumnType::Character(length) | ColumnType::Varchar(Some(length)) => {
            Value::Int(i64::from(*length) * 4)
        }
        _ => Value::Null,
    }
}

pub(super) fn info_numeric_precision(ty: &ColumnType) -> Value {
    match ty {
        ColumnType::SmallInteger => Value::Int(16),
        ColumnType::Integer => Value::Int(32),
        ColumnType::BigInteger => Value::Int(64),
        ColumnType::Real => Value::Int(24),
        ColumnType::DoublePrecision => Value::Int(53),
        ColumnType::Numeric {
            precision: Some(precision),
            ..
        } => Value::Int(i64::from(*precision)),
        _ => Value::Null,
    }
}

pub(super) fn info_numeric_scale(ty: &ColumnType) -> Value {
    match ty {
        ColumnType::Numeric {
            scale: Some(scale), ..
        } => Value::Int(i64::from(*scale)),
        _ => Value::Null,
    }
}

pub(super) fn info_udt_name(ty: &ColumnType) -> String {
    match ty {
        ColumnType::SmallInteger => "int2".into(),
        ColumnType::Integer => "int4".into(),
        ColumnType::BigInteger => "int8".into(),
        ColumnType::Oid => "oid".into(),
        ColumnType::Xid => "xid".into(),
        ColumnType::Boolean => "bool".into(),
        ColumnType::Text => "text".into(),
        ColumnType::Name => "name".into(),
        ColumnType::Uuid => "uuid".into(),
        ColumnType::Varchar(_) => "varchar".into(),
        ColumnType::Bpchar | ColumnType::Character(_) => "bpchar".into(),
        ColumnType::Real => "float4".into(),
        ColumnType::DoublePrecision => "float8".into(),
        ColumnType::Numeric { .. } => "numeric".into(),
        ColumnType::Json => "json".into(),
        ColumnType::JsonB => "jsonb".into(),
        ColumnType::Bytea => "bytea".into(),
        ColumnType::InternalChar => "char".into(),
        ColumnType::Regproc => "regproc".into(),
        ColumnType::Regclass => "regclass".into(),
        ColumnType::Regnamespace => "regnamespace".into(),
        ColumnType::Regtype => "regtype".into(),
        ColumnType::PgNodeTree => "pg_node_tree".into(),
        ColumnType::AclItem => "aclitem".into(),
        ColumnType::Int2Vector => "int2vector".into(),
        ColumnType::OidVector => "oidvector".into(),
        ColumnType::AnyArray => "anyarray".into(),
        ColumnType::Record => "record".into(),
        ColumnType::Array(element) => match element.as_ref() {
            ColumnType::SmallInteger => "_int2".into(),
            ColumnType::Integer => "_int4".into(),
            ColumnType::BigInteger => "_int8".into(),
            ColumnType::Oid => "_oid".into(),
            ColumnType::Xid => "_xid".into(),
            ColumnType::Boolean => "_bool".into(),
            ColumnType::Text => "_text".into(),
            ColumnType::Name => "_name".into(),
            ColumnType::Uuid => "_uuid".into(),
            ColumnType::Varchar(_) => "_varchar".into(),
            ColumnType::Bpchar | ColumnType::Character(_) => "_bpchar".into(),
            ColumnType::Real => "_float4".into(),
            ColumnType::DoublePrecision => "_float8".into(),
            ColumnType::Numeric { .. } => "_numeric".into(),
            ColumnType::Json => "_json".into(),
            ColumnType::JsonB => "_jsonb".into(),
            ColumnType::Bytea => "_bytea".into(),
            ColumnType::InternalChar => "_char".into(),
            ColumnType::Regproc => "_regproc".into(),
            ColumnType::Regclass => "_regclass".into(),
            ColumnType::Regnamespace => "_regnamespace".into(),
            ColumnType::Regtype => "_regtype".into(),
            ColumnType::PgNodeTree => "_pg_node_tree".into(),
            ColumnType::AclItem => "_aclitem".into(),
            ColumnType::Int2Vector => "_int2vector".into(),
            ColumnType::OidVector => "_oidvector".into(),
            ColumnType::AnyArray => "_anyarray".into(),
            ColumnType::Record => "_record".into(),
            ColumnType::Date => "_date".into(),
            ColumnType::Time => "_time".into(),
            ColumnType::TimeTz => "_timetz".into(),
            ColumnType::Timestamp => "_timestamp".into(),
            ColumnType::TimestampTz => "_timestamptz".into(),
            ColumnType::Interval => "_interval".into(),
            ColumnType::Vector(_) => "_vector".into(),
            ColumnType::Tensor(_) => "_tensor".into(),
            ColumnType::Domain { name, .. } => format!("_{name}"),
            ColumnType::Array(_) => info_udt_name(element),
        },
        ColumnType::Date => "date".into(),
        ColumnType::Time => "time".into(),
        ColumnType::TimeTz => "timetz".into(),
        ColumnType::Timestamp => "timestamp".into(),
        ColumnType::TimestampTz => "timestamptz".into(),
        ColumnType::Interval => "interval".into(),
        ColumnType::Vector(_) => "vector".into(),
        ColumnType::Tensor(_) => "tensor".into(),
        ColumnType::Domain { name, .. } => name.clone(),
    }
}

pub(super) fn info_data_type(ty: &ColumnType) -> &str {
    if matches!(ty, ColumnType::Array(_)) {
        "ARRAY"
    } else {
        column_type_name(ty)
    }
}

pub(super) fn array_dimension_count(ty: &ColumnType) -> i64 {
    let mut dimensions = 0_i64;
    let mut current = ty;
    while let ColumnType::Array(element) = current {
        dimensions += 1;
        current = element;
    }
    dimensions
}

pub(super) fn index_columns(columns_json: &str) -> Result<Vec<String>, SQLError> {
    serde_json::from_str(columns_json)
        .map_err(|err| SQLError::Internal(format!("decode index column catalog: {err}")))
}

pub(super) fn indexdef(name: &str, index_type: &str, table: &str, columns: &[String]) -> String {
    let method = if index_type.is_empty() {
        "btree"
    } else {
        index_type
    };
    format!(
        "CREATE INDEX {name} ON {table} USING {method} ({})",
        columns.join(", ")
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ConstraintCatalogKind {
    PrimaryKey,
    Unique { nulls_not_distinct: bool },
    ForeignKey,
    Check,
    NotNull,
}

impl ConstraintCatalogKind {
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::PrimaryKey => "PRIMARY KEY",
            Self::Unique { .. } => "UNIQUE",
            Self::ForeignKey => "FOREIGN KEY",
            Self::Check => "CHECK",
            Self::NotNull => "NOT NULL",
        }
    }

    pub(super) const fn pg_type(self) -> &'static str {
        match self {
            Self::PrimaryKey => "p",
            Self::Unique { .. } => "u",
            Self::ForeignKey => "f",
            Self::Check => "c",
            Self::NotNull => "n",
        }
    }

    pub(super) const fn nulls_distinct(self) -> Option<bool> {
        match self {
            Self::Unique { nulls_not_distinct } => Some(!nulls_not_distinct),
            _ => None,
        }
    }

    pub(super) const fn no_inherit(self) -> bool {
        matches!(
            self,
            Self::PrimaryKey | Self::Unique { .. } | Self::ForeignKey
        )
    }
}

#[derive(Debug, Clone)]
pub(super) struct ConstraintCatalogColumn {
    pub(super) name: String,
    pub(super) table_ordinal: i64,
}

#[derive(Debug, Clone)]
pub(super) struct ForeignKeyCatalogData {
    pub(super) schema: String,
    pub(super) table: String,
    pub(super) column_ordinals: Vec<i64>,
    pub(super) positions_in_unique_constraint: Vec<Option<i64>>,
    pub(super) on_update: ForeignKeyAction,
    pub(super) on_delete: ForeignKeyAction,
    pub(super) match_type: ForeignKeyMatch,
}

#[derive(Debug, Clone)]
pub(super) struct ConstraintCatalogRow {
    pub(super) schema: String,
    pub(super) table: String,
    pub(super) name: String,
    pub(super) kind: ConstraintCatalogKind,
    pub(super) columns: Vec<ConstraintCatalogColumn>,
    pub(super) enforced: bool,
    pub(super) foreign_key: Option<ForeignKeyCatalogData>,
}

#[derive(Debug)]
struct PendingConstraintCatalogRow {
    schema: String,
    table: String,
    requested_name: Option<String>,
    kind: ConstraintCatalogKind,
    columns: Vec<ConstraintCatalogColumn>,
    enforced: bool,
    foreign_key: Option<ForeignKeyCatalogData>,
}

pub(super) fn constraint_catalog_rows(
    engine: &Engine,
) -> Result<Vec<ConstraintCatalogRow>, SQLError> {
    let mut out = Vec::new();
    for table_name in engine
        .table_names()
        .map_err(|err| SQLError::Internal(format!("read table catalog: {err}")))?
    {
        let (schema, table) = split_schema_name(&table_name)?;
        let columns = table_columns_for(engine, &table_name)?;
        let declared = engine
            .try_declared_table_constraints(&table_name)
            .map_err(|err| SQLError::Internal(format!("read table constraints: {err}")))?;
        let mut pending = Vec::new();

        for (idx, col) in columns.iter().enumerate() {
            let ordinal = catalog_ordinal(idx, "constraint column")?;
            if col.not_null {
                pending.push(PendingConstraintCatalogRow {
                    schema: schema.clone(),
                    table: table.clone(),
                    requested_name: col.not_null_name.clone(),
                    kind: ConstraintCatalogKind::NotNull,
                    columns: vec![ConstraintCatalogColumn {
                        name: col.name.clone(),
                        table_ordinal: ordinal,
                    }],
                    enforced: true,
                    foreign_key: None,
                });
            }
            if let Some(expr) = &col.check {
                pending.push(PendingConstraintCatalogRow {
                    schema: schema.clone(),
                    table: table.clone(),
                    requested_name: col.check_name.clone(),
                    kind: ConstraintCatalogKind::Check,
                    columns: check_constraint_columns(expr, &columns, &table_name)?,
                    enforced: col.check_enforced,
                    foreign_key: None,
                });
            }
            if let Some(reference) = &col.references {
                let foreign_key = ForeignKey {
                    name: reference.name.clone(),
                    local_columns: vec![col.name.clone()],
                    ref_table: reference.table.clone(),
                    ref_columns: vec![reference.column.clone()],
                    on_update: reference.on_update,
                    on_delete: reference.on_delete,
                    on_delete_set_columns: Vec::new(),
                    match_type: reference.match_type,
                    enforced: reference.enforced,
                };
                pending.push(foreign_key_catalog_row(
                    engine,
                    &schema,
                    &table,
                    &table_name,
                    &columns,
                    &foreign_key,
                )?);
            }
        }

        for constraint in engine
            .try_key_constraints(&table_name)
            .map_err(|err| SQLError::Internal(format!("read key constraints: {err}")))?
        {
            pending.push(PendingConstraintCatalogRow {
                schema: schema.clone(),
                table: table.clone(),
                requested_name: constraint.name,
                kind: match constraint.kind {
                    TableKeyConstraintKind::PrimaryKey => ConstraintCatalogKind::PrimaryKey,
                    TableKeyConstraintKind::Unique => ConstraintCatalogKind::Unique {
                        nulls_not_distinct: constraint.nulls_not_distinct,
                    },
                },
                columns: named_constraint_columns(&constraint.columns, &columns, &table_name)?,
                enforced: true,
                foreign_key: None,
            });
        }

        for constraint in declared.checks {
            pending.push(PendingConstraintCatalogRow {
                schema: schema.clone(),
                table: table.clone(),
                requested_name: constraint.name,
                kind: ConstraintCatalogKind::Check,
                columns: check_constraint_columns(&constraint.expr, &columns, &table_name)?,
                enforced: constraint.enforced,
                foreign_key: None,
            });
        }

        for foreign_key in declared.foreign_keys {
            pending.push(foreign_key_catalog_row(
                engine,
                &schema,
                &table,
                &table_name,
                &columns,
                &foreign_key,
            )?);
        }

        for constraint in pending {
            let name = constraint.requested_name.ok_or_else(|| {
                SQLError::Internal(format!(
                    "durable constraint on `{}.{}` has no name",
                    constraint.schema, constraint.table
                ))
            })?;
            out.push(ConstraintCatalogRow {
                schema: constraint.schema,
                table: constraint.table,
                name,
                kind: constraint.kind,
                columns: constraint.columns,
                enforced: constraint.enforced,
                foreign_key: constraint.foreign_key,
            });
        }
    }
    Ok(out)
}

fn named_constraint_columns(
    names: &[String],
    columns: &[SQLColumnDef],
    table_name: &str,
) -> Result<Vec<ConstraintCatalogColumn>, SQLError> {
    names
        .iter()
        .map(|name| {
            let index = columns
                .iter()
                .position(|column| column.name == *name)
                .ok_or_else(|| {
                    SQLError::Internal(format!(
                        "constraint on table `{table_name}` references missing column `{name}`"
                    ))
                })?;
            Ok(ConstraintCatalogColumn {
                name: name.clone(),
                table_ordinal: catalog_ordinal(index, "constraint column")?,
            })
        })
        .collect()
}

fn check_constraint_columns(
    expression: &Expr,
    columns: &[SQLColumnDef],
    table_name: &str,
) -> Result<Vec<ConstraintCatalogColumn>, SQLError> {
    let mut names = Vec::new();
    collect_expression_columns(expression, &mut names);
    named_constraint_columns(&names, columns, table_name)
}

fn collect_expression_columns(expression: &Expr, output: &mut Vec<String>) {
    match expression {
        Expr::Column(name) | Expr::QualifiedColumn { column: name, .. } => {
            if !output.contains(name) {
                output.push(name.clone());
            }
        }
        Expr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            for argument in args {
                collect_expression_columns(argument, output);
            }
            for order in order_by {
                collect_expression_columns(&order.expr, output);
            }
            if let Some(filter) = filter {
                collect_expression_columns(filter, output);
            }
        }
        Expr::Array(items) | Expr::Row(items) | Expr::And(items) | Expr::Or(items) => {
            for item in items {
                collect_expression_columns(item, output);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_expression_columns(lhs, output);
            collect_expression_columns(rhs, output);
        }
        Expr::Not(inner)
        | Expr::UnaryMinus(inner)
        | Expr::IsNull { expr: inner, .. }
        | Expr::Cast { expr: inner, .. } => {
            collect_expression_columns(inner, output);
        }
        Expr::Between { expr, low, high } => {
            collect_expression_columns(expr, output);
            collect_expression_columns(low, output);
            collect_expression_columns(high, output);
        }
        Expr::InList { expr, list, .. } => {
            collect_expression_columns(expr, output);
            for item in list {
                collect_expression_columns(item, output);
            }
        }
        Expr::WindowCall { args, spec, .. } => {
            for argument in args {
                collect_expression_columns(argument, output);
            }
            collect_window_columns(spec, output);
        }
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base {
                collect_expression_columns(base, output);
            }
            for (condition, result) in when {
                collect_expression_columns(condition, output);
                collect_expression_columns(result, output);
            }
            if let Some(else_branch) = else_branch {
                collect_expression_columns(else_branch, output);
            }
        }
        Expr::InSubquery { expr, .. } => collect_expression_columns(expr, output),
        Expr::Default
        | Expr::Star
        | Expr::QualifiedStar(_)
        | Expr::Literal(_)
        | Expr::Param(_)
        | Expr::ScalarSubquery(_)
        | Expr::Exists { .. } => {}
    }
}

fn collect_window_columns(spec: &WindowSpec, output: &mut Vec<String>) {
    for expression in &spec.partition_by {
        collect_expression_columns(expression, output);
    }
    for order in &spec.order_by {
        collect_expression_columns(&order.expr, output);
    }
    if let Some(frame) = &spec.frame {
        collect_window_frame_columns(frame, output);
    }
}

fn collect_window_frame_columns(frame: &WindowFrame, output: &mut Vec<String>) {
    use uqa_sql::ast::FrameBound;
    for bound in [&frame.start, &frame.end] {
        match bound {
            FrameBound::Preceding(expression) | FrameBound::Following(expression) => {
                collect_expression_columns(expression, output);
            }
            FrameBound::UnboundedPreceding
            | FrameBound::UnboundedFollowing
            | FrameBound::CurrentRow => {}
        }
    }
}

fn foreign_key_catalog_row(
    engine: &Engine,
    schema: &str,
    table: &str,
    table_name: &str,
    columns: &[SQLColumnDef],
    foreign_key: &ForeignKey,
) -> Result<PendingConstraintCatalogRow, SQLError> {
    let local_columns = named_constraint_columns(&foreign_key.local_columns, columns, table_name)?;
    let referenced_name = engine
        .try_resolve_table_name(&foreign_key.ref_table)
        .map_err(|err| SQLError::Internal(format!("resolve referenced table: {err}")))?
        .ok_or_else(|| {
            SQLError::Internal(format!(
                "constraint on table `{table_name}` references missing table `{}`",
                foreign_key.ref_table
            ))
        })?;
    let (referenced_schema, referenced_table) = split_schema_name(&referenced_name)?;
    let referenced_columns = table_columns_for(engine, &referenced_name)?;
    let referenced_column_rows = named_constraint_columns(
        &foreign_key.ref_columns,
        &referenced_columns,
        &referenced_name,
    )?;
    let referenced_keys = engine
        .try_key_constraints(&referenced_name)
        .map_err(|err| SQLError::Internal(format!("read referenced key constraints: {err}")))?;
    let referenced_key = referenced_keys.iter().find(|constraint| {
        constraint.columns.len() == foreign_key.ref_columns.len()
            && foreign_key
                .ref_columns
                .iter()
                .all(|column| constraint.columns.contains(column))
    });
    let positions_in_unique_constraint = foreign_key
        .ref_columns
        .iter()
        .map(|column| {
            referenced_key
                .and_then(|constraint| constraint.columns.iter().position(|item| item == column))
                .map(|index| catalog_ordinal(index, "referenced key column"))
                .transpose()
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    Ok(PendingConstraintCatalogRow {
        schema: schema.to_string(),
        table: table.to_string(),
        requested_name: foreign_key.name.clone(),
        kind: ConstraintCatalogKind::ForeignKey,
        columns: local_columns,
        enforced: foreign_key.enforced,
        foreign_key: Some(ForeignKeyCatalogData {
            schema: referenced_schema,
            table: referenced_table,
            column_ordinals: referenced_column_rows
                .iter()
                .map(|column| column.table_ordinal)
                .collect(),
            positions_in_unique_constraint,
            on_update: foreign_key.on_update,
            on_delete: foreign_key.on_delete,
            match_type: foreign_key.match_type,
        }),
    })
}
