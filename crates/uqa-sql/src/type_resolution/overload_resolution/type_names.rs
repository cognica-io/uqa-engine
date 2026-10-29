//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Overload identity normalization streams through ordinary or admitted text owners.

use crate::ast::ColumnType;
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionString},
    ValueRetentionError,
};

/// Canonical type spelling used by routine identity and overload resolution.
#[must_use]
pub fn canonical_routine_type_name(type_name: &str) -> String {
    canonical_routine_type_name_with_control(type_name, &ProductionControl::uncontrolled())
        .expect("ordinary routine type normalization cannot be cancelled or limited")
        .into_uncontrolled()
        .expect("ordinary routine type normalization has no reservation")
}

pub(in crate::type_resolution) fn canonical_routine_type_name_with_control(
    type_name: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, ValueRetentionError> {
    let mut compact = ProductionString::new(*control);
    for (index, word) in type_name.split_whitespace().enumerate() {
        if index > 0 {
            compact.push(' ')?;
        }
        for character in word.chars() {
            compact.push(character.to_ascii_lowercase())?;
        }
    }
    let compact = compact.finish()?;
    if let Some(element) = array_element_spelling(&compact, control)? {
        let element = canonical_routine_type_name_with_control(element, control)?;
        return control.format(format_args!("{}[]", element.as_str()));
    }
    let without_catalog = compact
        .strip_prefix("pg_catalog.")
        .or_else(|| compact.strip_prefix("\"pg_catalog\"."))
        .unwrap_or(&compact);
    let base = strip_type_modifiers(without_catalog, control)?;
    let base = if compact.starts_with("pg_catalog.") || compact.starts_with("\"pg_catalog\".") {
        match base
            .strip_prefix('"')
            .and_then(|name| name.strip_suffix('"'))
            .and_then(crate::catalog::type_metadata::builtin_scalar_type_by_name)
        {
            Some(ty) => ty.regtype_name_with_control(control)?,
            None => base,
        }
    } else {
        base
    };
    let canonical = match base.as_str() {
        "smallint" | "int2" => "int2",
        "integer" | "int" | "int4" | "serial" | "serial4" => "int4",
        "bigint" | "int8" | "bigserial" | "serial8" => "int8",
        "real" | "float4" => "float4",
        "double" | "double precision" | "float8" => "float8",
        "decimal" | "numeric" => "numeric",
        "character varying" | "varchar" => "varchar",
        "character" | "char" | "bpchar" => "bpchar",
        "bit" | "\"bit\"" => "bit",
        "bit varying" | "varbit" => "varbit",
        "bool" | "boolean" => "bool",
        "timestamp without time zone" | "timestamp" => "timestamp",
        "timestamp with time zone" | "timestamptz" => "timestamptz",
        "time without time zone" | "time" => "time",
        "time with time zone" | "timetz" => "timetz",
        other => other,
    };
    if canonical == base.as_str() {
        Ok(base)
    } else {
        control.copy_text(canonical)
    }
}

fn array_element_spelling<'a>(
    name: &'a str,
    control: &ProductionControl<'_>,
) -> Result<Option<&'a str>, ValueRetentionError> {
    let Some(mut element) = name.strip_suffix("[]") else {
        return Ok(None);
    };
    loop {
        control.check()?;
        element = element.trim_end();
        let Some(inner) = element.strip_suffix("[]") else {
            return Ok(Some(element));
        };
        element = inner;
    }
}

fn strip_type_modifiers(
    type_name: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, ValueRetentionError> {
    let mut stripped = ProductionString::new(*control);
    let mut pending_space = false;
    let mut modifier_depth = 0usize;
    let mut quoted = false;
    let mut characters = type_name.chars().peekable();
    while let Some(character) = characters.next() {
        control.check()?;
        if character == '"' && modifier_depth == 0 {
            push_compacted(&mut stripped, character, &mut pending_space)?;
            if quoted && characters.peek() == Some(&'"') {
                push_compacted(
                    &mut stripped,
                    characters.next().expect("peeked quoted identifier escape"),
                    &mut pending_space,
                )?;
            } else {
                quoted = !quoted;
            }
            continue;
        }
        if !quoted {
            if character == '(' {
                modifier_depth += 1;
                continue;
            }
            if character == ')' && modifier_depth > 0 {
                modifier_depth -= 1;
                continue;
            }
        }
        if modifier_depth == 0 {
            push_compacted(&mut stripped, character, &mut pending_space)?;
        }
    }
    stripped.finish()
}

fn push_compacted(
    output: &mut ProductionString<'_>,
    character: char,
    pending_space: &mut bool,
) -> Result<(), ValueRetentionError> {
    if character.is_whitespace() {
        *pending_space = !output.is_empty();
        return Ok(());
    }
    if *pending_space {
        output.push(' ')?;
        *pending_space = false;
    }
    output.push(character)
}

#[must_use]
pub fn canonical_column_type_name(ty: &ColumnType) -> String {
    canonical_column_type_name_with_control(ty, &ProductionControl::uncontrolled())
        .expect("ordinary column type normalization cannot be cancelled or limited")
        .into_uncontrolled()
        .expect("ordinary column type normalization has no reservation")
}

/// Fixed overload spelling of a builtin scalar type, identical to normalizing its SQL name. Type modifiers never take part in overload identity.
pub(in crate::type_resolution) const fn builtin_canonical_type_name(
    ty: &ColumnType,
) -> Option<&'static str> {
    Some(match ty {
        ColumnType::SmallInteger => "int2",
        ColumnType::Integer => "int4",
        ColumnType::BigInteger => "int8",
        ColumnType::Real => "float4",
        ColumnType::DoublePrecision => "float8",
        ColumnType::Numeric { .. } => "numeric",
        ColumnType::Boolean => "bool",
        ColumnType::Text => "text",
        ColumnType::Name => "name",
        ColumnType::Varchar(_) => "varchar",
        ColumnType::Bpchar | ColumnType::Character(_) => "bpchar",
        ColumnType::Date => "date",
        ColumnType::Time | ColumnType::TimePrecision(_) => "time",
        ColumnType::TimeTz | ColumnType::TimeTzPrecision(_) => "timetz",
        ColumnType::Timestamp | ColumnType::TimestampPrecision(_) => "timestamp",
        ColumnType::TimestampTz | ColumnType::TimestampTzPrecision(_) => "timestamptz",
        ColumnType::Interval => "interval",
        ColumnType::Bytea => "bytea",
        ColumnType::Uuid => "uuid",
        ColumnType::Json => "json",
        ColumnType::Xml => "xml",
        ColumnType::JsonB => "jsonb",
        ColumnType::Oid => "oid",
        ColumnType::Bit(_) => "bit",
        ColumnType::Int2Vector => "int2vector",
        ColumnType::OidVector => "oidvector",
        ColumnType::Record => "record",
        ColumnType::Void => "void",
        _ => return None,
    })
}

pub(in crate::type_resolution) fn canonical_column_type_name_with_control(
    ty: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, ValueRetentionError> {
    if let Some(name) = builtin_canonical_type_name(ty) {
        return control.copy_text(name);
    }
    // User-defined type spellings are neither unique across schemas nor stable across renames, so overload matching identifies them by OID.
    if let Some(identity) = ty.user_type_identity() {
        if let Some(element) = array_element_spelling(&identity, control)? {
            return control.format(format_args!("{element}[]"));
        }
        return control.copy_text(&identity);
    }
    let name = ty.sql_name_with_control(control)?;
    canonical_routine_type_name_with_control(&name, control)
}

/// Parse a canonical enum identity produced by overload matching: the enum type OID and the number of array dimensions around it.
#[must_use]
pub fn parse_enum_type_identity(name: &str) -> Option<(u32, usize)> {
    crate::ast::UserTypeIdentity::parse(name)
        .filter(|identity| identity.kind == crate::ast::UserTypeKind::Enum)
        .map(|identity| (identity.oid, identity.dimensions))
}

#[cfg(test)]
mod tests;
