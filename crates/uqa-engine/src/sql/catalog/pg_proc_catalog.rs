//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Virtual `pg_proc` row synthesis.

use super::builtin_routines::PG18_BUILTIN_ROUTINES;
use super::helpers::{
    bool_value, catalog_array, catalog_usize, current_user_oid, int_value, list_int,
    routine_type_oid, routine_variadic_element_oid, row, schema_expr_text, schema_oid,
    split_schema_name, stable_oid, str_value,
};
use super::{canonical_routine_type_name, registered_names, Engine, ResultRow, SQLError, Value};
use crate::engine_roles::role_oid;
use crate::engine_user_functions::builtin_routine_support_oid;

pub(super) fn build_pg_proc(engine: &Engine) -> Result<Vec<ResultRow>, SQLError> {
    let mut rows: Vec<ResultRow> = PG18_BUILTIN_ROUTINES
        .iter()
        .map(|routine| {
            Ok(row([
                ("oid", int_value(routine.oid)),
                ("proname", str_value(routine.name)),
                ("pronamespace", int_value(schema_oid("pg_catalog"))),
                ("proowner", int_value(current_user_oid())),
                ("prolang", int_value(routine.language())),
                ("procost", Value::Float(1.0)),
                ("prorows", Value::Float(0.0)),
                ("provariadic", int_value(0)),
                ("prosupport", str_value("-")),
                ("prokind", str_value(routine.kind)),
                ("prosecdef", bool_value(false)),
                ("proleakproof", bool_value(routine.leakproof)),
                ("proisstrict", bool_value(routine.strict)),
                ("proretset", bool_value(false)),
                ("provolatile", str_value(routine.volatility)),
                ("proparallel", str_value(routine.parallel)),
                (
                    "pronargs",
                    int_value(catalog_usize(
                        routine.argument_types.len(),
                        "pg_proc built-in argument count",
                    )?),
                ),
                (
                    "pronargdefaults",
                    int_value(catalog_usize(
                        routine.default_arguments,
                        "pg_proc built-in default argument count",
                    )?),
                ),
                ("prorettype", int_value(routine.return_type)),
                ("proargtypes", list_int(routine.argument_types)),
                ("proallargtypes", Value::Null),
                ("proargmodes", Value::Null),
                (
                    "proargnames",
                    if routine.argument_names.is_empty() {
                        Value::Null
                    } else {
                        catalog_array(
                            routine
                                .argument_names
                                .iter()
                                .map(|name| str_value(*name))
                                .collect(),
                            "pg_proc.proargnames",
                        )?
                    },
                ),
                (
                    "proargdefaults",
                    routine.argument_defaults.map_or(Value::Null, str_value),
                ),
                ("protrftypes", Value::Null),
                ("prosrc", str_value(routine.source)),
                ("probin", Value::Null),
                (
                    "prosqlbody",
                    routine.sql_body().map_or(Value::Null, str_value),
                ),
                ("proconfig", Value::Null),
                ("proacl", Value::Null),
            ]))
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    rows.extend(registered_names().into_iter().map(|name| {
        row([
            ("oid", int_value(stable_oid("proc", name))),
            ("proname", str_value(name)),
            ("pronamespace", int_value(schema_oid("pg_catalog"))),
            ("proowner", int_value(current_user_oid())),
            ("prolang", int_value(0)),
            ("procost", Value::Float(1.0)),
            ("prorows", Value::Float(0.0)),
            ("provariadic", int_value(0)),
            ("prosupport", str_value("-")),
            ("prokind", str_value("f")),
            ("prosecdef", bool_value(false)),
            ("proleakproof", bool_value(false)),
            ("proisstrict", bool_value(false)),
            ("proretset", bool_value(false)),
            ("provolatile", str_value("s")),
            ("proparallel", str_value("s")),
            ("pronargs", int_value(0)),
            ("pronargdefaults", int_value(0)),
            ("prorettype", int_value(25)),
            ("proargtypes", Value::List(Vec::new())),
            ("proallargtypes", Value::Null),
            ("proargmodes", Value::Null),
            ("proargnames", Value::Null),
            ("proargdefaults", Value::Null),
            ("protrftypes", Value::Null),
            ("prosrc", str_value(name)),
            ("probin", Value::Null),
            ("prosqlbody", Value::Null),
            ("proconfig", Value::Null),
            ("proacl", Value::Null),
        ])
    }));
    for function in engine.list_sql_functions() {
        let def = &function.def;
        let (routine_schema, routine_name) = split_schema_name(&def.name)?;
        let signature = def
            .identity_params()
            .iter()
            .map(|parameter| canonical_routine_type_name(&parameter.type_name))
            .collect::<Vec<_>>();
        let identity = format!(
            "{}:{}:{}",
            def.name,
            if def.is_procedure {
                "procedure"
            } else {
                "function"
            },
            signature.join(",")
        );
        let source = match &def.body {
            uqa_sql::ast::FunctionBody::Source(source) => source.clone(),
            uqa_sql::ast::FunctionBody::Statements(_) => String::new(),
        };
        let volatile = match def.volatility {
            uqa_sql::ast::FunctionVolatility::Immutable => "i",
            uqa_sql::ast::FunctionVolatility::Stable => "s",
            uqa_sql::ast::FunctionVolatility::Volatile => "v",
        };
        let input_params = def.identity_params();
        let defaults = input_params
            .iter()
            .filter(|parameter| parameter.default.is_some())
            .count();
        let argument_defaults = input_params
            .iter()
            .filter_map(|parameter| parameter.default.as_ref())
            .map(schema_expr_text)
            .collect::<Vec<_>>();
        let argument_defaults = if argument_defaults.is_empty() {
            Value::Null
        } else {
            str_value(argument_defaults.join(", "))
        };
        let argument_type_oids = input_params
            .iter()
            .map(|parameter| int_value(routine_type_oid(&parameter.type_name)))
            .collect::<Vec<_>>();
        let has_non_input_mode = def
            .params
            .iter()
            .any(|parameter| parameter.mode != uqa_sql::ast::FunctionParamMode::In);
        let all_argument_type_oids = if has_non_input_mode {
            catalog_array(
                def.params
                    .iter()
                    .map(|parameter| int_value(routine_type_oid(&parameter.type_name)))
                    .collect(),
                "pg_proc.proallargtypes",
            )?
        } else {
            Value::Null
        };
        let arg_modes = if has_non_input_mode {
            catalog_array(
                def.params
                    .iter()
                    .map(|parameter| {
                        str_value(match parameter.mode {
                            uqa_sql::ast::FunctionParamMode::In => "i",
                            uqa_sql::ast::FunctionParamMode::Out => "o",
                            uqa_sql::ast::FunctionParamMode::InOut => "b",
                            uqa_sql::ast::FunctionParamMode::Variadic => "v",
                            uqa_sql::ast::FunctionParamMode::Table => "t",
                        })
                    })
                    .collect(),
                "pg_proc.proargmodes",
            )?
        } else {
            Value::Null
        };
        let arg_names = if def
            .params
            .iter()
            .any(|parameter| !parameter.name.is_empty())
        {
            catalog_array(
                def.params
                    .iter()
                    .map(|parameter| str_value(parameter.name.clone()))
                    .collect(),
                "pg_proc.proargnames",
            )?
        } else {
            Value::Null
        };
        let variadic_type_oid = def
            .params
            .iter()
            .find(|parameter| parameter.mode == uqa_sql::ast::FunctionParamMode::Variadic)
            .map(|parameter| routine_variadic_element_oid(&parameter.type_name))
            .transpose()?
            .unwrap_or(0);
        let return_type_oid = if def.is_procedure {
            if def.output_params().is_empty() {
                2278
            } else {
                2249
            }
        } else {
            match &def.returns {
                uqa_sql::ast::FunctionReturns::Scalar { type_name }
                | uqa_sql::ast::FunctionReturns::SetOf { type_name } => routine_type_oid(type_name),
                uqa_sql::ast::FunctionReturns::Table | uqa_sql::ast::FunctionReturns::None => {
                    match def.output_params().as_slice() {
                        [output] => routine_type_oid(&output.type_name),
                        [] => 2278,
                        _ => 2249,
                    }
                }
            }
        };
        rows.push(row([
            ("oid", int_value(stable_oid("proc", &identity))),
            ("proname", str_value(routine_name)),
            ("pronamespace", int_value(schema_oid(&routine_schema))),
            ("proowner", int_value(role_oid(&def.owner))),
            ("prolang", int_value(0)),
            ("procost", Value::Float(100.0)),
            (
                "prorows",
                Value::Float(if def.returns_set() { 1000.0 } else { 0.0 }),
            ),
            ("provariadic", int_value(variadic_type_oid)),
            (
                "prosupport",
                def.support.as_deref().map_or_else(
                    || str_value("-"),
                    |support| {
                        int_value(
                            builtin_routine_support_oid(support)
                                .unwrap_or_else(|| stable_oid("proc", support)),
                        )
                    },
                ),
            ),
            (
                "prokind",
                str_value(if def.is_procedure { "p" } else { "f" }),
            ),
            ("prosecdef", bool_value(def.security.security_definer)),
            ("proleakproof", bool_value(def.security.leakproof)),
            ("proisstrict", bool_value(def.strict)),
            ("proretset", bool_value(def.returns_set())),
            ("provolatile", str_value(volatile)),
            (
                "proparallel",
                str_value(match def.parallel {
                    uqa_sql::ast::FunctionParallel::Unsafe => "u",
                    uqa_sql::ast::FunctionParallel::Restricted => "r",
                    uqa_sql::ast::FunctionParallel::Safe => "s",
                }),
            ),
            (
                "pronargs",
                int_value(catalog_usize(input_params.len(), "pg_proc argument count")?),
            ),
            (
                "pronargdefaults",
                int_value(catalog_usize(defaults, "pg_proc default argument count")?),
            ),
            ("prorettype", int_value(return_type_oid)),
            ("proargtypes", Value::List(argument_type_oids)),
            ("proallargtypes", all_argument_type_oids),
            ("proargmodes", arg_modes),
            ("proargnames", arg_names),
            ("proargdefaults", argument_defaults),
            ("protrftypes", Value::Null),
            ("prosrc", str_value(source)),
            ("probin", Value::Null),
            ("prosqlbody", Value::Null),
            ("proconfig", routine_config_catalog_value(def)?),
            ("proacl", routine_acl_catalog_value(def)?),
        ]));
    }
    Ok(rows)
}

fn routine_config_catalog_value(def: &uqa_sql::ast::CreateFunction) -> Result<Value, SQLError> {
    if def.config.is_empty() {
        return Ok(Value::Null);
    }
    catalog_array(
        def.config
            .iter()
            .map(|(name, value)| str_value(format!("{name}={value}")))
            .collect(),
        "pg_proc.proconfig",
    )
}

fn routine_acl_catalog_value(def: &uqa_sql::ast::CreateFunction) -> Result<Value, SQLError> {
    let Some(acl) = def.execute_acl.as_ref() else {
        return Ok(Value::Null);
    };
    let grantor = acl_identifier(&def.owner);
    let mut entries = vec![str_value(format!("{grantor}=X/{grantor}"))];
    entries.extend(
        acl.iter()
            .filter(|entry| entry.role != def.owner)
            .map(|entry| {
                let grantee = if entry.role == "PUBLIC" {
                    String::new()
                } else {
                    acl_identifier(&entry.role)
                };
                str_value(format!(
                    "{grantee}=X{}/{grantor}",
                    if entry.grant_option { "*" } else { "" }
                ))
            }),
    );
    catalog_array(entries, "pg_proc.proacl")
}

fn acl_identifier(name: &str) -> String {
    if name.bytes().enumerate().all(|(index, byte)| {
        byte == b'_' || byte.is_ascii_lowercase() || index > 0 && byte.is_ascii_digit()
    }) {
        name.to_string()
    } else {
        format!("\"{}\"", name.replace('"', "\"\""))
    }
}
