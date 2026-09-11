//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compile an anonymous procedural block against the caller's current catalog.
use crate::{
    ast::{CreateFunction, FunctionReturns},
    plpgsql::PLpgSQLFunction,
    routines::{compilation::RoutineParserCatalog, declaration::RoutineTypeCatalog},
    SQLError,
};
pub fn compile_do_block(
    types: &dyn RoutineTypeCatalog,
    parsers: &dyn RoutineParserCatalog,
    language: &str,
    body: &str,
) -> Result<(CreateFunction, PLpgSQLFunction), SQLError> {
    if language != "plpgsql" {
        return Err(SQLError::Routine {
            sqlstate: "42704".into(),
            message: format!("language \"{language}\" does not exist"),
        });
    }
    let catalog = parsers.plpgsql_catalog()?;
    let mut parsed = crate::plpgsql::parse_do_block_with_catalog(body, &catalog)?;
    crate::routines::declaration::resolve_plpgsql_datum_types(types, &mut parsed)?;
    let def = CreateFunction {
        object_id: None,
        name: "inline_code_block".into(),
        or_replace: false,
        is_procedure: false,
        params: Vec::new(),
        returns: FunctionReturns::Scalar {
            type_name: "void".into(),
        },
        return_type_reference: None,
        language: "plpgsql".into(),
        body: crate::ast::FunctionBody::Source(body.to_string()),
        creation_search_path: Vec::new(),
        volatility: crate::ast::FunctionVolatility::Volatile,
        strict: false,
        owner: String::new(),
        security: crate::ast::RoutineSecurityAttributes::default(),
        parallel: crate::ast::FunctionParallel::Unsafe,
        support: None,
        config: Vec::new(),
        config_actions: Vec::new(),
        execute_acl: None,
    };
    Ok((def, parsed))
}
