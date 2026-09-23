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
    if let Some(element) = compact.strip_suffix("[]") {
        let element = canonical_routine_type_name_with_control(element, control)?;
        return control.format(format_args!("{}[]", element.as_str()));
    }
    let without_catalog = compact.strip_prefix("pg_catalog.").unwrap_or(&compact);
    let base = strip_type_modifiers(without_catalog, control)?;
    let canonical = match base.as_str() {
        "smallint" | "int2" => "int2",
        "integer" | "int" | "int4" | "serial" | "serial4" => "int4",
        "bigint" | "int8" | "bigserial" | "serial8" => "int8",
        "real" | "float4" => "float4",
        "double" | "double precision" | "float8" => "float8",
        "decimal" | "numeric" => "numeric",
        "character varying" | "varchar" => "varchar",
        "character" | "char" | "bpchar" => "bpchar",
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

pub(in crate::type_resolution) fn canonical_column_type_name_with_control(
    ty: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, ValueRetentionError> {
    let name = ty.sql_name_with_control(control)?;
    canonical_routine_type_name_with_control(&name, control)
}

#[cfg(test)]
mod tests;
