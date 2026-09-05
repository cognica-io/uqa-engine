//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` type display with nullable typmods and catalog-visible names.

use super::{format_regtype, regtype_output_catalog, Engine, SQLError, Value};

pub(in crate::sql) fn format_type_value(
    engine: &Engine,
    args: &[Value],
) -> Result<Value, SQLError> {
    let [oid, modifier] = args else {
        return Err(SQLError::BadArity {
            name: "format_type".into(),
            expected: "2".into(),
            actual: args.len(),
        });
    };
    let oid = match oid {
        Value::Null => return Ok(Value::Null),
        Value::Int(oid) => *oid,
        _ => return Err(SQLError::TypeMismatch("format_type requires an oid".into())),
    };
    if oid == 0 {
        return Ok(Value::Str("-".into()));
    }
    let modifier = match modifier {
        Value::Null => None,
        Value::Int(value) => Some(*value),
        _ => {
            return Err(SQLError::TypeMismatch(
                "format_type requires an integer typmod".into(),
            ))
        }
    };
    let catalog = regtype_output_catalog(engine)?;
    let Some(entry) = catalog.types.get(&oid) else {
        return Ok(Value::Str("???".into()));
    };
    let array = catalog
        .types
        .get(&entry.element_oid)
        .is_some_and(|element| element.array_oid == oid);
    let oid = if array { entry.element_oid } else { oid };
    let default =
        || format_regtype(engine, &catalog, oid).map(|name| name.unwrap_or_else(|| "???".into()));
    let with_modifier = modifier.filter(|modifier| *modifier >= 0);
    let mut name = match oid {
        16 => "boolean".into(),
        20 => "bigint".into(),
        21 => "smallint".into(),
        23 => "integer".into(),
        700 => "real".into(),
        701 => "double precision".into(),
        114 => "json".into(),
        1042 | 1043 => {
            let name = if oid == 1043 {
                "character varying"
            } else if modifier.is_some() && with_modifier.is_none() {
                "bpchar"
            } else {
                "character"
            };
            match with_modifier.filter(|modifier| *modifier >= 4) {
                Some(modifier) => format!("{name}({})", modifier - 4),
                None => name.into(),
            }
        }
        1700 => match with_modifier.filter(|modifier| *modifier >= 4) {
            Some(modifier) => {
                let encoded = modifier - 4;
                let precision = (encoded >> 16) & 65535;
                let scale = ((encoded & 2047) ^ 1024) - 1024;
                format!("numeric({precision},{scale})")
            }
            None => "numeric".into(),
        },
        1083 | 1266 | 1114 | 1184 => {
            let base = if matches!(oid, 1083 | 1266) {
                "time"
            } else {
                "timestamp"
            };
            let zone = if matches!(oid, 1266 | 1184) {
                "with"
            } else {
                "without"
            };
            let precision =
                with_modifier.map_or_else(String::new, |modifier| format!("({modifier})"));
            format!("{base}{precision} {zone} time zone")
        }
        1186 => interval_name(with_modifier)?,
        _ => match with_modifier {
            Some(modifier) => format!("{}({modifier})", default()?),
            None => default()?,
        },
    };
    if array {
        name.push_str("[]");
    }
    Ok(Value::Str(name))
}

fn interval_name(modifier: Option<i64>) -> Result<String, SQLError> {
    let Some(modifier) = modifier else {
        return Ok("interval".into());
    };
    let fields = (modifier >> 16) & 32767;
    let precision = modifier & 65535;
    let range = match fields {
        4 => " year",
        2 => " month",
        8 => " day",
        1024 => " hour",
        2048 => " minute",
        4096 => " second",
        6 => " year to month",
        1032 => " day to hour",
        3080 => " day to minute",
        7176 => " day to second",
        3072 => " hour to minute",
        7168 => " hour to second",
        6144 => " minute to second",
        32767 => "",
        _ => {
            return Err(SQLError::Internal(format!(
                "invalid INTERVAL typmod: 0x{modifier:x}"
            )))
        }
    };
    let precision = if precision == 65535 {
        String::new()
    } else {
        format!("({precision})")
    };
    Ok(format!("interval{range}{precision}"))
}
