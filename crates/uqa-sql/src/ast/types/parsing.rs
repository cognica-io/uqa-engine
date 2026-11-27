//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical type parsing keeps normalized spelling and nested result constructors in distinct resource owners.

use super::{
    builtin_array_element_name,
    lexical::{self, Work},
    modifiers::split_type_modifier_from,
    ColumnType, ColumnTypeInputWork,
};
use crate::ast::{IntervalFields, RangeSubtype};
use crate::SQLError;
use std::borrow::Cow;
use uqa_core::memory::{Produced, ProductionControl, ProductionString};

mod numbers;
#[cfg(test)]
mod tests;

#[derive(Clone, Copy, PartialEq, Eq)]
enum TypeUse {
    Declaration,
    Expression,
}

enum Parsed {
    Scalar(Produced<ColumnType>),
    Array(&'static str),
}

struct TypeWork<'a, W>(&'a mut W);
impl<W: ColumnTypeInputWork> Work for TypeWork<'_, W>
where
    W::Error: Into<SQLError>,
{
    type Error = SQLError;
    async fn checkpoint(&mut self) -> Result<(), SQLError> {
        self.0.checkpoint().await.map_err(Into::into)
    }
}

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
    pub fn from_sql_name_with_control(
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, SQLError> {
        crate::catalog::work::inline(Self::from_sql_name_with_input_work(name, control, &mut ()))
    }

    /// Parse a cast target, including built-in expression types without a physical scalar representation.
    pub fn from_expression_type_name_with_control(
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, SQLError> {
        crate::catalog::work::inline(Self::parse_type_name(
            name,
            control,
            &mut (),
            TypeUse::Expression,
        ))
    }

    /// Parse a native catalog type spelling with the invoking continuation.
    pub async fn from_sql_name_from_input(
        name: &str,
        control: &ProductionControl<'_>,
        input: &mut uqa_core::image_io::AsyncImageInput<'_>,
    ) -> Result<Produced<Self>, SQLError> {
        Self::from_sql_name_with_input_work(name, control, input).await
    }

    pub(crate) async fn from_sql_name_with_input_work<W: ColumnTypeInputWork>(
        name: &str,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<Produced<Self>, SQLError>
    where
        W::Error: Into<SQLError>,
    {
        Self::parse_type_name(name, control, work, TypeUse::Declaration).await
    }

    async fn parse_type_name<W: ColumnTypeInputWork>(
        name: &str,
        control: &ProductionControl<'_>,
        work: &mut W,
        usage: TypeUse,
    ) -> Result<Produced<Self>, SQLError>
    where
        W::Error: Into<SQLError>,
    {
        let mut work = TypeWork(work);
        let normalized = normalized_type_name(name, control, &mut work).await?;
        let mut selected: &str = &normalized;
        let mut dimensions = 0;
        let mut invalid_array = None;
        let mut parsed = loop {
            lexical::checkpoint(control, &mut work).await?;
            if let Some(element) = builtin_array_element_name(selected) {
                dimensions += 1;
                selected = element;
            } else if let Some(element) = selected.strip_suffix("[]") {
                dimensions += 1;
                invalid_array = Some(selected);
                selected = lexical::trim_end(element, control, &mut work).await?;
            } else {
                match Self::parse_scalar(selected, control, &mut work, usage).await? {
                    Parsed::Scalar(value) => break value,
                    Parsed::Array(element) => {
                        dimensions += 1;
                        selected = element;
                    }
                }
            }
        };
        if let Some(name) = invalid_array.filter(|_| {
            matches!(
                *parsed,
                Self::Void | Self::PgNodeTree | Self::AnyArray | Self::Pseudo(_)
            )
        }) {
            return Err(SQLError::Routine {
                sqlstate: "42704".into(),
                message: message("type \"", name, "\" does not exist", control, &mut work).await?,
            });
        }
        for _ in 0..dimensions {
            lexical::checkpoint(control, &mut work).await?;
            parsed = Self::array_with_control(parsed, control)?;
        }
        Ok(parsed)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one scalar type grammar preserves every accepted spelling and diagnostic"
    )]
    async fn parse_scalar<W: Work<Error = SQLError>>(
        normalized: &str,
        control: &ProductionControl<'_>,
        work: &mut W,
        usage: TypeUse,
    ) -> Result<Parsed, SQLError> {
        let (base, modifier) = split_type_modifier_from(normalized, control, work).await?;
        let catalog_name = base
            .strip_prefix("pg_catalog.")
            .or_else(|| base.strip_prefix("\"pg_catalog\"."));
        let catalog_modifiers = catalog_name.is_some() || base.starts_with('"');
        let local = catalog_name.unwrap_or(&base);
        let local = local
            .strip_prefix('"')
            .and_then(|name| name.strip_suffix('"'))
            .unwrap_or(local);
        let pseudo = if local == "anyarray" {
            Some(Self::AnyArray)
        } else if usage == TypeUse::Expression {
            super::ExpressionPseudoType::from_name(local).map(Self::Pseudo)
        } else {
            None
        };
        if let Some(ty) = pseudo {
            if modifier.is_some() {
                return Err(SQLError::Routine {
                    sqlstate: "42601".into(),
                    message: message(
                        "type modifier is not allowed for type \"",
                        local,
                        "\"",
                        control,
                        work,
                    )
                    .await?,
                });
            }
            return Ok(Parsed::Scalar(ty.clone_with_control(control)?));
        }
        let catalog_type;
        let base = if catalog_modifiers {
            let name = local;
            if let Some(element) = builtin_array_element_name(name) {
                return Ok(Parsed::Array(element));
            }
            let Some(ty) = crate::catalog::type_metadata::builtin_scalar_type_by_name(name) else {
                return Err(SQLError::Unsupported(
                    message("SQL type `", &base, "` is not supported", control, work).await?,
                ));
            };
            // Catalog identity without a modifier already determines the type. Regtype output is display syntax: bpchar prints as character, whose input spelling implies a length of one.
            if modifier.is_none() && !matches!(ty, Self::Vector(_) | Self::Tensor(_)) {
                return Ok(Parsed::Scalar(ty.clone_with_control(control)?));
            }
            catalog_type = ty.regtype_name_with_control(control)?;
            catalog_type.as_str()
        } else {
            &base
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
            "varchar" | "character varying" => Ok(Self::Varchar(
                numbers::character_length(modifier, control, work).await?,
            )),
            "character" | "char" => Ok(Self::Character(
                numbers::character_length(modifier, control, work)
                    .await?
                    .unwrap_or(1),
            )),
            "bpchar" => Ok(numbers::character_length(modifier, control, work)
                .await?
                .map_or(Self::Bpchar, Self::Character)),
            "real" | "float4" => Ok(Self::Real),
            "double" | "double precision" | "float8" => Ok(Self::DoublePrecision),
            "numeric" | "decimal" => {
                let (precision, scale) = numbers::numeric(modifier, control, work).await?;
                Ok(Self::Numeric { precision, scale })
            }
            "json" => Ok(Self::Json),
            "xml" => Ok(Self::Xml),
            "tsvector" => Ok(Self::TsVector),
            "jsonb" => Ok(Self::JsonB),
            "bytea" => Ok(Self::Bytea),
            "bit" | "bit varying" | "varbit" => {
                use crate::expr::bit_string::BitStringKind;
                let kind = if base == "bit" {
                    BitStringKind::Fixed
                } else {
                    BitStringKind::Varying
                };
                let length = match modifier {
                    None => None,
                    Some(text) => {
                        let value = numbers::integer(text, i64::MIN..=i64::MAX, control, work)
                            .await?
                            .ok_or_else(|| SQLError::Routine {
                                sqlstate: "22023".into(),
                                message: "invalid type modifier".into(),
                            })?;
                        Some(kind.validate_length(value)?)
                    }
                };
                Ok(if kind == BitStringKind::Fixed {
                    Self::Bit(length.or(Some(1)))
                } else {
                    Self::VarBit(length)
                })
            }
            "\"char\"" => Ok(Self::InternalChar),
            "regproc" => Ok(Self::Regproc),
            "regprocedure" => Ok(Self::Regprocedure),
            "regclass" => Ok(Self::Regclass),
            "regcollation" => Ok(Self::Regcollation),
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
            "time" | "time without time zone" => Self::Time
                .with_temporal_precision(numbers::temporal(modifier, control, work).await?),
            "timetz" | "time with time zone" => Self::TimeTz
                .with_temporal_precision(numbers::temporal(modifier, control, work).await?),
            "timestamp" | "datetime" | "timestamp without time zone" => Self::Timestamp
                .with_temporal_precision(numbers::temporal(modifier, control, work).await?),
            "timestamptz" | "timestamp with time zone" => Self::TimestampTz
                .with_temporal_precision(numbers::temporal(modifier, control, work).await?),
            "interval" if catalog_modifiers => {
                numbers::catalog_interval_type(modifier, control, work).await
            }
            "interval" => Self::with_interval_modifiers(
                IntervalFields::All,
                numbers::temporal(modifier, control, work).await?,
            ),
            other if other.starts_with("interval ") => {
                let Some(fields) = IntervalFields::from_sql_suffix(&other[9..]) else {
                    return Err(SQLError::TypeMismatch(
                        message("invalid interval fields: ", other, "", control, work).await?,
                    ));
                };
                Self::with_interval_modifiers(
                    fields,
                    numbers::temporal(modifier, control, work).await?,
                )
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
            "vector" | "tensor" => {
                let dimension = numbers::optional_unsigned(modifier, control, work)
                    .await?
                    .filter(|value| *value > 0);
                match (base, dimension) {
                    ("vector", Some(value)) => Ok(Self::Vector(value)),
                    ("tensor", Some(value)) => Ok(Self::Tensor(value)),
                    ("vector", None) => {
                        Err(SQLError::TypeMismatch("VECTOR requires a dimension".into()))
                    }
                    _ => Err(SQLError::TypeMismatch("TENSOR requires a dimension".into())),
                }
            }
            other => Err(SQLError::Unsupported(
                message("SQL type `", other, "` is not supported", control, work).await?,
            )),
        }?;
        Ok(Parsed::Scalar(
            control.finish(parsed, control.empty_reservation())?,
        ))
    }
}

async fn normalized_type_name<'a, W: Work<Error = SQLError>>(
    name: &'a str,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<Produced<Cow<'a, str>>, SQLError> {
    control.check()?;
    let name = lexical::trim(name, control, work).await?;
    let mut uppercase = false;
    for chunk in name.as_bytes().chunks(4096) {
        lexical::checkpoint(control, work).await?;
        if chunk.iter().any(u8::is_ascii_uppercase) {
            uppercase = true;
            break;
        }
    }
    if !uppercase {
        return Ok(control.finish(Cow::Borrowed(name), control.empty_reservation())?);
    }
    let mut normalized = ProductionString::new(*control);
    normalized.reserve(name.len())?;
    let mut quoted = false;
    for (index, character) in name.chars().enumerate() {
        if index % 1024 == 0 {
            lexical::checkpoint(control, work).await?;
        }
        if character == '"' {
            quoted = !quoted;
        }
        normalized.push(if quoted {
            character
        } else {
            character.to_ascii_lowercase()
        })?;
    }
    let (normalized, memory) = normalized.finish()?.into_parts();
    Ok(control.finish(Cow::Owned(normalized), memory)?)
}

async fn message<W: Work<Error = SQLError>>(
    prefix: &str,
    value: &str,
    suffix: &str,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<String, SQLError> {
    Ok(lexical::text(&[prefix, value, suffix], control, work)
        .await?
        .into_parts()
        .0)
}
