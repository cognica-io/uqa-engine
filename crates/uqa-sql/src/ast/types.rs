//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use serde::{Deserialize, Serialize};

use super::{IntervalFields, RangeSubtype};

mod modifiers;
mod names;
mod parsing;
mod production;

pub(crate) use modifiers::split_type_modifier_with_control;

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
        self.without_type_modifiers_with_control(
            &uqa_core::memory::ProductionControl::uncontrolled(),
        )
        .expect("ordinary type modifier removal cannot be limited or cancelled")
        .into_uncontrolled()
        .expect("ordinary type modifier removal has no reservation")
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
}
