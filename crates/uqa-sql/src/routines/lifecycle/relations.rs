//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored relation, sequence OID, column, and namespace references in routine definitions.

use crate::{
    ast::{CreateFunction, Expr, FunctionBody},
    SQLError,
};
use std::collections::BTreeSet;
use uqa_core::Value;

pub trait RoutineRelationOids {
    fn bound_regclass_oid(&self, name: &str) -> Result<Option<i64>, SQLError>;
}

pub fn is_regclass(name: &str) -> bool {
    name.eq_ignore_ascii_case("regclass") || name.eq_ignore_ascii_case("pg_catalog.regclass")
}

pub fn regclass_oid(expression: &Expr) -> Option<i64> {
    match expression {
        Expr::TypedLiteral {
            value: Value::Int(oid),
            ty,
        } if is_regclass(ty) => Some(*oid),
        Expr::Cast { expr, ty, .. } if is_regclass(ty) => regclass_oid(expr),
        _ => None,
    }
}

pub fn stored_routine_references_relations(
    catalog: &dyn RoutineRelationOids,
    definition: &CreateFunction,
    relations: &BTreeSet<String>,
) -> Result<bool, SQLError> {
    if relations.is_empty() {
        return Ok(false);
    }
    let mut oids = BTreeSet::new();
    for relation in relations {
        if let Some(oid) = catalog.bound_regclass_oid(relation)? {
            oids.insert(oid);
        }
    }
    let mut depends = false;
    let mut visit = |expression: &mut crate::ast::Expr| {
        depends |= regclass_oid(expression).is_some_and(|oid| oids.contains(&oid));
        Ok(())
    };
    for default in definition
        .params
        .iter()
        .filter_map(|parameter| parameter.default.as_ref())
    {
        crate::catalog::stored_ast::visit_stored_expression(&mut default.clone(), &mut visit)?;
    }
    if let FunctionBody::Statements(statements) = &definition.body {
        for statement in statements {
            if crate::catalog::stored_ast::stored_statement_relation_names(statement)?
                .iter()
                .any(|name| relations.contains(name))
            {
                return Ok(true);
            }
            crate::catalog::stored_ast::visit_stored_statement_expressions(
                &mut statement.clone(),
                &mut visit,
            )?;
        }
    }
    Ok(depends)
}
