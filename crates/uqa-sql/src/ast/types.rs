//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use serde::{Deserialize, Serialize};

use super::{IntervalFields, RangeSubtype};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ColumnType {
    /// A declaration awaiting catalog type resolution. This variant is never a stored column type.
    Named(String),
    SmallInteger,
    Integer,
    BigInteger,
    /// `PostgreSQL` object identifier (`pg_catalog.oid`).
    Oid,
    /// `PostgreSQL` transaction identifier (`pg_catalog.xid`).
    Xid,
    Boolean,
    /// `PostgreSQL`'s non-null zero-width `void` pseudo-type.
    Void,
    Text,
    /// `PostgreSQL` cursor portal name (`pg_catalog.refcursor`).
    RefCursor,
    Name,
    Uuid,
    Varchar(Option<u32>),
    /// Internal unconstrained `bpchar` type used after common-type selection.
    Bpchar,
    /// `PostgreSQL` blank-padded `CHARACTER(n)` / `CHAR(n)` (`bpchar`).
    /// The length counts Unicode scalar values and defaults to one when the
    /// declaration omits an explicit modifier.
    Character(u32),
    Real,
    DoublePrecision,
    /// `NUMERIC(precision, scale)` -- exact decimal storage. When
    /// `scale` is `Some(s)` the engine rounds `INSERT` values to `s`
    /// fractional digits. `precision` is captured for round-tripping
    /// the catalog text but is not currently enforced.
    Numeric {
        precision: Option<u32>,
        scale: Option<i32>,
    },
    /// `JSON` / `JSONB` columns store typed JSON values.
    Json,
    /// `JSONB` columns store typed JSON values with `PostgreSQL` JSONB operators.
    JsonB,
    /// `BYTEA` columns store opaque bytes.
    Bytea,
    /// `PostgreSQL`'s internal single-byte `"char"` catalog type.
    InternalChar,
    Regproc,
    /// `PostgreSQL` routine-signature object identifier (`pg_catalog.regprocedure`).
    Regprocedure,
    /// `PostgreSQL` relation object identifier (`pg_catalog.regclass`).
    Regclass,
    /// `PostgreSQL` namespace object identifier (`pg_catalog.regnamespace`).
    Regnamespace,
    /// `PostgreSQL` role object identifier (`pg_catalog.regrole`).
    Regrole,
    Regtype,
    PgNodeTree,
    AclItem,
    Int2Vector,
    OidVector,
    AnyArray,
    /// `PostgreSQL`'s anonymous composite pseudo-type (OID 2249).
    Record,
    /// A `PostgreSQL` array whose elements retain their declared SQL type.
    /// Nested array bounds are represented recursively.
    Array(Box<ColumnType>),
    /// `DATE` columns store days since 1970-01-01.
    Date,
    /// `TIME` columns store microseconds since midnight.
    Time,
    /// `TIME(p)` with an explicit fractional-second precision.
    TimePrecision(u32),
    /// `TIME WITH TIME ZONE` columns store local time plus offset.
    TimeTz,
    /// `TIME(p) WITH TIME ZONE` with an explicit fractional-second precision.
    TimeTzPrecision(u32),
    /// `TIMESTAMP WITHOUT TIME ZONE` columns store naive microseconds
    /// since 1970-01-01 00:00:00.
    Timestamp,
    /// `TIMESTAMP(p)` with an explicit fractional-second precision.
    TimestampPrecision(u32),
    /// `TIMESTAMP WITH TIME ZONE` columns store UTC microseconds since
    /// 1970-01-01 00:00:00Z.
    TimestampTz,
    /// `TIMESTAMP(p) WITH TIME ZONE` with an explicit fractional-second precision.
    TimestampTzPrecision(u32),
    Interval,
    /// An interval retaining its stored-field restriction and fractional-second precision.
    IntervalWithFields {
        fields: IntervalFields,
        precision: Option<u32>,
    },
    /// One of `PostgreSQL`'s six built-in range identities. Values use a
    /// canonical textual carrier so bounds remain durable across every
    /// storage backend while the declared subtype stays in row metadata.
    Range(RangeSubtype),
    /// The `PostgreSQL` multirange paired with one built-in range subtype.
    Multirange(RangeSubtype),
    /// `VECTOR(N)` columns store an `N`-dimensional `f32` embedding.
    Vector(u32),
    /// `TENSOR(N)` columns store an array of `N`-dimensional `f32`
    /// embeddings. The row remains the retrieval identity; vector
    /// indexes score against the best element in the tensor.
    Tensor(u32),
    /// A named `PostgreSQL` domain retaining both its own type identity and the
    /// base type used for value conversion and operator selection.
    Domain {
        schema: String,
        name: String,
        oid: u32,
        base: Box<ColumnType>,
    },
}

pub(crate) fn builtin_array_element_name(type_name: &str) -> Option<&'static str> {
    Some(match type_name {
        "_bool" => "bool",
        "_bytea" => "bytea",
        "_char" => "\"char\"",
        "_name" => "name",
        "_int8" => "int8",
        "_int2" => "int2",
        "_int2vector" => "int2vector",
        "_int4" => "int4",
        "_regproc" => "regproc",
        "_regprocedure" => "regprocedure",
        "_regclass" => "regclass",
        "_regrole" => "regrole",
        "_text" => "text",
        "_refcursor" => "refcursor",
        "_oid" => "oid",
        "_oidvector" => "oidvector",
        "_bpchar" => "bpchar",
        "_varchar" => "varchar",
        "_float4" => "float4",
        "_float8" => "float8",
        "_aclitem" => "aclitem",
        "_date" => "date",
        "_time" => "time",
        "_timestamp" => "timestamp",
        "_timestamptz" => "timestamptz",
        "_interval" => "interval",
        "_numeric" => "numeric",
        "_timetz" => "timetz",
        "_record" => "record",
        "_uuid" => "uuid",
        "_json" => "json",
        "_jsonb" => "jsonb",
        "_regtype" => "regtype",
        "_xid" => "xid",
        "_pg_node_tree" => "pg_node_tree",
        "_int4range" => "int4range",
        "_int8range" => "int8range",
        "_numrange" => "numrange",
        "_daterange" => "daterange",
        "_tsrange" => "tsrange",
        "_tstzrange" => "tstzrange",
        "_int4multirange" => "int4multirange",
        "_int8multirange" => "int8multirange",
        "_nummultirange" => "nummultirange",
        "_datemultirange" => "datemultirange",
        "_tsmultirange" => "tsmultirange",
        "_tstzmultirange" => "tstzmultirange",
        _ => return None,
    })
}

impl ColumnType {
    /// Retain the SQL type identity without a declaration's length, scale, or temporal precision.
    #[must_use]
    pub fn without_type_modifiers(&self) -> Self {
        match self {
            Self::Varchar(_) => Self::Varchar(None),
            Self::Character(_) => Self::Bpchar,
            Self::Numeric { .. } => Self::Numeric {
                precision: None,
                scale: None,
            },
            Self::Array(element) => Self::Array(Box::new(element.without_type_modifiers())),
            other => other.without_temporal_modifiers().clone(),
        }
    }

    #[must_use]
    pub const fn temporal_precision(&self) -> Option<u32> {
        match self {
            Self::IntervalWithFields { precision, .. } => *precision,
            Self::TimePrecision(p)
            | Self::TimeTzPrecision(p)
            | Self::TimestampPrecision(p)
            | Self::TimestampTzPrecision(p) => Some(*p),
            _ => None,
        }
    }

    #[must_use]
    pub const fn without_temporal_modifiers(&self) -> &Self {
        match self {
            Self::IntervalWithFields { .. } => &Self::Interval,
            Self::TimePrecision(_) => &Self::Time,
            Self::TimeTzPrecision(_) => &Self::TimeTz,
            Self::TimestampPrecision(_) => &Self::Timestamp,
            Self::TimestampTzPrecision(_) => &Self::TimestampTz,
            other => other,
        }
    }

    pub(crate) fn with_temporal_precision(
        self,
        precision: Option<i64>,
    ) -> Result<Self, crate::SQLError> {
        let Some(precision) = precision else {
            return Ok(self);
        };
        if precision < 0 {
            return Err(crate::SQLError::Routine {
                sqlstate: "22023".into(),
                message: format!(
                    "{} precision must not be negative",
                    self.regtype_name().to_uppercase()
                ),
            });
        }
        let precision = u32::try_from(precision.min(6)).expect("bounded temporal precision");
        Ok(match self {
            Self::Time => Self::TimePrecision(precision),
            Self::TimeTz => Self::TimeTzPrecision(precision),
            Self::Timestamp => Self::TimestampPrecision(precision),
            Self::TimestampTz => Self::TimestampTzPrecision(precision),
            other => other,
        })
    }

    pub(crate) fn with_interval_modifiers(
        fields: IntervalFields,
        precision: Option<i64>,
    ) -> Result<Self, crate::SQLError> {
        let precision = precision
            .map(|precision| {
                if precision < 0 {
                    return Err(crate::SQLError::Routine {
                        sqlstate: "22023".into(),
                        message: "INTERVAL precision must not be negative".into(),
                    });
                }
                Ok(u32::try_from(precision.min(6)).expect("bounded interval precision"))
            })
            .transpose()?;
        if fields == IntervalFields::All && precision.is_none() {
            return Ok(Self::Interval);
        }
        Ok(Self::IntervalWithFields { fields, precision })
    }

    #[must_use]
    pub fn is_integer(&self) -> bool {
        match self {
            Self::SmallInteger | Self::Integer | Self::BigInteger | Self::Oid | Self::Xid => true,
            Self::Domain { base, .. } => base.is_integer(),
            _ => false,
        }
    }

    #[must_use]
    pub fn is_character_string(&self) -> bool {
        match self {
            Self::Text
            | Self::Name
            | Self::Varchar(_)
            | Self::Bpchar
            | Self::Character(_)
            | Self::InternalChar
            | Self::PgNodeTree
            | Self::AclItem => true,
            Self::Domain { base, .. } => base.is_character_string(),
            _ => false,
        }
    }

    /// Parse the canonical or accepted spelling of one implemented SQL type.
    /// This is shared by expression binding and row-schema propagation so a
    /// cast's declared type is not reconstructed from its runtime value.
    #[expect(
        clippy::too_many_lines,
        reason = "exhaustive AST migration preserves every serialized variant"
    )]
    pub fn from_sql_name(name: &str) -> Result<Self, crate::SQLError> {
        let normalized = name.trim().to_ascii_lowercase();
        if let Some(element) = builtin_array_element_name(&normalized) {
            return Self::from_sql_name(element).map(|ty| Self::Array(Box::new(ty)));
        }
        if let Some(element) = normalized.strip_suffix("[]") {
            let element_type = Self::from_sql_name(element)?;
            if matches!(element_type, Self::Void) {
                return Err(crate::SQLError::Routine {
                    sqlstate: "42704".into(),
                    message: format!("type \"{normalized}\" does not exist"),
                });
            }
            return Ok(Self::Array(Box::new(element_type)));
        }
        let (base, modifier) = split_type_modifier(&normalized);
        let base = base.strip_prefix("pg_catalog.").unwrap_or(&base);
        let temporal_precision = || {
            modifier
                .map(|value| {
                    value.trim().parse::<i64>().map_err(|_| {
                        crate::SQLError::TypeMismatch(format!(
                            "invalid temporal precision: {value}"
                        ))
                    })
                })
                .transpose()
        };
        let character_length = || -> Result<Option<u32>, crate::SQLError> {
            modifier
                .map(|value| {
                    value
                        .parse::<u32>()
                        .ok()
                        .filter(|length| *length > 0)
                        .ok_or_else(|| {
                            crate::SQLError::TypeMismatch(format!(
                                "character length must be greater than zero, got {value}"
                            ))
                        })
                })
                .transpose()
        };
        match base {
            "smallint" | "int2" | "smallserial" | "serial2" => Ok(Self::SmallInteger),
            "integer" | "int" | "int4" | "serial" | "serial4" => Ok(Self::Integer),
            "bigint" | "int8" | "bigserial" | "serial8" => Ok(Self::BigInteger),
            "oid" => Ok(Self::Oid),
            "xid" => Ok(Self::Xid),
            "boolean" | "bool" => Ok(Self::Boolean),
            "void" => Ok(Self::Void),
            "text" => Ok(Self::Text),
            "refcursor" => Ok(Self::RefCursor),
            "name" => Ok(Self::Name),
            "uuid" => Ok(Self::Uuid),
            "varchar" | "character varying" => Ok(Self::Varchar(character_length()?)),
            "character" | "char" => Ok(Self::Character(character_length()?.unwrap_or(1))),
            "bpchar" => Ok(character_length()?.map_or(Self::Bpchar, Self::Character)),
            "real" | "float4" => Ok(Self::Real),
            "double" | "double precision" | "float8" => Ok(Self::DoublePrecision),
            "numeric" | "decimal" => {
                let (precision, scale) = match modifier {
                    None => (None, None),
                    Some(modifier) => {
                        let mut parts = modifier.split(',').map(str::trim);
                        let precision = parts
                            .next()
                            .and_then(|value| value.parse::<u32>().ok())
                            .ok_or_else(|| {
                                crate::SQLError::TypeMismatch(format!(
                                    "invalid numeric modifier `{modifier}`"
                                ))
                            })?;
                        let scale = parts
                            .next()
                            .map(|value| value.parse::<i32>())
                            .transpose()
                            .map_err(|_| {
                                crate::SQLError::TypeMismatch(format!(
                                    "invalid numeric modifier `{modifier}`"
                                ))
                            })?
                            .unwrap_or(0);
                        if parts.next().is_some() {
                            return Err(crate::SQLError::TypeMismatch(format!(
                                "invalid numeric modifier `{modifier}`"
                            )));
                        }
                        (Some(precision), Some(scale))
                    }
                };
                Ok(Self::Numeric { precision, scale })
            }
            "json" => Ok(Self::Json),
            "jsonb" => Ok(Self::JsonB),
            "bytea" => Ok(Self::Bytea),
            "\"char\"" => Ok(Self::InternalChar),
            "regproc" => Ok(Self::Regproc),
            "regprocedure" => Ok(Self::Regprocedure),
            "regclass" => Ok(Self::Regclass),
            "regnamespace" => Ok(Self::Regnamespace),
            "regrole" => Ok(Self::Regrole),
            "regtype" => Ok(Self::Regtype),
            "pg_node_tree" => Ok(Self::PgNodeTree),
            "aclitem" => Ok(Self::AclItem),
            "int2vector" => Ok(Self::Int2Vector),
            "oidvector" => Ok(Self::OidVector),
            "anyarray" => Ok(Self::AnyArray),
            "record" => Ok(Self::Record),
            "date" => Ok(Self::Date),
            "time" | "time without time zone" => {
                Self::Time.with_temporal_precision(temporal_precision()?)
            }
            "timetz" | "time with time zone" => {
                Self::TimeTz.with_temporal_precision(temporal_precision()?)
            }
            "timestamp" | "datetime" | "timestamp without time zone" => {
                Self::Timestamp.with_temporal_precision(temporal_precision()?)
            }
            "timestamptz" | "timestamp with time zone" => {
                Self::TimestampTz.with_temporal_precision(temporal_precision()?)
            }
            "interval" => Self::with_interval_modifiers(IntervalFields::All, temporal_precision()?),
            other if other.starts_with("interval ") => {
                let fields = IntervalFields::from_sql_suffix(&other[9..]).ok_or_else(|| {
                    crate::SQLError::TypeMismatch(format!("invalid interval fields: {other}"))
                })?;
                Self::with_interval_modifiers(fields, temporal_precision()?)
            }
            "int4range" => Ok(Self::Range(RangeSubtype::Integer)),
            "int8range" => Ok(Self::Range(RangeSubtype::BigInteger)),
            "numrange" => Ok(Self::Range(RangeSubtype::Numeric)),
            "daterange" => Ok(Self::Range(RangeSubtype::Date)),
            "tsrange" => Ok(Self::Range(RangeSubtype::Timestamp)),
            "tstzrange" => Ok(Self::Range(RangeSubtype::TimestampTz)),
            "int4multirange" => Ok(Self::Multirange(RangeSubtype::Integer)),
            "int8multirange" => Ok(Self::Multirange(RangeSubtype::BigInteger)),
            "nummultirange" => Ok(Self::Multirange(RangeSubtype::Numeric)),
            "datemultirange" => Ok(Self::Multirange(RangeSubtype::Date)),
            "tsmultirange" => Ok(Self::Multirange(RangeSubtype::Timestamp)),
            "tstzmultirange" => Ok(Self::Multirange(RangeSubtype::TimestampTz)),
            "vector" => modifier
                .and_then(|value| value.parse::<u32>().ok())
                .filter(|dimension| *dimension > 0)
                .map(Self::Vector)
                .ok_or_else(|| crate::SQLError::TypeMismatch("VECTOR requires a dimension".into())),
            "tensor" => modifier
                .and_then(|value| value.parse::<u32>().ok())
                .filter(|dimension| *dimension > 0)
                .map(Self::Tensor)
                .ok_or_else(|| crate::SQLError::TypeMismatch("TENSOR requires a dimension".into())),
            other => Err(crate::SQLError::Unsupported(format!(
                "SQL type `{other}` is not supported"
            ))),
        }
    }

    #[must_use]
    pub fn sql_name(&self) -> String {
        match self {
            Self::Named(name) => name.clone(),
            Self::SmallInteger => "smallint".into(),
            Self::Integer => "integer".into(),
            Self::BigInteger => "bigint".into(),
            Self::Oid => "oid".into(),
            Self::Xid => "xid".into(),
            Self::Boolean => "boolean".into(),
            Self::Void => "void".into(),
            Self::Text => "text".into(),
            Self::RefCursor => "refcursor".into(),
            Self::Name => "name".into(),
            Self::Uuid => "uuid".into(),
            Self::Varchar(Some(length)) => format!("character varying({length})"),
            Self::Varchar(None) => "character varying".into(),
            Self::Bpchar => "bpchar".into(),
            Self::Character(length) => format!("character({length})"),
            Self::Real => "real".into(),
            Self::DoublePrecision => "double precision".into(),
            Self::Numeric {
                precision: Some(precision),
                scale: Some(scale),
            } => format!("numeric({precision},{scale})"),
            Self::Numeric { .. } => "numeric".into(),
            Self::Json => "json".into(),
            Self::JsonB => "jsonb".into(),
            Self::Bytea => "bytea".into(),
            Self::InternalChar => "\"char\"".into(),
            Self::Regproc => "regproc".into(),
            Self::Regprocedure => "regprocedure".into(),
            Self::Regclass => "regclass".into(),
            Self::Regnamespace => "regnamespace".into(),
            Self::Regrole => "regrole".into(),
            Self::Regtype => "regtype".into(),
            Self::PgNodeTree => "pg_node_tree".into(),
            Self::AclItem => "aclitem".into(),
            Self::Int2Vector => "int2vector".into(),
            Self::OidVector => "oidvector".into(),
            Self::AnyArray => "anyarray".into(),
            Self::Record => "record".into(),
            Self::Array(element) => format!("{}[]", element.sql_name()),
            Self::Date => "date".into(),
            Self::Time => "time without time zone".into(),
            Self::TimePrecision(p) => format!("time({p}) without time zone"),
            Self::TimeTz => "time with time zone".into(),
            Self::TimeTzPrecision(p) => format!("time({p}) with time zone"),
            Self::Timestamp => "timestamp without time zone".into(),
            Self::TimestampPrecision(p) => format!("timestamp({p}) without time zone"),
            Self::TimestampTz => "timestamp with time zone".into(),
            Self::TimestampTzPrecision(p) => format!("timestamp({p}) with time zone"),
            Self::Interval => "interval".into(),
            Self::IntervalWithFields { fields, precision } => {
                let precision =
                    precision.map_or_else(String::new, |precision| format!("({precision})"));
                format!("interval{}{precision}", fields.sql_suffix())
            }
            Self::Range(subtype) => subtype.range_name().into(),
            Self::Multirange(subtype) => subtype.multirange_name().into(),
            Self::Vector(dimension) => format!("vector({dimension})"),
            Self::Tensor(dimension) => format!("tensor({dimension})"),
            Self::Domain { schema, name, .. } => format!(
                "{}.{}",
                crate::compiler::render_relation_component(schema),
                crate::compiler::render_relation_component(name)
            ),
        }
    }

    /// Name emitted by `PostgreSQL`'s `regtype` output, including
    /// `pg_typeof(...)`.
    #[must_use]
    pub fn regtype_name(&self) -> String {
        if matches!(self, Self::IntervalWithFields { .. }) {
            return "interval".into();
        }
        if self.temporal_precision().is_some() {
            return self.without_temporal_modifiers().regtype_name();
        }
        match self {
            Self::Varchar(_) => "character varying".into(),
            Self::Bpchar | Self::Character(_) => "character".into(),
            Self::Numeric { .. } => "numeric".into(),
            Self::Vector(_) => "vector".into(),
            Self::Tensor(_) => "tensor".into(),
            Self::Domain { .. } => self.sql_name(),
            Self::Array(element) => format!("{}[]", element.regtype_name()),
            other => other.sql_name(),
        }
    }
}

/// Split a SQL type modifier while retaining qualifiers after its parentheses.
pub(crate) fn split_type_modifier(ty: &str) -> (std::borrow::Cow<'_, str>, Option<&str>) {
    use std::borrow::Cow;
    match (ty.find('('), ty.rfind(')')) {
        (Some(open), Some(close)) if close > open => {
            let prefix = ty[..open].trim_end();
            let suffix = ty[close + 1..].trim();
            let base = if suffix.is_empty() {
                Cow::Borrowed(prefix)
            } else {
                Cow::Owned(format!("{prefix} {suffix}"))
            };
            (base, Some(ty[open + 1..close].trim()))
        }
        _ => (Cow::Borrowed(ty), None),
    }
}
