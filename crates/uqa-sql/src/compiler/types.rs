//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Column type and foreign-key constraint lowering helpers.

use pg_query::protobuf::Node;
use pg_query::NodeEnum;

use crate::ast::ColumnType;
use crate::error::{Result, SQLError};

use super::tree::extract_string;

pub(super) fn compile_foreign_key_action(raw: &str) -> Result<crate::ast::ForeignKeyAction> {
    use crate::ast::ForeignKeyAction;
    match raw.as_bytes().first().copied() {
        None | Some(0) | Some(b'a') => Ok(ForeignKeyAction::NoAction),
        Some(b'r') => Ok(ForeignKeyAction::Restrict),
        Some(b'c') => Ok(ForeignKeyAction::Cascade),
        Some(b'n') => Ok(ForeignKeyAction::SetNull),
        Some(b'd') => Ok(ForeignKeyAction::SetDefault),
        Some(other) => Err(SQLError::Unsupported(format!(
            "unsupported FOREIGN KEY action byte {other:?}"
        ))),
    }
}

pub(super) fn compile_foreign_key_match(raw: &str) -> Result<crate::ast::ForeignKeyMatch> {
    use crate::ast::ForeignKeyMatch;
    match raw.as_bytes().first().copied() {
        None | Some(0) | Some(b's') => Ok(ForeignKeyMatch::Simple),
        Some(b'f') => Ok(ForeignKeyMatch::Full),
        Some(b'p') => Err(SQLError::Unsupported(
            "FOREIGN KEY MATCH PARTIAL is not implemented by PostgreSQL".into(),
        )),
        Some(other) => Err(SQLError::Unsupported(format!(
            "unsupported FOREIGN KEY match byte {other:?}"
        ))),
    }
}

pub(super) fn validate_foreign_key_set_columns(
    local_columns: &[String],
    set_columns: &[String],
    raw_delete_action: &str,
) -> Result<()> {
    if set_columns.is_empty() {
        return Ok(());
    }
    let action = compile_foreign_key_action(raw_delete_action)?;
    if !matches!(
        action,
        crate::ast::ForeignKeyAction::SetNull | crate::ast::ForeignKeyAction::SetDefault
    ) {
        return Err(SQLError::Unsupported(
            "FOREIGN KEY column lists are only valid for ON DELETE SET NULL/DEFAULT".into(),
        ));
    }
    for col in set_columns {
        if !local_columns.iter().any(|local| local == col) {
            return Err(SQLError::Unsupported(format!(
                "FOREIGN KEY SET column `{col}` is not part of the local key"
            )));
        }
    }
    Ok(())
}

pub(super) fn raw_type_name(col: &pg_query::protobuf::ColumnDef) -> Result<Option<String>> {
    let Some(type_name) = col.type_name.as_ref() else {
        return Ok(None);
    };
    let names = type_name
        .names
        .iter()
        .map(extract_string)
        .collect::<Result<Vec<_>>>()?;
    Ok(names.last().map(|name| name.to_lowercase()))
}

pub(super) fn compile_type_name(col: &pg_query::protobuf::ColumnDef) -> Result<ColumnType> {
    let Some(type_name) = col.type_name.as_ref() else {
        return Err(SQLError::Internal(format!(
            "column `{}` has no type",
            col.colname
        )));
    };
    compile_pg_type_name(type_name, &col.colname)
}

pub(super) fn compile_pg_type_name(
    type_name: &pg_query::protobuf::TypeName,
    column_name: &str,
) -> Result<ColumnType> {
    let names = type_name
        .names
        .iter()
        .map(extract_string)
        .collect::<Result<Vec<_>>>()?;
    let raw = names
        .last()
        .ok_or_else(|| {
            SQLError::Internal(format!(
                "type name for `{column_name}` has no name components"
            ))
        })?
        .to_lowercase();
    let base = match raw.as_str() {
        "smallint" | "int2" | "smallserial" | "serial2" => Ok(ColumnType::SmallInteger),
        "int" | "int4" | "integer" | "serial" | "serial4" => Ok(ColumnType::Integer),
        "bigint" | "int8" | "bigserial" | "serial8" => Ok(ColumnType::BigInteger),
        "oid" => Ok(ColumnType::Oid),
        "xid" => Ok(ColumnType::Xid),
        "text" => Ok(ColumnType::Text),
        "name" => Ok(ColumnType::Name),
        "uuid" => Ok(ColumnType::Uuid),
        "varchar" | "character varying" => {
            if type_name.typmods.len() > 1 {
                return Err(SQLError::TypeMismatch(format!(
                    "CHARACTER VARYING accepts at most one length modifier, got {}",
                    type_name.typmods.len()
                )));
            }
            let length = type_name
                .typmods
                .first()
                .map(expect_positive_character_length)
                .transpose()?;
            Ok(ColumnType::Varchar(length))
        }
        "character" | "char" | "bpchar" => {
            if type_name.typmods.len() > 1 {
                return Err(SQLError::TypeMismatch(format!(
                    "CHARACTER accepts at most one length modifier, got {}",
                    type_name.typmods.len()
                )));
            }
            let length = type_name
                .typmods
                .first()
                .map(expect_positive_character_length)
                .transpose()?
                .unwrap_or(1);
            Ok(ColumnType::Character(length))
        }
        "bool" | "boolean" => Ok(ColumnType::Boolean),
        "real" | "float4" => Ok(ColumnType::Real),
        "float8" | "double" | "double precision" => Ok(ColumnType::DoublePrecision),
        "numeric" | "decimal" => {
            if type_name.typmods.len() > 2 {
                return Err(SQLError::TypeMismatch(format!(
                    "NUMERIC accepts at most precision and scale, got {} modifiers",
                    type_name.typmods.len()
                )));
            }
            let mut typmods_iter = type_name.typmods.iter();
            let precision = typmods_iter
                .next()
                .map(|n| {
                    let value = expect_integer_const(n)?;
                    if !(1..=1000).contains(&value) {
                        return Err(SQLError::TypeMismatch(format!(
                            "NUMERIC precision must be between 1 and 1000, got {value}"
                        )));
                    }
                    Ok(value as u32)
                })
                .transpose()?;
            let scale = typmods_iter
                .next()
                .map(|n| {
                    let value = expect_integer_const(n)?;
                    if !(-1000..=1000).contains(&value) {
                        return Err(SQLError::TypeMismatch(format!(
                            "NUMERIC scale must be between -1000 and 1000, got {value}"
                        )));
                    }
                    Ok(value as i32)
                })
                .transpose()?;
            // PostgreSQL semantics: NUMERIC(precision) without an
            // explicit scale defaults to scale=0, rounding to integers.
            let scale = scale.or(precision.map(|_| 0));
            Ok(ColumnType::Numeric { precision, scale })
        }
        "date" => Ok(ColumnType::Date),
        "time" | "time without time zone" => Ok(ColumnType::Time),
        "timetz" | "time with time zone" => Ok(ColumnType::TimeTz),
        "timestamp" | "datetime" | "timestamp without time zone" => Ok(ColumnType::Timestamp),
        "timestamptz" | "timestamp with time zone" => Ok(ColumnType::TimestampTz),
        "interval" => Ok(ColumnType::Interval),
        "json" => Ok(ColumnType::Json),
        "jsonb" => Ok(ColumnType::JsonB),
        "bytea" => Ok(ColumnType::Bytea),
        "regproc" => Ok(ColumnType::Regproc),
        "regclass" => Ok(ColumnType::Regclass),
        "regtype" => Ok(ColumnType::Regtype),
        "pg_node_tree" => Ok(ColumnType::PgNodeTree),
        "aclitem" => Ok(ColumnType::AclItem),
        "int2vector" => Ok(ColumnType::Int2Vector),
        "oidvector" => Ok(ColumnType::OidVector),
        "vector" => {
            // VECTOR(N): the dimension is the only typmod argument.
            let [arg] = type_name.typmods.as_slice() else {
                return Err(SQLError::Unsupported(
                    "VECTOR requires exactly one dimension".into(),
                ));
            };
            let raw_dim = expect_integer_const(arg)?;
            let dim = u32::try_from(raw_dim).map_err(|_| {
                SQLError::TypeMismatch(format!(
                    "VECTOR dimension must be between 1 and {}, got {raw_dim}",
                    u32::MAX
                ))
            })?;
            if dim == 0 {
                return Err(SQLError::TypeMismatch(
                    "VECTOR dimension must be greater than zero".into(),
                ));
            }
            Ok(ColumnType::Vector(dim))
        }
        "tensor" => {
            // TENSOR(N): an array of N-dimensional vectors.
            let [arg] = type_name.typmods.as_slice() else {
                return Err(SQLError::Unsupported(
                    "TENSOR requires exactly one dimension".into(),
                ));
            };
            let raw_dim = expect_integer_const(arg)?;
            let dim = u32::try_from(raw_dim).map_err(|_| {
                SQLError::TypeMismatch(format!(
                    "TENSOR dimension must be between 1 and {}, got {raw_dim}",
                    u32::MAX
                ))
            })?;
            if dim == 0 {
                return Err(SQLError::TypeMismatch(
                    "TENSOR dimension must be greater than zero".into(),
                ));
            }
            Ok(ColumnType::Tensor(dim))
        }
        other => Err(SQLError::Unsupported(format!(
            "column `{column_name}` type `{other}` is not supported"
        ))),
    }?;
    Ok(type_name
        .array_bounds
        .iter()
        .fold(base, |element, _| ColumnType::Array(Box::new(element))))
}

fn expect_positive_character_length(node: &Node) -> Result<u32> {
    let length = expect_integer_const(node)?;
    u32::try_from(length)
        .ok()
        .filter(|length| *length > 0)
        .ok_or_else(|| {
            SQLError::TypeMismatch(format!(
                "character length must be greater than zero, got {length}"
            ))
        })
}

fn expect_integer_const(node: &Node) -> Result<i64> {
    let Some(inner) = node.node.as_ref() else {
        return Err(SQLError::Internal("missing const node".into()));
    };
    match inner {
        NodeEnum::AConst(c) => match &c.val {
            Some(pg_query::protobuf::a_const::Val::Ival(i)) => Ok(i64::from(i.ival)),
            Some(pg_query::protobuf::a_const::Val::Fval(f)) => {
                f.fval.parse::<i64>().map_err(|_| {
                    SQLError::TypeMismatch(format!(
                        "type modifier must be an integer, got `{}`",
                        f.fval
                    ))
                })
            }
            other => Err(SQLError::Internal(format!(
                "expected integer constant, got {other:?}"
            ))),
        },
        _ => Err(SQLError::Internal(format!(
            "expected A_Const, got {inner:?}"
        ))),
    }
}
