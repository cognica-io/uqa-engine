//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Domain dependency analysis over declared types, stored syntax, and immutable catalog definitions.

use crate::{
    ast::{
        ColumnDef, ColumnType, CreateFunction, Expr, FunctionBody, FunctionReturns, IndexKey,
        TableCheck,
    },
    catalog::{domain::StoredDomain, index::IndexDefinition, stored_view::StoredView},
    ir::ScalarExpr,
    SQLError,
};
use std::collections::{BTreeMap, BTreeSet};
use uqa_core::RelationIdentity;

pub trait DomainTypeCatalog {
    fn resolve_domain_type_reference(&self, name: &str) -> Option<ColumnType>;
}

#[derive(Default)]
pub struct DomainDependents {
    pub indexes: BTreeSet<RelationIdentity>,
    pub columns: BTreeSet<(String, String, bool)>,
    pub defaults: BTreeSet<(String, String, bool)>,
    pub checks: BTreeSet<(String, String, bool)>,
}

pub fn references_domain(ty: &ColumnType, targets: &BTreeSet<u32>) -> bool {
    match ty {
        ColumnType::Domain { oid, base, .. } => {
            targets.contains(oid) || references_domain(base, targets)
        }
        ColumnType::Array(element) => references_domain(element, targets),
        _ => false,
    }
}

pub fn type_name_references_domain(
    types: &dyn DomainTypeCatalog,
    name: &str,
    targets: &BTreeSet<u32>,
) -> bool {
    types
        .resolve_domain_type_reference(name)
        .is_some_and(|ty| references_domain(&ty, targets))
}

pub fn expression_references_domain(
    types: &dyn DomainTypeCatalog,
    expression: &Expr,
    targets: &BTreeSet<u32>,
) -> Result<bool, SQLError> {
    Ok(
        crate::catalog::stored_ast::stored_expression_type_names(expression)?
            .iter()
            .any(|name| type_name_references_domain(types, name, targets)),
    )
}

pub fn routine_references_domain(
    types: &dyn DomainTypeCatalog,
    definition: &CreateFunction,
    targets: &BTreeSet<u32>,
) -> Result<bool, SQLError> {
    for param in &definition.params {
        if type_name_references_domain(types, &param.type_name, targets) {
            return Ok(true);
        }
        if let Some(default) = &param.default {
            if expression_references_domain(types, default, targets)? {
                return Ok(true);
            }
        }
    }
    if let FunctionReturns::Scalar { type_name } | FunctionReturns::SetOf { type_name } =
        &definition.returns
    {
        if type_name_references_domain(types, type_name, targets) {
            return Ok(true);
        }
    }
    if let FunctionBody::Statements(statements) = &definition.body {
        for statement in statements {
            let mut merge_assignment_depends = false;
            crate::catalog::stored_ast::visit_stored_statement_merges(
                &mut statement.clone(),
                &mut |merge| {
                    merge_assignment_depends |= merge
                        .target_column_bindings
                        .values()
                        .any(|binding| !binding.domain_dependencies.is_disjoint(targets));
                    Ok(())
                },
            )?;
            if merge_assignment_depends {
                return Ok(true);
            }
            if crate::catalog::stored_ast::stored_statement_type_names(statement)?
                .iter()
                .any(|name| type_name_references_domain(types, name, targets))
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

pub fn expand_domain_drop_targets(
    types: &dyn DomainTypeCatalog,
    registry: &BTreeMap<String, StoredDomain>,
    targets: &mut BTreeSet<u32>,
    routines: &[crate::ast::FunctionBinding],
) -> Result<(), SQLError> {
    loop {
        let previous = targets.len();
        for domain in registry.values() {
            let mut depends = references_domain(&domain.definition.base, targets);
            if let Some(default) = &domain.definition.default {
                depends |= expression_references_domain(types, default, targets)?;
                for routine in routines {
                    depends |= crate::catalog::stored_ast::expression_references_routine_identity(
                        default, routine,
                    )?;
                }
            }
            if depends {
                targets.insert(domain.oid);
            }
        }
        if previous == targets.len() {
            break;
        }
    }
    Ok(())
}

pub fn domain_checks_depending_on_routines(
    registry: BTreeMap<String, StoredDomain>,
    routines: &[crate::ast::FunctionBinding],
) -> Result<Vec<(String, String)>, SQLError> {
    let mut checks = BTreeSet::new();
    for (name, domain) in registry {
        for check in &domain.definition.checks {
            for routine in routines {
                if crate::catalog::stored_ast::expression_references_routine_identity(
                    &check.expression,
                    routine,
                )? {
                    checks.insert((
                        name.clone(),
                        check
                            .name
                            .clone()
                            .ok_or_else(|| SQLError::Internal("domain CHECK has no name".into()))?,
                    ));
                }
            }
        }
    }
    Ok(checks.into_iter().collect())
}

pub fn domain_schema_dependents(
    types: &dyn DomainTypeCatalog,
    table: &str,
    columns: &[ColumnDef],
    checks: &[TableCheck],
    foreign: bool,
    targets: &BTreeSet<u32>,
    dependents: &mut DomainDependents,
) -> Result<(), SQLError> {
    for column in columns {
        let target = (table.to_string(), column.name.clone(), foreign);
        let mut drop_column = references_domain(&column.ty, targets);
        if let Some(generated) = &column.generated {
            drop_column |= expression_references_domain(types, &generated.expression, targets)?;
        }
        if drop_column {
            dependents.columns.insert(target.clone());
        }
        if let Some(default) = &column.default {
            if expression_references_domain(types, default, targets)? {
                dependents.defaults.insert(target);
            }
        }
        if let Some(check) = &column.check {
            if expression_references_domain(types, check, targets)? {
                let name = column.check_name.clone().ok_or_else(|| {
                    SQLError::Internal("domain dependent CHECK has no name".into())
                })?;
                dependents.checks.insert((table.to_string(), name, foreign));
            }
        }
    }
    for check in checks {
        if expression_references_domain(types, &check.expr, targets)? {
            let name = check
                .name
                .clone()
                .ok_or_else(|| SQLError::Internal("domain dependent CHECK has no name".into()))?;
            dependents.checks.insert((table.to_string(), name, foreign));
        }
    }
    loop {
        let previous = dependents.columns.len();
        let removed = dependents
            .columns
            .iter()
            .filter(|(name, _, _)| name == table)
            .map(|(_, column, _)| column.clone())
            .collect::<Vec<_>>();
        for column in columns {
            let Some(generated) = &column.generated else {
                continue;
            };
            for removed in &removed {
                if crate::schema::dependencies::schema_expr_references_column(
                    &generated.expression,
                    removed,
                ) {
                    dependents
                        .columns
                        .insert((table.to_string(), column.name.clone(), foreign));
                }
            }
        }
        if previous == dependents.columns.len() {
            break;
        }
    }
    Ok(())
}

pub fn views_referencing_domains(
    types: &dyn DomainTypeCatalog,
    registry: BTreeMap<RelationIdentity, StoredView>,
    targets: &BTreeSet<u32>,
) -> BTreeSet<String> {
    let mut views = BTreeSet::new();
    for (identity, mut view) in registry {
        let mut depends = false;
        view.query.rewrite_scalar_expressions(&mut |expression| {
            if let ScalarExpr::Cast { ty, .. } | ScalarExpr::TypedLiteral { ty, .. } = expression {
                depends |= type_name_references_domain(types, ty, targets);
            }
        });
        if depends {
            views.insert(identity.qualified_name());
        }
    }
    views
}

pub fn remove_domain_references(
    types: &dyn DomainTypeCatalog,
    registry: &mut BTreeMap<String, StoredDomain>,
    targets: &BTreeSet<u32>,
) -> Result<(), SQLError> {
    registry.retain(|_, domain| !targets.contains(&domain.oid));
    for domain in registry.values_mut() {
        if let Some(default) = &domain.definition.default {
            if expression_references_domain(types, default, targets)? {
                domain.definition.default = None;
            }
        }
        let mut checks = Vec::new();
        for check in &domain.definition.checks {
            if !expression_references_domain(types, &check.expression, targets)? {
                checks.push(check.clone());
            }
        }
        domain.definition.checks = checks;
    }
    Ok(())
}

pub fn remove_domain_routine_checks(
    registry: &mut BTreeMap<String, StoredDomain>,
    checks: Vec<(String, String)>,
) -> Result<(), SQLError> {
    for (domain, name) in checks {
        let domain = registry
            .get_mut(&domain)
            .ok_or_else(|| SQLError::Internal("dependent domain disappeared".into()))?;
        domain
            .definition
            .checks
            .retain(|check| check.name.as_deref() != Some(&name));
    }
    Ok(())
}

pub fn parse_domain_index_keys(keys: &str) -> Result<Vec<IndexKey>, SQLError> {
    serde_json::from_str(keys).map_err(|error| SQLError::Internal(error.to_string()))
}

pub fn index_references_domain(
    types: &dyn DomainTypeCatalog,
    definition: &IndexDefinition,
    keys: &[IndexKey],
    targets: &BTreeSet<u32>,
) -> Result<bool, SQLError> {
    let mut depends = definition
        .key_types
        .iter()
        .any(|ty| references_domain(ty, targets));
    for expression in keys
        .iter()
        .filter_map(|key| match key {
            IndexKey::Expression(expression) => Some(expression.as_ref()),
            IndexKey::Column(_) => None,
        })
        .chain(definition.predicate.as_deref())
    {
        depends |= expression_references_domain(types, expression, targets)?;
    }
    Ok(depends)
}
