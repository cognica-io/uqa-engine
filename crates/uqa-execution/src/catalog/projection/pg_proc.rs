//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Virtual `pg_catalog.pg_proc` relation builder.

use super::builtin_routines::PG18_BUILTIN_ROUTINE_GROUPS;
use super::helpers::acl::object_acl_items;
use super::helpers::oids::{
    current_user_oid, namespace_oid, schema_oid, split_schema_name, stable_object_oid, stable_oid,
};
use super::helpers::rows::{
    bool_value, catalog_array, catalog_oidvector, catalog_usize, int_value, row, str_value,
};
use super::helpers::type_metadata::routine_variadic_element_oid;
use super::regtypes::catalog_routine_type_oid;
use crate::catalog::CatalogReadView;
use uqa_core::Value;
use uqa_sql::catalog::languages::language_oid;
use uqa_sql::registry::registered_names;
use uqa_sql::routines::{builtin_routine_support_oid, SQLUserFunction};
use uqa_sql::{ResultRow, SQLError};

/// The routine's public OID: the recorded one, or for a routine created before OIDs were recorded, the one its identity derives.
pub fn user_routine_catalog_oid(function: &SQLUserFunction) -> Result<i64, SQLError> {
    if let Some(oid) = function.def.catalog_oid {
        return Ok(i64::from(oid));
    }
    let object_id = function.def.object_id.ok_or_else(|| {
        SQLError::Internal(format!(
            "routine `{}` has no catalog object identity",
            function.def.name
        ))
    })?;
    Ok(stable_object_oid("proc", &object_id))
}

/// Whether a routine holds `oid` in `pg_proc`: a user routine, or a registered function, whose OID derives from its name.
pub fn routine_oid_in_use(
    routines: &std::collections::BTreeMap<String, Vec<std::sync::Arc<SQLUserFunction>>>,
    oid: i64,
) -> Result<bool, SQLError> {
    for function in routines.values().flatten() {
        if user_routine_catalog_oid(function)? == oid {
            return Ok(true);
        }
    }
    Ok(registered_names()
        .into_iter()
        .any(|name| stable_oid("proc", name) == oid))
}

pub fn build_pg_proc(
    catalog: &CatalogReadView,
    resolution: &crate::catalog::RelationNameResolution,
) -> Result<Vec<ResultRow>, SQLError> {
    build_pg_proc_rows(catalog, resolution, true)
}

/// The `pg_proc` rows with `proargdefaults` left NULL, for the `reg*` output catalog: printing an argument default may print a `reg*` constant, whose output function reads that catalog.
pub fn build_pg_proc_without_defaults(
    catalog: &CatalogReadView,
    resolution: &crate::catalog::RelationNameResolution,
) -> Result<Vec<ResultRow>, SQLError> {
    build_pg_proc_rows(catalog, resolution, false)
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves catalog column and OID order"
)]
fn build_pg_proc_rows(
    catalog: &CatalogReadView,
    resolution: &crate::catalog::RelationNameResolution,
    with_defaults: bool,
) -> Result<Vec<ResultRow>, SQLError> {
    let mut rows: Vec<ResultRow> = PG18_BUILTIN_ROUTINE_GROUPS
        .iter()
        .flat_map(|group| group.iter())
        .map(|routine| {
            Ok(row([
                ("oid", int_value(routine.oid)),
                ("proname", str_value(routine.name)),
                ("pronamespace", int_value(schema_oid("pg_catalog"))),
                ("proowner", int_value(current_user_oid())),
                ("prolang", int_value(routine.language())),
                ("procost", Value::Float(1.0)),
                ("prorows", Value::Float(routine.estimated_rows())),
                ("provariadic", int_value(routine.variadic_type())),
                ("prosupport", int_value(routine.support_oid())),
                ("prokind", str_value(routine.kind)),
                ("prosecdef", bool_value(false)),
                ("proleakproof", bool_value(routine.leakproof)),
                ("proisstrict", bool_value(routine.strict)),
                ("proretset", bool_value(routine.returns_set())),
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
                (
                    "proargtypes",
                    catalog_oidvector(
                        routine
                            .argument_types
                            .iter()
                            .copied()
                            .map(Value::Int)
                            .collect(),
                        "pg_proc.proargtypes",
                    )?,
                ),
                (
                    "proallargtypes",
                    routine
                        .all_argument_types()
                        .map_or(Ok(Value::Null), |types| {
                            catalog_array(
                                types.iter().copied().map(int_value).collect(),
                                "pg_proc.proallargtypes",
                            )
                        })?,
                ),
                (
                    "proargmodes",
                    routine.argument_modes().map_or(Ok(Value::Null), |modes| {
                        catalog_array(
                            modes.iter().copied().map(str_value).collect(),
                            "pg_proc.proargmodes",
                        )
                    })?,
                ),
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
                (
                    "proacl",
                    builtin_acl_value(
                        catalog,
                        u32::try_from(routine.oid).expect("builtin routine OID"),
                    )?,
                ),
            ]))
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    for name in registered_names() {
        rows.push(row([
            ("oid", int_value(stable_oid("proc", name))),
            ("proname", str_value(name)),
            ("pronamespace", int_value(schema_oid("pg_catalog"))),
            ("proowner", int_value(current_user_oid())),
            ("prolang", int_value(0)),
            ("procost", Value::Float(1.0)),
            ("prorows", Value::Float(0.0)),
            ("provariadic", int_value(0)),
            ("prosupport", int_value(0)),
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
            (
                "proargtypes",
                catalog_oidvector(Vec::new(), "pg_proc.proargtypes")?,
            ),
            ("proallargtypes", Value::Null),
            ("proargmodes", Value::Null),
            ("proargnames", Value::Null),
            ("proargdefaults", Value::Null),
            ("protrftypes", Value::Null),
            ("prosrc", str_value(name)),
            ("probin", Value::Null),
            ("prosqlbody", Value::Null),
            ("proconfig", Value::Null),
            (
                "proacl",
                builtin_acl_value(
                    catalog,
                    u32::try_from(stable_oid("proc", name)).expect("registered routine OID"),
                )?,
            ),
        ]));
    }
    for function in catalog.all_sql_functions() {
        let def = &function.def;
        let language = language_oid(&def.language).ok_or_else(|| {
            SQLError::Internal(format!(
                "routine `{}` references unknown language `{}`",
                def.name, def.language
            ))
        })?;
        let (routine_schema, routine_name) = split_schema_name(&def.name)?;
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
            .filter(|_| with_defaults)
            .filter_map(|parameter| {
                super::routine_definitions::routine_parameter_default_text(
                    catalog, resolution, parameter,
                )
                .transpose()
            })
            .collect::<Result<Vec<_>, SQLError>>()?;
        let argument_defaults = if argument_defaults.is_empty() {
            Value::Null
        } else {
            str_value(argument_defaults.join(", "))
        };
        let argument_type_oids = input_params
            .iter()
            .map(|parameter| int_value(catalog_routine_type_oid(catalog, &parameter.type_name)))
            .collect::<Vec<_>>();
        let has_non_input_mode = def
            .params
            .iter()
            .any(|parameter| parameter.mode != uqa_sql::ast::FunctionParamMode::In);
        let all_argument_type_oids = if has_non_input_mode {
            catalog_array(
                def.params
                    .iter()
                    .map(|parameter| {
                        int_value(catalog_routine_type_oid(catalog, &parameter.type_name))
                    })
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
            .map(|parameter| {
                // A user-defined element type is named by identity, whose OID is the element's.
                let canonical =
                    uqa_sql::type_resolution::canonical_routine_type_name(&parameter.type_name);
                match uqa_sql::ast::UserTypeIdentity::parse(&canonical) {
                    Some(identity) if identity.dimensions > 0 => Ok(i64::from(identity.oid)),
                    _ => routine_variadic_element_oid(&parameter.type_name),
                }
            })
            .transpose()?
            .unwrap_or(0);
        let return_type_oid = routine_result_type_oid(catalog, def);
        rows.push(row([
            ("oid", int_value(user_routine_catalog_oid(&function)?)),
            ("proname", str_value(routine_name)),
            (
                "pronamespace",
                int_value(namespace_oid(catalog, &routine_schema)),
            ),
            (
                "proowner",
                int_value(uqa_sql::routines::security::bound_routine_owner(def)?.oid),
            ),
            ("prolang", int_value(i64::from(language))),
            // `CreateFunction`'s defaults for SQL and PL/pgSQL routines without COST or ROWS.
            ("procost", Value::Float(def.cost.map_or(100.0, f64::from))),
            (
                "prorows",
                Value::Float(
                    def.rows
                        .map_or_else(|| if def.returns_set() { 1000.0 } else { 0.0 }, f64::from),
                ),
            ),
            ("provariadic", int_value(variadic_type_oid)),
            (
                "prosupport",
                int_value(def.support.as_deref().map_or(0, |support| {
                    builtin_routine_support_oid(support)
                        .unwrap_or_else(|| stable_oid("proc", support))
                })),
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
            (
                "proargtypes",
                catalog_oidvector(argument_type_oids, "pg_proc.proargtypes")?,
            ),
            ("proallargtypes", all_argument_type_oids),
            ("proargmodes", arg_modes),
            ("proargnames", arg_names),
            ("proargdefaults", argument_defaults),
            ("protrftypes", Value::Null),
            ("prosrc", str_value(source)),
            ("probin", Value::Null),
            ("prosqlbody", Value::Null),
            ("proconfig", routine_config_catalog_value(def)?),
            ("proacl", routine_acl_catalog_value(catalog, def)?),
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

fn routine_acl_catalog_value(
    catalog: &CatalogReadView,
    def: &uqa_sql::ast::CreateFunction,
) -> Result<Value, SQLError> {
    let Some(acl) = def.execute_acl.as_ref() else {
        return Ok(Value::Null);
    };
    catalog_array(
        object_acl_items(&catalog.snapshot().definitions.roles, acl, 'X', "routine")?,
        "pg_proc.proacl",
    )
}

/// `prorettype` of a user routine: `void` for a procedure without output parameters and `record` for one with them; for a function, its declared result type, or that of its single output parameter, or `record` for several.
pub(crate) fn routine_result_type_oid(
    catalog: &CatalogReadView,
    def: &uqa_sql::ast::CreateFunction,
) -> i64 {
    if def.is_procedure {
        return if def.output_params().is_empty() {
            2278
        } else {
            2249
        };
    }
    match &def.returns {
        uqa_sql::ast::FunctionReturns::Scalar { type_name }
        | uqa_sql::ast::FunctionReturns::SetOf { type_name } => {
            catalog_routine_type_oid(catalog, type_name)
        }
        uqa_sql::ast::FunctionReturns::Table | uqa_sql::ast::FunctionReturns::None => {
            match def.output_params().as_slice() {
                [output] => catalog_routine_type_oid(catalog, &output.type_name),
                [] => 2278,
                _ => 2249,
            }
        }
    }
}

fn builtin_acl_value(catalog: &CatalogReadView, oid: u32) -> Result<Value, SQLError> {
    let definitions = &catalog.snapshot().definitions;
    let Some(entry) = definitions.builtin_routine_security.get(&oid) else {
        return Ok(Value::Null);
    };
    catalog_array(
        object_acl_items(
            &definitions.roles,
            &entry.execute_acl,
            'X',
            "builtin routine",
        )?,
        "pg_proc.proacl",
    )
}
