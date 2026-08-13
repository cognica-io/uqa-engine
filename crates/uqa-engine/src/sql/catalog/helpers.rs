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
use uqa_sql::ast::{
    ForeignKey, ForeignKeyAction, ForeignKeyMatch, TableKeyConstraintKind, WindowFrame, WindowSpec,
};

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

pub(super) fn row(entries: impl IntoIterator<Item = (&'static str, Value)>) -> ResultRow {
    let mut out = ResultRow::new();
    for (key, value) in entries {
        out.insert(key.to_string(), value);
    }
    out
}

#[derive(Debug, Clone, Copy)]
pub(super) struct BuiltinRoutineCatalogEntry {
    pub(super) oid: i64,
    pub(super) name: &'static str,
    pub(super) kind: &'static str,
    pub(super) strict: bool,
    pub(super) volatility: &'static str,
    pub(super) leakproof: bool,
    pub(super) return_type: i64,
    pub(super) argument_types: &'static [i64],
    pub(super) argument_names: &'static [&'static str],
    pub(super) default_arguments: usize,
    pub(super) argument_defaults: Option<&'static str>,
    pub(super) source: &'static str,
}

const FALSE_NODE: &str = "({CONST :consttype 16 :consttypmod -1 :constcollid 0 :constlen 1 :constbyval true :constisnull false :location -1 :constvalue 1 [ 0 0 0 0 0 0 0 0 ]})";

pub(super) const PG18_BUILTIN_ROUTINES: &[BuiltinRoutineCatalogEntry] = &[
    BuiltinRoutineCatalogEntry {
        oid: 6381,
        name: "array_reverse",
        kind: "f",
        strict: true,
        volatility: "i",
        leakproof: false,
        return_type: 2277,
        argument_types: &[2277],
        argument_names: &[],
        default_arguments: 0,
        argument_defaults: None,
        source: "array_reverse",
    },
    BuiltinRoutineCatalogEntry {
        oid: 6388,
        name: "array_sort",
        kind: "f",
        strict: true,
        volatility: "i",
        leakproof: false,
        return_type: 2277,
        argument_types: &[2277],
        argument_names: &[],
        default_arguments: 0,
        argument_defaults: None,
        source: "array_sort",
    },
    BuiltinRoutineCatalogEntry {
        oid: 6389,
        name: "array_sort",
        kind: "f",
        strict: true,
        volatility: "i",
        leakproof: false,
        return_type: 2277,
        argument_types: &[2277, 16],
        argument_names: &["array", "descending"],
        default_arguments: 0,
        argument_defaults: None,
        source: "array_sort_order",
    },
    BuiltinRoutineCatalogEntry {
        oid: 6390,
        name: "array_sort",
        kind: "f",
        strict: true,
        volatility: "i",
        leakproof: false,
        return_type: 2277,
        argument_types: &[2277, 16, 16],
        argument_names: &["array", "descending", "nulls_first"],
        default_arguments: 0,
        argument_defaults: None,
        source: "array_sort_order_nulls_first",
    },
    BuiltinRoutineCatalogEntry {
        oid: 6412,
        name: "casefold",
        kind: "f",
        strict: true,
        volatility: "i",
        leakproof: false,
        return_type: 25,
        argument_types: &[25],
        argument_names: &[],
        default_arguments: 0,
        argument_defaults: None,
        source: "casefold",
    },
    BuiltinRoutineCatalogEntry {
        oid: 6364,
        name: "crc32",
        kind: "f",
        strict: true,
        volatility: "i",
        leakproof: true,
        return_type: 20,
        argument_types: &[17],
        argument_names: &[],
        default_arguments: 0,
        argument_defaults: None,
        source: "crc32_bytea",
    },
    BuiltinRoutineCatalogEntry {
        oid: 6365,
        name: "crc32c",
        kind: "f",
        strict: true,
        volatility: "i",
        leakproof: true,
        return_type: 20,
        argument_types: &[17],
        argument_names: &[],
        default_arguments: 0,
        argument_defaults: None,
        source: "crc32c_bytea",
    },
    BuiltinRoutineCatalogEntry {
        oid: 6383,
        name: "gamma",
        kind: "f",
        strict: true,
        volatility: "i",
        leakproof: false,
        return_type: 701,
        argument_types: &[701],
        argument_names: &[],
        default_arguments: 0,
        argument_defaults: None,
        source: "dgamma",
    },
    BuiltinRoutineCatalogEntry {
        oid: 6384,
        name: "lgamma",
        kind: "f",
        strict: true,
        volatility: "i",
        leakproof: false,
        return_type: 701,
        argument_types: &[701],
        argument_names: &[],
        default_arguments: 0,
        argument_defaults: None,
        source: "dlgamma",
    },
    BuiltinRoutineCatalogEntry {
        oid: 3261,
        name: "json_strip_nulls",
        kind: "f",
        strict: true,
        volatility: "i",
        leakproof: false,
        return_type: 114,
        argument_types: &[114, 16],
        argument_names: &["target", "strip_in_arrays"],
        default_arguments: 1,
        argument_defaults: Some(FALSE_NODE),
        source: "json_strip_nulls",
    },
    BuiltinRoutineCatalogEntry {
        oid: 3262,
        name: "jsonb_strip_nulls",
        kind: "f",
        strict: true,
        volatility: "i",
        leakproof: false,
        return_type: 3802,
        argument_types: &[3802, 16],
        argument_names: &["target", "strip_in_arrays"],
        default_arguments: 1,
        argument_defaults: Some(FALSE_NODE),
        source: "jsonb_strip_nulls",
    },
    BuiltinRoutineCatalogEntry {
        oid: 2050,
        name: "max",
        kind: "a",
        strict: false,
        volatility: "i",
        leakproof: false,
        return_type: 2277,
        argument_types: &[2277],
        argument_names: &[],
        default_arguments: 0,
        argument_defaults: None,
        source: "aggregate_dummy",
    },
    BuiltinRoutineCatalogEntry {
        oid: 6373,
        name: "max",
        kind: "a",
        strict: false,
        volatility: "i",
        leakproof: false,
        return_type: 2249,
        argument_types: &[2249],
        argument_names: &[],
        default_arguments: 0,
        argument_defaults: None,
        source: "aggregate_dummy",
    },
    BuiltinRoutineCatalogEntry {
        oid: 6395,
        name: "max",
        kind: "a",
        strict: false,
        volatility: "i",
        leakproof: false,
        return_type: 17,
        argument_types: &[17],
        argument_names: &[],
        default_arguments: 0,
        argument_defaults: None,
        source: "aggregate_dummy",
    },
    BuiltinRoutineCatalogEntry {
        oid: 2051,
        name: "min",
        kind: "a",
        strict: false,
        volatility: "i",
        leakproof: false,
        return_type: 2277,
        argument_types: &[2277],
        argument_names: &[],
        default_arguments: 0,
        argument_defaults: None,
        source: "aggregate_dummy",
    },
    BuiltinRoutineCatalogEntry {
        oid: 6374,
        name: "min",
        kind: "a",
        strict: false,
        volatility: "i",
        leakproof: false,
        return_type: 2249,
        argument_types: &[2249],
        argument_names: &[],
        default_arguments: 0,
        argument_defaults: None,
        source: "aggregate_dummy",
    },
    BuiltinRoutineCatalogEntry {
        oid: 6396,
        name: "min",
        kind: "a",
        strict: false,
        volatility: "i",
        leakproof: false,
        return_type: 17,
        argument_types: &[17],
        argument_names: &[],
        default_arguments: 0,
        argument_defaults: None,
        source: "aggregate_dummy",
    },
    BuiltinRoutineCatalogEntry {
        oid: 6382,
        name: "reverse",
        kind: "f",
        strict: true,
        volatility: "i",
        leakproof: false,
        return_type: 17,
        argument_types: &[17],
        argument_names: &[],
        default_arguments: 0,
        argument_defaults: None,
        source: "bytea_reverse",
    },
    BuiltinRoutineCatalogEntry {
        oid: 3062,
        name: "reverse",
        kind: "f",
        strict: true,
        volatility: "i",
        leakproof: false,
        return_type: 25,
        argument_types: &[25],
        argument_names: &[],
        default_arguments: 0,
        argument_defaults: None,
        source: "text_reverse",
    },
    BuiltinRoutineCatalogEntry {
        oid: 6428,
        name: "uuidv4",
        kind: "f",
        strict: true,
        volatility: "v",
        leakproof: false,
        return_type: 2950,
        argument_types: &[],
        argument_names: &[],
        default_arguments: 0,
        argument_defaults: None,
        source: "gen_random_uuid",
    },
    BuiltinRoutineCatalogEntry {
        oid: 6429,
        name: "uuidv7",
        kind: "f",
        strict: true,
        volatility: "v",
        leakproof: false,
        return_type: 2950,
        argument_types: &[],
        argument_names: &[],
        default_arguments: 0,
        argument_defaults: None,
        source: "uuidv7",
    },
    BuiltinRoutineCatalogEntry {
        oid: 6430,
        name: "uuidv7",
        kind: "f",
        strict: true,
        volatility: "v",
        leakproof: false,
        return_type: 2950,
        argument_types: &[1186],
        argument_names: &["shift"],
        default_arguments: 0,
        argument_defaults: None,
        source: "uuidv7_interval",
    },
];

pub(super) fn catalog_type_name(oid: i64) -> &'static str {
    match oid {
        16 => "boolean",
        17 => "bytea",
        20 => "bigint",
        25 => "text",
        114 => "json",
        701 => "double precision",
        1186 => "interval",
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
        "information_schema" => 13_377,
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

pub(super) fn pg_type_oid(ty: &ColumnType) -> i64 {
    match ty {
        ColumnType::Integer => 23,
        ColumnType::Boolean => 16,
        ColumnType::Text => 25,
        ColumnType::Character(_) => 1042,
        ColumnType::Real => 701,
        ColumnType::Numeric { .. } => 1700,
        ColumnType::Json => 114,
        ColumnType::JsonB => 3802,
        ColumnType::Bytea => 17,
        ColumnType::Array(element) => match element.as_ref() {
            ColumnType::Integer => 1007,
            ColumnType::Boolean => 1000,
            ColumnType::Text => 1009,
            ColumnType::Character(_) => 1014,
            ColumnType::Real => 1022,
            ColumnType::Numeric { .. } => 1231,
            ColumnType::Json => 199,
            ColumnType::JsonB => 3807,
            ColumnType::Bytea => 1001,
            ColumnType::Date => 1182,
            ColumnType::Time => 1183,
            ColumnType::TimeTz => 1270,
            ColumnType::Timestamp => 1115,
            ColumnType::TimestampTz => 1185,
            ColumnType::Vector(_) => 380_002,
            ColumnType::Tensor(_) => 380_003,
            ColumnType::Array(_) => pg_type_oid(element),
        },
        ColumnType::Date => 1082,
        ColumnType::Time => 1083,
        ColumnType::TimeTz => 1266,
        ColumnType::Timestamp => 1114,
        ColumnType::TimestampTz => 1184,
        ColumnType::Vector(_) => 380_000,
        ColumnType::Tensor(_) => 380_001,
    }
}

pub(super) fn routine_type_oid(type_name: &str) -> i64 {
    let canonical = canonical_routine_type_name(type_name);
    match canonical.as_str() {
        "bool" => 16,
        "bytea" => 17,
        "int8" => 20,
        "int2" => 21,
        "int4" => 23,
        "text" => 25,
        "json" => 114,
        "float4" => 700,
        "float8" => 701,
        "varchar" => 1043,
        "date" => 1082,
        "time" => 1083,
        "timestamp" => 1114,
        "timestamptz" => 1184,
        "timetz" => 1266,
        "numeric" => 1700,
        "record" => 2249,
        "void" => 2278,
        "jsonb" => 3802,
        "vector" => 380_000,
        other => stable_oid("type", other),
    }
}

pub(super) fn pg_type_len(ty: &ColumnType) -> i64 {
    match ty {
        ColumnType::Integer => 4,
        ColumnType::Boolean => 1,
        ColumnType::Real | ColumnType::Timestamp | ColumnType::TimestampTz => 8,
        ColumnType::Date => 4,
        ColumnType::Time | ColumnType::TimeTz => 8,
        _ => -1,
    }
}

pub(super) fn pg_type_modifier(ty: &ColumnType) -> i64 {
    match ty {
        // PostgreSQL stores varlena type modifiers with a four-byte header.
        ColumnType::Character(length) => i64::from(*length) + 4,
        _ => -1,
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
        ColumnType::Character(length) => Value::Int(i64::from(*length)),
        _ => Value::Null,
    }
}

pub(super) fn info_character_octet_length(ty: &ColumnType) -> Value {
    match ty {
        // The engine catalog advertises UTF8, whose maximum encoded scalar
        // width is four bytes, matching PostgreSQL's information_schema.
        ColumnType::Character(length) => Value::Int(i64::from(*length) * 4),
        _ => Value::Null,
    }
}

pub(super) fn info_numeric_precision(ty: &ColumnType) -> Value {
    match ty {
        ColumnType::Integer => Value::Int(32),
        ColumnType::Real => Value::Int(53),
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

pub(super) fn info_udt_name(ty: &ColumnType) -> &'static str {
    match ty {
        ColumnType::Integer => "int4",
        ColumnType::Boolean => "bool",
        ColumnType::Text => "text",
        ColumnType::Character(_) => "bpchar",
        ColumnType::Real => "float8",
        ColumnType::Numeric { .. } => "numeric",
        ColumnType::Json => "json",
        ColumnType::JsonB => "jsonb",
        ColumnType::Bytea => "bytea",
        ColumnType::Array(element) => match element.as_ref() {
            ColumnType::Integer => "_int4",
            ColumnType::Boolean => "_bool",
            ColumnType::Text => "_text",
            ColumnType::Character(_) => "_bpchar",
            ColumnType::Real => "_float8",
            ColumnType::Numeric { .. } => "_numeric",
            ColumnType::Json => "_json",
            ColumnType::JsonB => "_jsonb",
            ColumnType::Bytea => "_bytea",
            ColumnType::Date => "_date",
            ColumnType::Time => "_time",
            ColumnType::TimeTz => "_timetz",
            ColumnType::Timestamp => "_timestamp",
            ColumnType::TimestampTz => "_timestamptz",
            ColumnType::Vector(_) => "_vector",
            ColumnType::Tensor(_) => "_tensor",
            ColumnType::Array(_) => info_udt_name(element),
        },
        ColumnType::Date => "date",
        ColumnType::Time => "time",
        ColumnType::TimeTz => "timetz",
        ColumnType::Timestamp => "timestamp",
        ColumnType::TimestampTz => "timestamptz",
        ColumnType::Vector(_) => "vector",
        ColumnType::Tensor(_) => "tensor",
    }
}

pub(super) fn info_data_type(ty: &ColumnType) -> &'static str {
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

pub(super) fn default_expr_text(expr: Option<&Expr>) -> Value {
    expr.map_or(Value::Null, |expr| Value::Str(format!("{expr:?}")))
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
        Expr::Array(items) | Expr::And(items) | Expr::Or(items) => {
            for item in items {
                collect_expression_columns(item, output);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_expression_columns(lhs, output);
            collect_expression_columns(rhs, output);
        }
        Expr::Not(inner) | Expr::IsNull { expr: inner, .. } | Expr::Cast { expr: inner, .. } => {
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
        Expr::Star
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
