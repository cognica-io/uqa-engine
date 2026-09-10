//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Domain dependency closure and removal during namespace deletion.

use std::collections::BTreeSet;

use uqa_planner::ScalarExpr;
use uqa_sql::ast::{
    ColumnDef, ColumnType, CreateFunction, Expr, FunctionBody, FunctionReturns, TableCheck,
};
use uqa_sql::SQLError;

use crate::{Engine, StorageBackendError};

fn storage_error(error: &StorageBackendError) -> SQLError {
    SQLError::Internal(format!("drop domain dependency: {error}"))
}

fn references_domain(ty: &ColumnType, targets: &BTreeSet<u32>) -> bool {
    match ty {
        ColumnType::Domain { oid, base, .. } => {
            targets.contains(oid) || references_domain(base, targets)
        }
        ColumnType::Array(element) => references_domain(element, targets),
        _ => false,
    }
}

#[derive(Default)]
struct DomainDependents {
    indexes: BTreeSet<crate::RelationIdentity>,
    columns: BTreeSet<(String, String, bool)>,
    defaults: BTreeSet<(String, String, bool)>,
    checks: BTreeSet<(String, String, bool)>,
}

impl Engine {
    pub(crate) fn domain_drop_column_names(
        &self,
        targets: &BTreeSet<u32>,
    ) -> Result<BTreeSet<(String, String)>, SQLError> {
        Ok(self
            .domain_drop_dependents(targets)?
            .columns
            .into_iter()
            .map(|(table, column, _)| (table, column))
            .collect())
    }

    pub(crate) fn domain_drop_has_dependents(
        &self,
        targets: &BTreeSet<u32>,
    ) -> Result<bool, SQLError> {
        let dependents = self.domain_drop_dependents(targets)?;
        if !dependents.indexes.is_empty()
            || !dependents.columns.is_empty()
            || !dependents.defaults.is_empty()
            || !dependents.checks.is_empty()
            || !self
                .domain_dependent_view_names(targets, &dependents)?
                .is_empty()
        {
            return Ok(true);
        }
        for domain in self
            .durable
            .domains
            .read()
            .clone()
            .values()
            .filter(|domain| !targets.contains(&domain.oid))
        {
            for check in &domain.definition.checks {
                if self.expression_references_domain(&check.expression, targets)? {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    fn type_name_references_domain(&self, name: &str, targets: &BTreeSet<u32>) -> bool {
        crate::sql::resolve_catalog_column_type(self, name)
            .is_some_and(|ty| references_domain(&ty, targets))
    }

    fn expression_references_domain(
        &self,
        expression: &Expr,
        targets: &BTreeSet<u32>,
    ) -> Result<bool, SQLError> {
        Ok(crate::events::stored_expression_type_names(expression)?
            .iter()
            .any(|name| self.type_name_references_domain(name, targets)))
    }

    pub(crate) fn routine_references_domain(
        &self,
        definition: &CreateFunction,
        targets: &BTreeSet<u32>,
    ) -> Result<bool, SQLError> {
        for param in &definition.params {
            if self.type_name_references_domain(&param.type_name, targets) {
                return Ok(true);
            }
            if let Some(default) = &param.default {
                if self.expression_references_domain(default, targets)? {
                    return Ok(true);
                }
            }
        }
        if let FunctionReturns::Scalar { type_name } | FunctionReturns::SetOf { type_name } =
            &definition.returns
        {
            if self.type_name_references_domain(type_name, targets) {
                return Ok(true);
            }
        }
        if let FunctionBody::Statements(statements) = &definition.body {
            for statement in statements {
                let mut merge_assignment_depends = false;
                crate::events::visit_stored_statement_merges(
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
                if crate::events::stored_statement_type_names(statement)?
                    .iter()
                    .any(|name| self.type_name_references_domain(name, targets))
                {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    pub(crate) fn expand_domain_drop_targets(
        &self,
        targets: &mut BTreeSet<u32>,
        routines: &[uqa_sql::ast::FunctionBinding],
    ) -> Result<(), SQLError> {
        let registry = self.durable.domains.read().clone();
        loop {
            let previous = targets.len();
            for domain in registry.values() {
                let mut depends = references_domain(&domain.definition.base, targets);
                if let Some(default) = &domain.definition.default {
                    depends |= self.expression_references_domain(default, targets)?;
                    for routine in routines {
                        depends |= crate::events::expression_references_routine_identity(
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

    pub(crate) fn domain_checks_depending_on_routines(
        &self,
        routines: &[uqa_sql::ast::FunctionBinding],
    ) -> Result<Vec<(String, String)>, SQLError> {
        let mut checks = BTreeSet::new();
        for (name, domain) in self.durable.domains.read().clone() {
            for check in &domain.definition.checks {
                for routine in routines {
                    if crate::events::expression_references_routine_identity(
                        &check.expression,
                        routine,
                    )? {
                        checks.insert((
                            name.clone(),
                            check.name.clone().ok_or_else(|| {
                                SQLError::Internal("domain CHECK has no name".into())
                            })?,
                        ));
                    }
                }
            }
        }
        Ok(checks.into_iter().collect())
    }

    pub(crate) fn drop_domain_routine_checks(
        &self,
        routines: &[uqa_sql::ast::FunctionBinding],
    ) -> Result<(), SQLError> {
        let checks = self.domain_checks_depending_on_routines(routines)?;
        if checks.is_empty() {
            return Ok(());
        }
        let mut registry = self.durable.domains.read().clone();
        for (domain, name) in checks {
            let domain = registry
                .get_mut(&domain)
                .ok_or_else(|| SQLError::Internal("dependent domain disappeared".into()))?;
            domain
                .definition
                .checks
                .retain(|check| check.name.as_deref() != Some(&name));
        }
        self.persist_domains(&registry)?;
        *self.durable.domains.write() = registry;
        self.note_catalog_registry_changed();
        Ok(())
    }

    pub(crate) fn commit_domain_drop(&self, targets: &BTreeSet<u32>) -> Result<(), SQLError> {
        if targets.is_empty() {
            return Ok(());
        }
        let mut registry = self.durable.domains.read().clone();
        let dependents = self.domain_drop_dependents(targets)?;
        self.drop_domain_view_dependents(targets, &dependents)?;
        self.drop_domain_schema_dependents(&dependents)?;
        registry.retain(|_, domain| !targets.contains(&domain.oid));
        for domain in registry.values_mut() {
            if let Some(default) = &domain.definition.default {
                if self.expression_references_domain(default, targets)? {
                    domain.definition.default = None;
                }
            }
            let mut checks = Vec::new();
            for check in &domain.definition.checks {
                if !self.expression_references_domain(&check.expression, targets)? {
                    checks.push(check.clone());
                }
            }
            domain.definition.checks = checks;
        }
        self.persist_domains(&registry)?;
        *self.durable.domains.write() = registry;
        self.note_catalog_registry_changed();
        Ok(())
    }

    fn domain_drop_dependents(
        &self,
        targets: &BTreeSet<u32>,
    ) -> Result<DomainDependents, SQLError> {
        let mut dependents = DomainDependents {
            indexes: self.domain_dependent_indexes(targets)?,
            ..DomainDependents::default()
        };
        for (table, state) in self.table_entries() {
            self.domain_schema_dependents(
                &table,
                &state.columns.read(),
                &state.table_checks.read(),
                false,
                targets,
                &mut dependents,
            )?;
        }
        for (identity, table) in self.durable.foreign_tables.read().clone() {
            self.domain_schema_dependents(
                &identity.qualified_name(),
                &table.columns,
                &table.checks,
                true,
                targets,
                &mut dependents,
            )?;
        }
        Ok(dependents)
    }

    pub(crate) fn domain_drop_view_names(
        &self,
        targets: &BTreeSet<u32>,
    ) -> Result<Vec<String>, SQLError> {
        if targets.is_empty() {
            return Ok(Vec::new());
        }
        self.domain_dependent_view_names(targets, &self.domain_drop_dependents(targets)?)
    }

    fn domain_schema_dependents(
        &self,
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
                drop_column |= self.expression_references_domain(&generated.expression, targets)?;
            }
            if drop_column {
                dependents.columns.insert(target.clone());
            }
            if let Some(default) = &column.default {
                if self.expression_references_domain(default, targets)? {
                    dependents.defaults.insert(target);
                }
            }
            if let Some(check) = &column.check {
                if self.expression_references_domain(check, targets)? {
                    let name = column.check_name.clone().ok_or_else(|| {
                        SQLError::Internal("domain dependent CHECK has no name".into())
                    })?;
                    dependents.checks.insert((table.to_string(), name, foreign));
                }
            }
        }
        for check in checks {
            if self.expression_references_domain(&check.expr, targets)? {
                let name = check.name.clone().ok_or_else(|| {
                    SQLError::Internal("domain dependent CHECK has no name".into())
                })?;
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
                    if crate::table_storage::schema_expr_references_column(
                        &generated.expression,
                        removed,
                    ) {
                        dependents.columns.insert((
                            table.to_string(),
                            column.name.clone(),
                            foreign,
                        ));
                    }
                }
            }
            if previous == dependents.columns.len() {
                break;
            }
        }
        Ok(())
    }

    fn domain_dependent_view_names(
        &self,
        targets: &BTreeSet<u32>,
        dependents: &DomainDependents,
    ) -> Result<Vec<String>, SQLError> {
        let mut views = BTreeSet::new();
        for (identity, mut view) in self.durable.views.read().clone() {
            let mut depends = false;
            view.query.rewrite_scalar_expressions(&mut |expression| {
                if let ScalarExpr::Cast { ty, .. } | ScalarExpr::TypedLiteral { ty, .. } =
                    expression
                {
                    depends |= self.type_name_references_domain(ty, targets);
                }
            });
            if depends {
                views.insert(identity.qualified_name());
            }
        }
        for (table, column, _) in &dependents.columns {
            views.extend(
                self.views_depending_on_column(table, column)
                    .map_err(|error| storage_error(&error))?,
            );
        }
        self.cascade_view_closure(views.into_iter().collect())
    }

    fn drop_domain_view_dependents(
        &self,
        targets: &BTreeSet<u32>,
        dependents: &DomainDependents,
    ) -> Result<(), SQLError> {
        let closure = self.domain_dependent_view_names(targets, dependents)?;
        self.drop_rules_depending_on_relations_inner(&closure)
            .map_err(|error| storage_error(&error))?;
        self.drop_views_inner(&closure, false)
    }

    fn drop_domain_schema_dependents(&self, dependents: &DomainDependents) -> Result<(), SQLError> {
        for index in &dependents.indexes {
            crate::sql::drop_index_dependency(self, index)?;
        }
        let mut tables = BTreeSet::new();
        tables.extend(dependents.columns.iter().map(|(table, _, _)| table));
        tables.extend(dependents.defaults.iter().map(|(table, _, _)| table));
        tables.extend(dependents.checks.iter().map(|(table, _, _)| table));
        for table in tables {
            self.lock_relation(table, crate::row_locks::RelationLockMode::AccessExclusive)?;
            self.ensure_no_pending_trigger_events(table, "ALTER TABLE")?;
        }
        for (table, constraint, foreign) in &dependents.checks {
            if *foreign {
                self.drop_foreign_table_check_dependency(table, constraint)
                    .map_err(|error| storage_error(&error))?;
            } else {
                crate::sql::drop_constraint_dependency(self, table, constraint)?;
            }
        }
        for (table, column, foreign) in &dependents.defaults {
            if *foreign {
                self.clear_foreign_table_default_dependency(table, column)
                    .map_err(|error| storage_error(&error))?;
            } else {
                self.set_column_default_inner(table, column, None)
                    .map_err(|error| storage_error(&error))?;
            }
        }
        for (table, column, foreign) in &dependents.columns {
            if *foreign {
                self.drop_foreign_table_column_dependency(table, column)
                    .map_err(|error| storage_error(&error))?;
            } else {
                crate::sql::drop_column_cascade(self, table, column, true)?;
            }
        }
        Ok(())
    }

    fn domain_dependent_indexes(
        &self,
        targets: &BTreeSet<u32>,
    ) -> Result<BTreeSet<crate::RelationIdentity>, SQLError> {
        let mut indexes = BTreeSet::new();
        for row in self.durable.catalog_indexes.read().clone().values() {
            let definition = crate::catalog_indexes::index_definition(row)
                .map_err(|error| storage_error(&error))?;
            let keys: Vec<uqa_sql::ast::IndexKey> = serde_json::from_str(&row.columns_json)
                .map_err(|error| SQLError::Internal(error.to_string()))?;
            let mut depends = definition
                .key_types
                .iter()
                .any(|ty| references_domain(ty, targets));
            for expression in keys
                .iter()
                .filter_map(|key| match key {
                    uqa_sql::ast::IndexKey::Expression(expression) => Some(expression.as_ref()),
                    uqa_sql::ast::IndexKey::Column(_) => None,
                })
                .chain(definition.predicate.as_deref())
            {
                depends |= self.expression_references_domain(expression, targets)?;
            }
            if depends {
                indexes.insert(row.relation.clone());
            }
        }
        Ok(indexes)
    }
}
