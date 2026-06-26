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

pub(super) fn raw_type_name(col: &pg_query::protobuf::ColumnDef) -> Option<String> {
    let type_name = col.type_name.as_ref()?;
    let names: Vec<String> = type_name
        .names
        .iter()
        .filter_map(|n| extract_string(n).ok())
        .collect();
    Some(names.last().cloned().unwrap_or_default().to_lowercase())
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
    let names: Vec<String> = type_name
        .names
        .iter()
        .filter_map(|n| extract_string(n).ok())
        .collect();
    let raw = names.last().cloned().unwrap_or_default().to_lowercase();
    match raw.as_str() {
        "int" | "int4" | "integer" | "bigint" | "int8" | "smallint" | "int2" | "serial"
        | "bigserial" | "serial4" | "serial8" => Ok(ColumnType::Integer),
        "text" | "varchar" | "character" | "char" | "bpchar" | "name" | "uuid" => {
            Ok(ColumnType::Text)
        }
        "bool" | "boolean" => Ok(ColumnType::Integer),
        "real" | "float4" | "float8" | "double" | "double precision" => Ok(ColumnType::Real),
        "numeric" | "decimal" => {
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
        "json" => Ok(ColumnType::Json),
        "jsonb" => Ok(ColumnType::JsonB),
        "bytea" => Ok(ColumnType::Bytea),
        "vector" => {
            // VECTOR(N): the dimension is the only typmod argument.
            let Some(arg) = type_name.typmods.first() else {
                return Err(SQLError::Unsupported(
                    "VECTOR without dimension is not supported".into(),
                ));
            };
            let dim = expect_integer_const(arg)? as u32;
            Ok(ColumnType::Vector(dim))
        }
        "tensor" => {
            // TENSOR(N): an array of N-dimensional vectors.
            let Some(arg) = type_name.typmods.first() else {
                return Err(SQLError::Unsupported(
                    "TENSOR without dimension is not supported".into(),
                ));
            };
            let dim = expect_integer_const(arg)? as u32;
            Ok(ColumnType::Tensor(dim))
        }
        other => Err(SQLError::Unsupported(format!(
            "column `{column_name}` type `{other}` is not supported"
        ))),
    }
}

fn expect_integer_const(node: &Node) -> Result<i64> {
    let Some(inner) = node.node.as_ref() else {
        return Err(SQLError::Internal("missing const node".into()));
    };
    match inner {
        NodeEnum::AConst(c) => match &c.val {
            Some(pg_query::protobuf::a_const::Val::Ival(i)) => Ok(i64::from(i.ival)),
            Some(pg_query::protobuf::a_const::Val::Fval(f)) => f
                .fval
                .parse::<f64>()
                .map(|v| v as i64)
                .map_err(|e| SQLError::Internal(e.to_string())),
            other => Err(SQLError::Internal(format!(
                "expected integer constant, got {other:?}"
            ))),
        },
        _ => Err(SQLError::Internal(format!(
            "expected A_Const, got {inner:?}"
        ))),
    }
}
