//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored relation, sequence OID, column, and namespace references in routine definitions.

use super::{RoutineDropResolution, RoutineDropTarget, RoutineRegistry};
use crate::{
    ast::{CreateFunction, Expr, FunctionBody},
    routines::routine_signature_types,
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
        Expr::Cast { expr, ty } if is_regclass(ty) => regclass_oid(expr),
        _ => None,
    }
}

pub fn stored_routine_references_columns(
    catalog: crate::binding::stored_columns::StoredColumnBindingContext<'_>,
    definition: &CreateFunction,
    columns: &BTreeSet<(String, String)>,
) -> Result<bool, SQLError> {
    let FunctionBody::Statements(statements) = &definition.body else {
        return Ok(false);
    };
    if columns.is_empty() {
        return Ok(false);
    }
    for statement in statements {
        let dependencies = crate::binding::stored_columns::stored_statement_column_dependencies(
            catalog, statement,
        )?;
        if dependencies.iter().any(|dependency| {
            columns.contains(&(
                dependency.relation.qualified_name(),
                dependency.column.clone(),
            ))
        }) {
            return Ok(true);
        }
    }
    Ok(false)
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

pub fn schema_routine_drop_targets(
    registry: &RoutineRegistry,
    schemas: &BTreeSet<String>,
) -> Result<RoutineDropResolution, SQLError> {
    let mut resolution = RoutineDropResolution {
        targets: Vec::new(),
        seen_targets: BTreeSet::new(),
        notices: Vec::new(),
    };
    for (name, overloads) in registry {
        for function in overloads {
            let identity =
                uqa_core::RelationIdentity::from_legacy_name(name).map_err(SQLError::Internal)?;
            let mut depends = schemas.contains(&identity.schema);
            if let crate::ast::FunctionBody::Statements(statements) = &function.def.body {
                for statement in statements {
                    for relation in
                        crate::catalog::stored_ast::stored_statement_relation_names(statement)?
                    {
                        let relation = uqa_core::RelationIdentity::from_legacy_name(&relation)
                            .map_err(SQLError::Internal)?;
                        depends |= schemas.contains(&relation.schema);
                    }
                }
            }
            if depends {
                let target = RoutineDropTarget {
                    object_id: function.def.object_id,
                    name: name.clone(),
                    argument_types: routine_signature_types(&function.def),
                    is_procedure: function.def.is_procedure,
                };
                resolution.seen_targets.insert(target.clone());
                resolution.targets.push(target);
            }
        }
    }
    Ok(resolution)
}
