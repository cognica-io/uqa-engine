//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical type parsing keeps normalized spelling and nested result constructors in distinct resource owners.

use super::{builtin_array_element_name, split_type_modifier_with_control, ColumnType};
use crate::ast::{IntervalFields, RangeSubtype};
use std::borrow::Cow;
use uqa_core::memory::{Produced, ProductionControl, ProductionString};
use uqa_core::ValueRetentionError;

impl ColumnType {
    /// Parse the canonical or accepted spelling of one implemented SQL type.
    pub fn from_sql_name(name: &str) -> Result<Self, crate::SQLError> {
        Self::from_sql_name_with_control(name, &ProductionControl::uncontrolled()).map(|value| {
            value
                .into_uncontrolled()
                .expect("ordinary type parsing has no reservation")
        })
    }

    /// Parse through the same grammar while each temporary name and nested result allocation uses the caller's allowance.
    #[expect(
        clippy::too_many_lines,
        reason = "one type grammar preserves every accepted spelling and diagnostic"
    )]
    pub fn from_sql_name_with_control(
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, crate::SQLError> {
        let normalized = normalized_type_name(name, control)?;
        if let Some(element) = builtin_array_element_name(&normalized) {
            return Self::array_with_control(
                Self::from_sql_name_with_control(element, control)?,
                control,
            )
            .map_err(Into::into);
        }
        if let Some(element) = normalized.strip_suffix("[]") {
            let element_type = Self::from_sql_name_with_control(element, control)?;
            if matches!(*element_type, Self::Void) {
                return Err(crate::SQLError::Routine {
                    sqlstate: "42704".into(),
                    message: format!("type \"{}\" does not exist", normalized.as_ref()),
                });
            }
            return Self::array_with_control(element_type, control).map_err(Into::into);
        }
        let (base, modifier) = split_type_modifier_with_control(&normalized, control)?;
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
        let parsed = match base {
            "smallint" | "int2" => Ok(Self::SmallInteger),
            "integer" | "int" | "int4" => Ok(Self::Integer),
            "bigint" | "int8" => Ok(Self::BigInteger),
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
        }?;
        control
            .finish(parsed, control.empty_reservation())
            .map_err(Into::into)
    }
}

fn normalized_type_name<'a>(
    name: &'a str,
    control: &ProductionControl<'_>,
) -> Result<Produced<Cow<'a, str>>, ValueRetentionError> {
    control.check()?;
    let name = name.trim();
    let mut uppercase = false;
    for chunk in name.as_bytes().chunks(4096) {
        control.check()?;
        if chunk.iter().any(u8::is_ascii_uppercase) {
            uppercase = true;
            break;
        }
    }
    if !uppercase {
        return control.finish(Cow::Borrowed(name), control.empty_reservation());
    }
    let mut normalized = ProductionString::new(*control);
    normalized.reserve(name.len())?;
    for character in name.chars() {
        normalized.push(character.to_ascii_lowercase())?;
    }
    let (normalized, memory) = normalized.finish()?.into_parts();
    control.finish(Cow::Owned(normalized), memory)
}
