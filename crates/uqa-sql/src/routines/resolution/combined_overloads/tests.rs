//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::routines::{
    declaration::RoutineTypeCatalog,
    resolution::{RoutineCallKind, RoutineOverloadCatalog, RoutineTypeSnapshot},
    CompiledFunctionBody, RoutineBody, SQLUserFunction,
};
use std::sync::Arc;

struct Catalog {
    routines: Vec<Arc<SQLUserFunction>>,
    search_path: Vec<String>,
}

impl Catalog {
    fn new(declarations: &[&str], search_path: &[&str]) -> Self {
        let routines = declarations
            .iter()
            .map(|sql| {
                let crate::Statement::CreateFunction(def) = crate::compile(sql).unwrap().remove(0)
                else {
                    unreachable!()
                };
                Arc::new(SQLUserFunction::new(
                    *def,
                    RoutineBody::Bound(Arc::new(CompiledFunctionBody::SQL(Vec::new()))),
                ))
            })
            .collect();
        Self {
            routines,
            search_path: search_path.iter().map(|name| (*name).into()).collect(),
        }
    }
}

impl RoutineTypeCatalog for Catalog {
    fn try_describe_table(&self, _: &str) -> Result<Option<Vec<crate::ast::ColumnDef>>, String> {
        Ok(None)
    }

    fn resolve_catalog_column_type(&self, name: &str) -> Option<ColumnType> {
        ColumnType::from_sql_name(name).ok()
    }

    fn resolve_catalog_column_type_name(&self, name: &str) -> Result<ColumnType, SQLError> {
        ColumnType::from_sql_name(name)
    }

    fn resolve_catalog_user_type_by_oid(&self, _: u32) -> Option<ColumnType> {
        None
    }

    fn require_type_usage(&self, _: &ColumnType) -> Result<(), SQLError> {
        Ok(())
    }

    fn format_type(&self, ty: &ColumnType) -> Result<String, SQLError> {
        Ok(ty.regtype_name())
    }
}

impl RoutineOverloadCatalog for Catalog {
    fn routine_type_snapshot(&self) -> RoutineTypeSnapshot {
        Arc::default()
    }

    fn routine_search_path(&self) -> Vec<String> {
        self.search_path.clone()
    }

    fn has_registered_scalar_function(&self, _: &str) -> bool {
        false
    }

    fn lookup_sql_routine_candidates(
        &self,
        _: &str,
    ) -> Result<Option<Vec<Arc<SQLUserFunction>>>, SQLError> {
        Ok(Some(self.routines.clone()))
    }

    fn lookup_bound_sql_routine_candidates_by_binding(
        &self,
        _: &FunctionBinding,
    ) -> Option<Vec<Arc<SQLUserFunction>>> {
        Some(self.routines.clone())
    }

    fn lookup_bound_sql_functions_by_binding(
        &self,
        binding: &FunctionBinding,
    ) -> Option<Vec<Arc<SQLUserFunction>>> {
        self.lookup_bound_sql_routine_candidates_by_binding(binding)
    }
}

fn md5() -> BuiltinFunctionOverload {
    BuiltinFunctionOverload {
        variadic: false,
        name: "pg_catalog.md5".into(),
        argument_names: vec![None],
        argument_types: vec![ColumnType::Text],
        default_arguments: 0,
        return_type: ColumnType::Text,
    }
}

#[test]
fn typed_variadic_builtins_preserve_catalog_precedence_and_same_schema_fixed_priority() {
    let builtins = [BuiltinFunctionOverload {
        variadic: true,
        name: "pg_catalog.jsonb_extract_path".into(),
        argument_names: vec![Some("from_json".into()), Some("path_elems".into())],
        argument_types: vec![
            ColumnType::JsonB,
            ColumnType::Array(Box::new(ColumnType::Text)),
        ],
        default_arguments: 0,
        return_type: ColumnType::JsonB,
    }];
    for (schema, path, builtin) in [
        ("pg_catalog", vec!["pg_catalog", "public"], false),
        ("app", vec!["app", "pg_catalog"], false),
        ("app", vec!["pg_catalog", "app"], true),
    ] {
        let declaration = format!("CREATE FUNCTION {schema}.jsonb_extract_path(jsonb,text) RETURNS text LANGUAGE SQL RETURN 'fixed'");
        let catalog = Catalog::new(&[&declaration], &path);
        let resolver = RoutineOverloadContext { catalog: &catalog };
        let selected = resolve(
            &resolver,
            "jsonb_extract_path",
            None,
            &[None, None],
            &[Some(ColumnType::JsonB), Some(ColumnType::Text)],
            false,
            &builtins,
        )
        .unwrap();
        assert_eq!(selected.binding.builtin, builtin, "{schema} {path:?}");
        assert_eq!(
            selected.return_type,
            if builtin {
                ColumnType::JsonB
            } else {
                ColumnType::Text
            }
        );
        let selected = resolve(
            &resolver,
            "jsonb_extract_path",
            None,
            &[None, None],
            &[
                Some(ColumnType::JsonB),
                Some(ColumnType::Array(Box::new(ColumnType::Text))),
            ],
            true,
            &builtins,
        )
        .unwrap();
        assert!(selected.binding.builtin);
        assert_eq!(selected.binding.argument_types, ["jsonb", "text[]"]);
        assert!(matches!(
            selected.binding.invocation.unwrap().variadic_mode,
            crate::ast::RoutineVariadicMode::Explicit { parameter_index: 1 }
        ));
    }
}

fn assert_procedure_error(error: SQLError, signature: &str) {
    let SQLError::Diagnostic {
        sqlstate,
        message,
        detail,
        hint,
    } = error
    else {
        panic!("expected a PostgreSQL procedure diagnostic, got {error:?}");
    };
    assert_eq!(sqlstate, "42809");
    assert_eq!(message, format!("{signature} is a procedure"));
    assert_eq!(detail, None);
    assert_eq!(hint.as_deref(), Some("To call a procedure, use CALL."));
}

// PostgreSQL 18.4 independently returns these signatures, SQLSTATEs and hints.
#[test]
fn procedure_shadowing_precedes_scalar_and_table_call_kind_validation() {
    let catalog = Catalog::new(
        &["CREATE PROCEDURE app.md5(value text) LANGUAGE plpgsql AS $$ BEGIN NULL; END $$"],
        &["app", "pg_catalog"],
    );
    let resolver = RoutineOverloadContext { catalog: &catalog };
    let builtins = [md5()];
    let types = [Some(ColumnType::Text)];
    for context in [ResolutionContext::Scalar, ResolutionContext::Table] {
        for (names, signature) in [
            ([None], "md5(text)"),
            ([Some("value".into())], "md5(value => text)"),
        ] {
            let request = ResolutionRequest {
                resolver: &resolver,
                name: "md5",
                argument_names: &names,
                argument_types: &types,
                explicit_variadic: false,
                builtins: &builtins,
                context,
            };
            assert_procedure_error(resolve_in_context(&request, None).unwrap_err(), signature);
        }
    }
    assert_procedure_error(
        resolver
            .resolve_static_sql_function("app.md5", None, &[Some("value".into())], &types, false)
            .err()
            .unwrap(),
        "app.md5(value => text)",
    );
}

#[test]
fn catalog_precedence_and_retained_builtin_bindings_survive_procedure_candidates() {
    let mut catalog = Catalog::new(
        &["CREATE PROCEDURE app.md5(value text) LANGUAGE plpgsql AS $$ BEGIN NULL; END $$"],
        &["pg_catalog", "app"],
    );
    let builtin = resolve(
        &RoutineOverloadContext { catalog: &catalog },
        "md5",
        None,
        &[None],
        &[Some(ColumnType::Text)],
        false,
        &[md5()],
    )
    .unwrap();
    assert!(builtin.binding.builtin);
    for path in [vec!["app".into(), "pg_catalog".into()], vec!["app".into()]] {
        catalog.search_path = path;
        let resolver = RoutineOverloadContext { catalog: &catalog };
        let retained = resolve(
            &resolver,
            "md5",
            Some(&builtin.binding),
            &[None],
            &[Some(ColumnType::Text)],
            false,
            &[md5()],
        )
        .unwrap();
        assert_eq!(retained.binding, builtin.binding);
        if catalog.search_path.len() == 1 {
            assert!(
                resolve(
                    &resolver,
                    "md5",
                    None,
                    &[None],
                    &[Some(ColumnType::Text)],
                    false,
                    &[md5()],
                )
                .unwrap()
                .binding
                .builtin
            );
        }
    }
}

#[test]
fn overload_ranking_and_ambiguity_include_procedures_before_rejecting_the_winner() {
    let catalog = Catalog::new(
        &[
            "CREATE FUNCTION app.pick(value integer) RETURNS integer LANGUAGE SQL AS 'SELECT 7'",
            "CREATE PROCEDURE app.pick(value bigint) LANGUAGE plpgsql AS $$ BEGIN NULL; END $$",
        ],
        &["app", "pg_catalog"],
    );
    let resolver = RoutineOverloadContext { catalog: &catalog };
    let selected = resolve(
        &resolver,
        "pick",
        None,
        &[None],
        &[Some(ColumnType::Integer)],
        false,
        &[],
    )
    .unwrap();
    assert_eq!(selected.return_type, ColumnType::Integer);
    assert_procedure_error(
        resolve(
            &resolver,
            "pick",
            None,
            &[None],
            &[Some(ColumnType::BigInteger)],
            false,
            &[],
        )
        .unwrap_err(),
        "pick(bigint)",
    );
    let ambiguous = resolve(&resolver, "pick", None, &[None], &[None], false, &[]).unwrap_err();
    assert_eq!(ambiguous.sqlstate(), Some("42725"));
    assert_eq!(
        ambiguous.to_string(),
        "function pick(unknown) is not unique"
    );
    for (names, signature) in [
        ([None], "pick(integer)"),
        ([Some("value".into())], "pick(value => integer)"),
    ] {
        let error = resolver
            .resolve_static_sql_routine_match(
                "pick",
                None,
                &names,
                &[Some(ColumnType::Integer)],
                false,
                RoutineCallKind::Procedure,
            )
            .err()
            .unwrap();
        let SQLError::Diagnostic {
            sqlstate,
            message,
            detail,
            hint,
        } = error
        else {
            panic!("expected a PostgreSQL function diagnostic, got {error:?}");
        };
        assert_eq!(sqlstate, "42809");
        assert_eq!(message, format!("{signature} is not a procedure"));
        assert_eq!(detail, None);
        assert_eq!(hint.as_deref(), Some("To call a function, use SELECT."));
    }
}

#[test]
fn concrete_sql_overload_wins_over_polymorphic_builtin_without_false_exact_matches() {
    let catalog = Catalog::new(
        &["CREATE FUNCTION app.mode(value integer) RETURNS integer LANGUAGE SQL AS $$SELECT value$$"],
        &["pg_catalog", "app"],
    );
    let builtins = [BuiltinFunctionOverload {
        variadic: false,
        name: "pg_catalog.mode".into(),
        argument_names: vec![None],
        argument_types: vec![ColumnType::Named("anyelement".into())],
        default_arguments: 0,
        return_type: ColumnType::Named("anyelement".into()),
    }];
    let resolver = RoutineOverloadContext { catalog: &catalog };
    let selected = resolve(
        &resolver,
        "mode",
        None,
        &[None],
        &[Some(ColumnType::Integer)],
        false,
        &builtins,
    )
    .unwrap();
    assert!(!selected.binding.builtin);
    assert_eq!(selected.binding.name, "app.mode");
    let selected = resolve(
        &resolver,
        "mode",
        None,
        &[None],
        &[Some(ColumnType::Text)],
        false,
        &builtins,
    )
    .unwrap();
    assert!(selected.binding.builtin);
    assert_eq!(selected.exact_matches, 0);
    assert_eq!(selected.binding.argument_types, vec!["anyelement"]);
}
