//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `information_schema.routines`: built-in catalog routines, registered operator functions and stored SQL routines, as `PostgreSQL` 18's view derives them from `pg_proc`, `pg_type` and the routine's owner and `EXECUTE` privilege.

use super::super::builtin_routines::PG18_BUILTIN_ROUTINE_GROUPS;
use super::super::helpers::oids::split_schema_name;
use super::super::helpers::rows::{catalog_name, row, str_value};
use super::super::pg_proc::{routine_result_type_oid, user_routine_catalog_oid};
use super::super::regtypes::AliasConstantOutput;
use crate::catalog::{CatalogReadView, RelationNameResolution};
use uqa_core::Value;
use uqa_sql::ast::RoleAttribute;
use uqa_sql::ast::RoutineAclEntry;
use uqa_sql::catalog::languages::SQL_LANGUAGE;
use uqa_sql::catalog::roles::{identity::RoleSubject, RoleIdentity, RoleReference};
use uqa_sql::registry::registered_names;
use uqa_sql::routines::security::{bound_routine_owner, routine_privilege_allowed};
use uqa_sql::{ResultRow, SQLError};

#[expect(
    clippy::too_many_lines,
    reason = "preserves catalog column and OID order"
)]
pub fn build_info_routines(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<Vec<ResultRow>, SQLError> {
    let current_user = resolution.current_user();
    let builtin_owner = catalog.role_is_enabled_for(current_user, &RoleIdentity::BOOTSTRAP);
    let privileges = ExecutePrivileges::new(catalog, current_user);
    let types = AliasConstantOutput::build(catalog, resolution)?;
    let definitions = &catalog.snapshot().definitions;
    let mut rows = Vec::new();
    for routine in PG18_BUILTIN_ROUTINE_GROUPS
        .iter()
        .flat_map(|group| group.iter())
    {
        let oid = u32::try_from(routine.oid).expect("builtin catalog OID");
        let acl = definitions
            .builtin_routine_security
            .get(&oid)
            .map(|entry| entry.execute_acl.as_slice());
        if !builtin_owner && !privileges.allowed(&RoleIdentity::BOOTSTRAP, acl) {
            continue;
        }
        let procedure = routine.kind == "p";
        rows.push(row([
            ("specific_catalog", catalog_name()),
            ("specific_schema", str_value("pg_catalog")),
            (
                "specific_name",
                str_value(format!("{}_{}", routine.name, routine.oid)),
            ),
            ("routine_catalog", catalog_name()),
            ("routine_schema", str_value("pg_catalog")),
            ("routine_name", str_value(routine.name)),
            ("routine_type", routine_type(routine.kind)),
            (
                "routine_body",
                str_value(if routine.language() == i64::from(SQL_LANGUAGE) {
                    "SQL"
                } else {
                    "EXTERNAL"
                }),
            ),
            (
                "routine_definition",
                if builtin_owner {
                    str_value(routine.source)
                } else {
                    Value::Null
                },
            ),
            (
                "external_language",
                str_value(if routine.language() == i64::from(SQL_LANGUAGE) {
                    "SQL"
                } else {
                    "INTERNAL"
                }),
            ),
            (
                "is_deterministic",
                str_value(if routine.volatility == "i" {
                    "YES"
                } else {
                    "NO"
                }),
            ),
            ("sql_data_access", str_value("MODIFIES")),
            ("is_null_call", null_call(procedure, routine.strict)),
        ]
        .into_iter()
        .chain(result_columns(
            &types,
            procedure,
            routine.return_type,
            false,
        )?)));
    }
    // Registered operator functions return `text`, as their `pg_proc` rows declare.
    let registered_result = result_columns(&types, false, 25, false)?;
    rows.extend(registered_names().into_iter().map(|name| {
        row([
            ("specific_catalog", catalog_name()),
            ("specific_schema", str_value("pg_catalog")),
            ("specific_name", str_value(format!("{name}_0"))),
            ("routine_catalog", catalog_name()),
            ("routine_schema", str_value("pg_catalog")),
            ("routine_name", str_value(name)),
            ("routine_type", str_value("FUNCTION")),
            ("routine_body", str_value("EXTERNAL")),
            ("external_language", str_value("rust")),
            ("is_deterministic", str_value("NO")),
            ("sql_data_access", str_value("READS SQL DATA")),
            ("is_null_call", str_value("YES")),
        ]
        .into_iter()
        .chain(registered_result.iter().cloned()))
    }));
    for function in catalog.all_sql_functions() {
        let def = &function.def;
        let owner = bound_routine_owner(def)?;
        let owner_usage = catalog.role_is_enabled_for(current_user, &owner);
        if !owner_usage && !privileges.allowed(&owner, def.execute_acl.as_deref()) {
            continue;
        }
        let (routine_schema, routine_name) = split_schema_name(&def.name)?;
        let catalog_oid = user_routine_catalog_oid(&function)?;
        let (routine_body, external_language) = if def.language == "sql" {
            ("SQL", "SQL".to_string())
        } else {
            ("EXTERNAL", def.language.to_ascii_uppercase())
        };
        // `prosrc` of a SQL-standard body is empty; its parsed statements are `prosqlbody`.
        let definition = if owner_usage {
            match &def.body {
                uqa_sql::ast::FunctionBody::Source(source) => str_value(source.clone()),
                uqa_sql::ast::FunctionBody::Statements(_) => str_value(""),
            }
        } else {
            Value::Null
        };
        rows.push(row([
            ("specific_catalog", catalog_name()),
            ("specific_schema", str_value(routine_schema.clone())),
            (
                "specific_name",
                str_value(format!("{routine_name}_{catalog_oid}")),
            ),
            ("routine_catalog", catalog_name()),
            ("routine_schema", str_value(routine_schema)),
            ("routine_name", str_value(routine_name)),
            (
                "routine_type",
                routine_type(if def.is_procedure { "p" } else { "f" }),
            ),
            ("routine_body", str_value(routine_body)),
            ("routine_definition", definition),
            ("external_language", str_value(external_language)),
            (
                "is_deterministic",
                str_value(
                    if matches!(def.volatility, uqa_sql::ast::FunctionVolatility::Immutable) {
                        "YES"
                    } else {
                        "NO"
                    },
                ),
            ),
            ("sql_data_access", str_value("MODIFIES")),
            ("is_null_call", null_call(def.is_procedure, def.strict)),
        ]
        .into_iter()
        .chain(result_columns(
            &types,
            def.is_procedure,
            routine_result_type_oid(catalog, def),
            def.security.security_definer,
        )?)));
    }
    Ok(rows)
}

fn routine_type(kind: &str) -> Value {
    match kind {
        "f" => str_value("FUNCTION"),
        "p" => str_value("PROCEDURE"),
        _ => Value::Null,
    }
}

fn null_call(procedure: bool, strict: bool) -> Value {
    if procedure {
        Value::Null
    } else {
        str_value(if strict { "YES" } else { "NO" })
    }
}

/// The result type, descriptor and calling-convention columns. A procedure has no result type: the view's outer join to `pg_type` excludes it, and `dtd_identifier` is NULL.
fn result_columns(
    types: &AliasConstantOutput,
    procedure: bool,
    return_type: i64,
    security_definer: bool,
) -> Result<Vec<(&'static str, Value)>, SQLError> {
    let mut columns = Vec::with_capacity(8);
    if !procedure {
        if let Some((data_type, schema, name)) = types.routine_result_type(return_type)? {
            columns.extend([
                ("data_type", str_value(data_type)),
                ("type_udt_catalog", catalog_name()),
                ("type_udt_schema", str_value(schema)),
                ("type_udt_name", str_value(name)),
            ]);
        }
        columns.push(("dtd_identifier", str_value("0")));
    }
    columns.extend([
        ("parameter_style", str_value("GENERAL")),
        (
            "security_type",
            str_value(if security_definer {
                "DEFINER"
            } else {
                "INVOKER"
            }),
        ),
        ("as_locator", str_value("NO")),
    ]);
    Ok(columns)
}

/// `has_function_privilege(p.oid, 'EXECUTE')` for the session's current user.
struct ExecutePrivileges<'a> {
    catalog: &'a CatalogReadView,
    current_user: &'a RoleReference,
    superuser: bool,
}

impl<'a> ExecutePrivileges<'a> {
    fn new(catalog: &'a CatalogReadView, current_user: &'a RoleReference) -> Self {
        let superuser = current_user
            .role_definition(&catalog.snapshot().definitions.roles)
            .is_some_and(|role| role.attributes.contains(&RoleAttribute::Superuser));
        Self {
            catalog,
            current_user,
            superuser,
        }
    }

    fn allowed(&self, owner: &RoleIdentity, acl: Option<&[RoutineAclEntry]>) -> bool {
        routine_privilege_allowed(owner, acl, false, self.superuser, |role| {
            self.catalog.role_is_enabled_for(self.current_user, role)
        })
    }
}

#[cfg(test)]
mod tests;
