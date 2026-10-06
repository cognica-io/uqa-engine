//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reconstruct a selected catalog routine without executing or rebinding its body.

use uqa_core::Value;
use uqa_sql::{
    ast::{FunctionBody, FunctionParallel, FunctionVolatility},
    expr::quote_ident,
    routines::{builtin_routine_support_oid, definition_output},
    SQLError,
};

use super::{
    builtin_arguments, find_routine, routine_oid_argument, routine_result, routine_sqlbody,
    user_arguments, CatalogContext, Routine,
};

/// `pg_get_functiondef` observes a stored identity and emits its complete replacement declaration.
pub fn pg_get_functiondef_value(
    context: &CatalogContext<'_>,
    arguments: &[Value],
) -> Result<Value, SQLError> {
    let Some(oid) = routine_oid_argument("pg_get_functiondef", arguments)? else {
        return Ok(Value::Null);
    };
    let Some(routine) = find_routine(context, oid)? else {
        return Ok(Value::Null);
    };
    definition(context, &routine).map(Value::Str)
}

struct Attributes<'a> {
    kind: &'static str,
    language: &'a str,
    volatility: &'a str,
    parallel: &'a str,
    strict: bool,
    security_definer: bool,
    leakproof: bool,
    cost: f32,
    rows: f32,
    support: i64,
    config: &'a [(String, String)],
    source: Option<&'a str>,
}

fn attributes(routine: &Routine) -> Result<Attributes<'_>, SQLError> {
    Ok(match routine {
        Routine::User(function) => {
            let def = &function.def;
            Attributes {
                kind: if def.is_procedure { "p" } else { "f" },
                language: &def.language,
                volatility: match def.volatility {
                    FunctionVolatility::Immutable => "i",
                    FunctionVolatility::Stable => "s",
                    FunctionVolatility::Volatile => "v",
                },
                parallel: match def.parallel {
                    FunctionParallel::Safe => "s",
                    FunctionParallel::Restricted => "r",
                    FunctionParallel::Unsafe => "u",
                },
                strict: def.strict,
                security_definer: def.security.security_definer,
                leakproof: def.security.leakproof,
                cost: def.cost.unwrap_or(100.0),
                rows: if def.returns_set() {
                    def.rows.unwrap_or(1000.0)
                } else {
                    0.0
                },
                support: def
                    .support
                    .as_deref()
                    .map(|support| {
                        builtin_routine_support_oid(support).ok_or_else(|| {
                            SQLError::Internal("unknown stored routine support identity".into())
                        })
                    })
                    .transpose()?
                    .unwrap_or(0),
                config: &def.config,
                source: match &def.body {
                    FunctionBody::Source(source) => Some(source),
                    FunctionBody::Statements(_) => None,
                },
            }
        }
        Routine::Builtin(entry) => Attributes {
            kind: entry.kind,
            language: uqa_sql::catalog::languages::language_name(
                u32::try_from(entry.language())
                    .map_err(|_| SQLError::Internal("invalid routine language OID".into()))?,
            )
            .ok_or_else(|| SQLError::Internal("unknown routine language OID".into()))?,
            volatility: entry.volatility,
            parallel: entry.parallel,
            strict: entry.strict,
            security_definer: false,
            leakproof: entry.leakproof,
            cost: 1.0,
            rows: entry.estimated_rows() as f32,
            support: entry.support_oid(),
            config: &[],
            source: Some(entry.source),
        },
    })
}

fn definition(context: &CatalogContext<'_>, routine: &Routine) -> Result<String, SQLError> {
    let (schema, name, arguments) = match routine {
        Routine::User(function) => {
            let (schema, name) =
                super::super::helpers::oids::split_schema_name(&function.def.name)?;
            (
                schema,
                name,
                user_arguments(context, function, false, true)?.0,
            )
        }
        Routine::Builtin(entry) => {
            if entry.kind == "a" {
                return Err(SQLError::Routine {
                    sqlstate: "42809".into(),
                    message: format!("\"{}\" is an aggregate function", entry.name),
                });
            }
            (
                "pg_catalog".into(),
                entry.name.into(),
                builtin_arguments(context, entry, true)?,
            )
        }
    };
    let attributes = attributes(routine)?;
    let kind = if attributes.kind == "p" {
        "PROCEDURE"
    } else {
        "FUNCTION"
    };
    let mut output = format!(
        "CREATE OR REPLACE {kind} {}.{}({arguments})\n",
        quote_ident(&schema),
        quote_ident(&name)
    );
    if attributes.kind != "p" {
        let Value::Str(result) = routine_result(context, routine)? else {
            return Err(SQLError::Internal(
                "function definition has no result type".into(),
            ));
        };
        output.push_str(" RETURNS ");
        output.push_str(&result);
        output.push('\n');
    }
    output.push_str(" LANGUAGE ");
    output.push_str(&quote_ident(attributes.language));
    output.push('\n');
    output.push_str(&attribute_clause(context, &attributes)?);
    if !attributes.config.is_empty() {
        let (_, setting) = context
            .session
            .show_parameter("standard_conforming_strings")?;
        output.push_str(&definition_output::configuration_clauses(
            attributes.config,
            setting == "on",
        )?);
    }
    match routine_sqlbody(context, routine)? {
        Value::Str(body) => output.push_str(&body),
        Value::Null => output.push_str(&definition_output::source_clause(
            attributes.source.ok_or_else(|| {
                SQLError::Internal("routine definition has no stored body".into())
            })?,
            attributes.kind == "p",
        )),
        _ => {
            return Err(SQLError::Internal(
                "routine body definition is not text".into(),
            ))
        }
    }
    output.push('\n');
    Ok(output)
}

fn attribute_clause(
    context: &CatalogContext<'_>,
    attributes: &Attributes<'_>,
) -> Result<String, SQLError> {
    let mut output = String::new();
    if attributes.kind == "w" {
        output.push_str(" WINDOW");
    }
    output.push_str(match attributes.volatility {
        "i" => " IMMUTABLE",
        "s" => " STABLE",
        _ => "",
    });
    output.push_str(match attributes.parallel {
        "s" => " PARALLEL SAFE",
        "r" => " PARALLEL RESTRICTED",
        _ => "",
    });
    if attributes.strict {
        output.push_str(" STRICT");
    }
    if attributes.security_definer {
        output.push_str(" SECURITY DEFINER");
    }
    if attributes.leakproof {
        output.push_str(" LEAKPROOF");
    }
    let default_cost = if matches!(attributes.language, "internal" | "c") {
        1.0
    } else {
        100.0
    };
    if attributes.cost != default_cost {
        output.push_str(" COST ");
        output.push_str(&definition_output::estimate(attributes.cost));
    }
    if attributes.rows > 0.0 && attributes.rows != 1000.0 {
        output.push_str(" ROWS ");
        output.push_str(&definition_output::estimate(attributes.rows));
    }
    if attributes.support != 0 {
        let names = super::super::regtypes::routine_name_parts(context, attributes.support)?
            .ok_or_else(|| SQLError::Internal("missing stored routine support identity".into()))?;
        output.push_str(" SUPPORT ");
        output.push_str(
            &names
                .iter()
                .map(|name| quote_ident(name))
                .collect::<Vec<_>>()
                .join("."),
        );
    }
    if !output.is_empty() {
        output.push('\n');
    }
    Ok(output)
}
