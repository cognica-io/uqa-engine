//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored statement column identities, durable source shapes, and rewrite-rule binding.

mod expressions;
mod helpers;
mod lifecycle;
mod sources;
mod statements;

pub use lifecycle::{
    bind_rule_action_column_dependencies, bind_rule_condition_column_dependencies,
    bind_stored_statement_source_columns, remove_rule_source_column_aliases,
    remove_stored_statement_source_column_aliases, rewrite_rule_column_references,
    stored_statement_column_dependencies,
};
pub use statements::rewrite_stored_statement_column;

/// Current names of a bound row relation, resolved when a binding pass needs them.
pub trait StoredColumnCatalog {
    fn stored_relation_column_names(&self, name: &str) -> Result<Option<Vec<String>>, SQLError>;
}

/// Catalog and namespace snapshots captured before resolving stored source columns.
pub struct StoredSourceColumns {
    pub catalog: crate::catalog::analysis::CatalogReadView,
    pub resolution: crate::catalog::resolution::RelationNameResolution,
}
pub trait StoredSourceCatalog {
    fn stored_source_columns(&self) -> StoredSourceColumns;
}
impl StoredSourceColumns {
    pub fn bind_statement(&self, statement: &mut Statement) -> Result<bool, SQLError> {
        bind_stored_statement_source_columns(statement, |name| {
            Ok(
                if let Some(table) = self.catalog.table_resolved(&self.resolution, name)? {
                    Some(
                        table
                            .columns
                            .iter()
                            .map(|column| column.name.clone())
                            .collect(),
                    )
                } else {
                    self.catalog
                        .foreign_table_resolved(&self.resolution, name)?
                        .map(|table| {
                            table
                                .columns
                                .iter()
                                .map(|column| column.name.clone())
                                .collect()
                        })
                },
            )
        })
    }
}

#[derive(Clone, Copy)]
pub struct StoredColumnBindingContext<'a> {
    pub sources: &'a dyn StoredColumnCatalog,
    pub merge: &'a dyn crate::routines::merge_columns::StoredMergeColumnCatalog,
}

use std::collections::{BTreeMap, BTreeSet};

use crate::ast::{
    DeleteStmt, Expr, FromClause, InsertStmt, OnConflictAction, Projection, SelectStmt, Statement,
    UpdateStmt, CTE,
};
use crate::SQLError;

use uqa_core::RelationIdentity;

use crate::catalog::events::RuleColumnDependency;
use helpers::{
    action_returning_scope, apply_positional_aliases, is_default_values_insert, is_output_alias,
    opaque_scope, preserve_table_column_name, same_identifier, select_output_names,
    table_alias_count_error, unique_current_name,
};

fn cte_output_names(body: &crate::ast::CteBody) -> Vec<String> {
    if let Some(query) = body.query() {
        return select_output_names(query);
    }
    helpers::projection_output_names(body.returning().unwrap_or_default())
}

#[derive(Clone, Copy)]
enum ColumnBindingMode<'a> {
    Bind,
    Rename {
        relation: &'a RelationIdentity,
        from: &'a str,
        to: &'a str,
    },
    Drop {
        dependencies: &'a BTreeSet<RuleColumnDependency>,
    },
}

impl<'a> ColumnBindingMode<'a> {
    fn column_names(self, relation: &RelationIdentity, current: &str) -> (String, String) {
        match self {
            Self::Rename {
                relation: target,
                from,
                to,
            } if relation == target && current == to => (from.to_string(), to.to_string()),
            Self::Bind | Self::Rename { .. } | Self::Drop { .. } => {
                (current.to_string(), current.to_string())
            }
        }
    }

    const fn is_rename(self) -> bool {
        matches!(self, Self::Rename { .. })
    }

    const fn dropped_dependencies(self) -> Option<&'a BTreeSet<RuleColumnDependency>> {
        match self {
            Self::Drop { dependencies } => Some(dependencies),
            Self::Bind | Self::Rename { .. } => None,
        }
    }
}

#[derive(Clone)]
struct ScopeColumn {
    /// Name present in the stored definition before this binding pass.
    name: String,
    /// Name present in the live catalog after this binding pass.
    current_name: String,
    /// Reference that identifies this output in the enclosing query scope.
    reference: Expr,
    dependencies: BTreeSet<RuleColumnDependency>,
}

#[derive(Clone, Default)]
struct ColumnScope {
    output: Vec<ScopeColumn>,
    qualifiers: BTreeMap<String, Vec<ScopeColumn>>,
}

impl ColumnScope {
    fn insert_qualifier(&mut self, qualifier: &str, columns: &[ScopeColumn]) {
        self.qualifiers
            .insert(qualifier.to_ascii_lowercase(), columns.to_vec());
    }

    fn qualified(&self, qualifier: &str) -> Option<&[ScopeColumn]> {
        self.qualifiers
            .get(&qualifier.to_ascii_lowercase())
            .map(Vec::as_slice)
    }

    fn unqualified(&self, name: &str) -> Vec<&ScopeColumn> {
        self.output
            .iter()
            .filter(|column| same_identifier(&column.name, name))
            .collect()
    }

    fn combined(left: &Self, right: &Self) -> Self {
        let mut output = left.output.clone();
        output.extend(right.output.iter().cloned());
        let mut qualifiers = left.qualifiers.clone();
        qualifiers.extend(right.qualifiers.clone());
        Self { output, qualifiers }
    }
}

#[derive(Clone, Default)]
struct ColumnBindingContext {
    ctes: BTreeMap<String, Vec<String>>,
}

struct StoredColumnBinder<'a> {
    catalog: StoredColumnBindingContext<'a>,
    mode: ColumnBindingMode<'a>,
    dependencies: BTreeSet<RuleColumnDependency>,
    alias_shape_changed: bool,
}

impl<'a> StoredColumnBinder<'a> {
    fn new(catalog: StoredColumnBindingContext<'a>, mode: ColumnBindingMode<'a>) -> Self {
        Self {
            catalog,
            mode,
            dependencies: BTreeSet::new(),
            alias_shape_changed: false,
        }
    }

    fn finish(self) -> BTreeSet<RuleColumnDependency> {
        self.dependencies
    }

    const fn alias_shape_changed(&self) -> bool {
        self.alias_shape_changed
    }

    fn remove_dropped_column_aliases(
        &mut self,
        column_aliases: &mut Vec<String>,
        scope: &ColumnScope,
    ) {
        let Some(dependencies) = self.mode.dropped_dependencies() else {
            return;
        };
        let positions = scope
            .output
            .iter()
            .enumerate()
            .filter_map(|(position, column)| {
                (!column.dependencies.is_disjoint(dependencies)).then_some(position)
            })
            .collect::<Vec<_>>();
        for position in positions.into_iter().rev() {
            if position < column_aliases.len() {
                column_aliases.remove(position);
                self.alias_shape_changed = true;
            }
        }
    }

    fn table_scope(
        &self,
        name: &str,
        qualifier: &str,
        alias: Option<&str>,
        column_aliases: &[String],
        bound_columns: Option<&[String]>,
        context: &ColumnBindingContext,
    ) -> Result<ColumnScope, SQLError> {
        if let Some(columns) = context.ctes.get(&name.to_ascii_lowercase()) {
            if column_aliases.len() > columns.len() {
                return Err(table_alias_count_error(
                    alias.unwrap_or(qualifier),
                    columns.len(),
                    column_aliases.len(),
                ));
            }
            let mut columns = columns.clone();
            apply_positional_aliases(&mut columns, column_aliases);
            return Ok(opaque_scope(&columns, Some(alias.unwrap_or(qualifier))));
        }
        let columns = match bound_columns {
            Some(columns) => columns.to_vec(),
            None => self
                .catalog
                .sources
                .stored_relation_column_names(name)?
                .ok_or_else(|| SQLError::UnknownTable(name.to_string()))?,
        };
        if column_aliases.len() > columns.len() {
            return Err(table_alias_count_error(
                alias.unwrap_or(qualifier),
                columns.len(),
                column_aliases.len(),
            ));
        }
        let relation = RelationIdentity::from_legacy_name(name).map_err(|error| {
            SQLError::Internal(format!("decode bound rule relation `{name}`: {error}"))
        })?;
        let visible_qualifier = alias.unwrap_or(qualifier);
        let output = columns
            .into_iter()
            .enumerate()
            .map(|(position, column)| {
                let (stored_physical, current_physical) =
                    self.mode.column_names(&relation, &column);
                let name = column_aliases
                    .get(position)
                    .cloned()
                    .unwrap_or_else(|| stored_physical.clone());
                let current_name = column_aliases
                    .get(position)
                    .cloned()
                    .unwrap_or(current_physical);
                ScopeColumn {
                    reference: Expr::qualified_column(visible_qualifier, &current_name),
                    dependencies: BTreeSet::from([RuleColumnDependency {
                        relation: relation.clone(),
                        column: stored_physical,
                    }]),
                    name,
                    current_name,
                }
            })
            .collect::<Vec<_>>();
        let mut scope = ColumnScope {
            output: output.clone(),
            ..ColumnScope::default()
        };
        if let Some(alias) = alias {
            scope.insert_qualifier(alias, &output);
        } else {
            scope.insert_qualifier(qualifier, &output);
            scope.insert_qualifier(name, &output);
            if let Some((_, local)) = name.rsplit_once('.') {
                scope.insert_qualifier(local.trim_matches('"'), &output);
            }
        }
        Ok(scope)
    }

    fn bind_statement(
        &mut self,
        statement: &mut Statement,
        outer: &[ColumnScope],
        context: &ColumnBindingContext,
    ) -> Result<(), SQLError> {
        match statement {
            Statement::Select(select) => self.bind_select(select, outer, context),
            Statement::Insert(insert) => self.bind_insert(insert, outer, context),
            Statement::Update(update) => self.bind_update(update, outer, context),
            Statement::Delete(delete) => self.bind_delete(delete, outer, context),
            Statement::Merge(merge) => self.bind_merge(merge, outer, context),
            Statement::Notify { .. } => Ok(()),
            _ => Err(SQLError::Internal(
                "validated rewrite-rule action has an unsupported statement kind".into(),
            )),
        }
    }

    fn bind_insert(
        &mut self,
        insert: &mut InsertStmt,
        outer: &[ColumnScope],
        context: &ColumnBindingContext,
    ) -> Result<(), SQLError> {
        let context = self.bind_ctes(&mut insert.with, outer, context)?;
        for expression in insert.rows.iter_mut().flatten() {
            self.bind_expr(expression, outer, &context)?;
        }
        if let Some(select) = insert.select_source.as_deref_mut() {
            self.bind_select(select, outer, &context)?;
        }
        let target = self.table_scope(
            &insert.table,
            &insert.target_qualifier,
            Some(&insert.target_qualifier),
            &[],
            None,
            &ColumnBindingContext::default(),
        )?;
        if insert.columns.is_empty() && !is_default_values_insert(&insert.rows) {
            let width = insert.rows.first().map(Vec::len).or_else(|| {
                insert
                    .select_source
                    .as_deref()
                    .map(select_output_names)
                    .map(|columns| columns.len())
            });
            if let Some(width) = width {
                insert.columns = target
                    .output
                    .iter()
                    .take(width)
                    .map(|column| column.current_name.clone())
                    .collect();
            }
        }
        self.bind_target_names(&mut insert.columns, &target);
        let mut conflict_scope = target.clone();
        conflict_scope.insert_qualifier("excluded", &target.output);
        let mut target_scopes = vec![conflict_scope];
        target_scopes.extend_from_slice(outer);
        if let Some(conflict) = &mut insert.on_conflict {
            self.bind_target_names(&mut conflict.conflict_columns, &target);
            for expression in &mut conflict.expressions {
                self.bind_expr(expression, &target_scopes, &context)?;
            }
            if let Some(predicate) = conflict.predicate.as_deref_mut() {
                self.bind_expr(predicate, &target_scopes, &context)?;
            }
            if let OnConflictAction::Update {
                assignments,
                r#where,
            } = &mut conflict.action
            {
                for (column, expression) in assignments {
                    self.bind_target_name(column, &target);
                    self.bind_expr(expression, &target_scopes, &context)?;
                }
                if let Some(expression) = r#where {
                    self.bind_expr(expression, &target_scopes, &context)?;
                }
            }
        }
        let returning = action_returning_scope(
            &target,
            &target,
            crate::ast::RuleEvent::Insert,
            &insert.returning_aliases,
        );
        let mut returning_scopes = vec![returning.clone()];
        returning_scopes.extend_from_slice(outer);
        self.bind_projections(
            &mut insert.returning,
            Some(&returning),
            &returning_scopes,
            &context,
        )
    }

    fn bind_update(
        &mut self,
        update: &mut UpdateStmt,
        outer: &[ColumnScope],
        context: &ColumnBindingContext,
    ) -> Result<(), SQLError> {
        let context = self.bind_ctes(&mut update.with, outer, context)?;
        let target = self.table_scope(
            &update.table,
            &update.target_qualifier,
            Some(&update.target_qualifier),
            &[],
            None,
            &ColumnBindingContext::default(),
        )?;
        let (local, scopes) =
            self.bind_dml_source(update.from.as_mut(), &target, outer, &context)?;
        for (column, expression) in &mut update.assignments {
            self.bind_target_name(column, &target);
            self.bind_expr(expression, &scopes, &context)?;
        }
        if let Some(expression) = &mut update.r#where {
            self.bind_expr(expression, &scopes, &context)?;
        }
        let returning = action_returning_scope(
            &local,
            &target,
            crate::ast::RuleEvent::Update,
            &update.returning_aliases,
        );
        let mut returning_scopes = vec![returning.clone()];
        returning_scopes.extend_from_slice(outer);
        self.bind_projections(
            &mut update.returning,
            Some(&returning),
            &returning_scopes,
            &context,
        )
    }

    fn bind_delete(
        &mut self,
        delete: &mut DeleteStmt,
        outer: &[ColumnScope],
        context: &ColumnBindingContext,
    ) -> Result<(), SQLError> {
        let context = self.bind_ctes(&mut delete.with, outer, context)?;
        let target = self.table_scope(
            &delete.table,
            &delete.target_qualifier,
            Some(&delete.target_qualifier),
            &[],
            None,
            &ColumnBindingContext::default(),
        )?;
        let (local, scopes) =
            self.bind_dml_source(delete.using.as_mut(), &target, outer, &context)?;
        if let Some(expression) = &mut delete.r#where {
            self.bind_expr(expression, &scopes, &context)?;
        }
        let returning = action_returning_scope(
            &local,
            &target,
            crate::ast::RuleEvent::Delete,
            &delete.returning_aliases,
        );
        let mut returning_scopes = vec![returning.clone()];
        returning_scopes.extend_from_slice(outer);
        self.bind_projections(
            &mut delete.returning,
            Some(&returning),
            &returning_scopes,
            &context,
        )
    }

    fn bind_dml_source(
        &mut self,
        source: Option<&mut FromClause>,
        target: &ColumnScope,
        outer: &[ColumnScope],
        context: &ColumnBindingContext,
    ) -> Result<(ColumnScope, Vec<ColumnScope>), SQLError> {
        let mut source_outer = vec![target.clone()];
        source_outer.extend_from_slice(outer);
        let source = source
            .map(|source| self.bind_from(source, &source_outer, context))
            .transpose()?;
        let local = source.as_ref().map_or_else(
            || target.clone(),
            |source| ColumnScope::combined(target, source),
        );
        let mut scopes = vec![local.clone()];
        scopes.extend_from_slice(outer);
        Ok((local, scopes))
    }

    fn bind_target_names(&mut self, names: &mut [String], target: &ColumnScope) {
        for name in names {
            self.bind_target_name(name, target);
        }
    }

    fn bind_target_name(&mut self, name: &mut String, target: &ColumnScope) {
        let Some(column) = target
            .output
            .iter()
            .find(|column| same_identifier(&column.name, name))
        else {
            return;
        };
        self.dependencies
            .extend(column.dependencies.iter().cloned());
        name.clone_from(&column.current_name);
    }

    fn bind_ctes(
        &mut self,
        ctes: &mut [CTE],
        outer: &[ColumnScope],
        context: &ColumnBindingContext,
    ) -> Result<ColumnBindingContext, SQLError> {
        let mut visible = context.clone();
        let recursive_names = ctes
            .iter()
            .filter(|cte| cte.recursive)
            .map(|cte| {
                let columns = if cte.columns.is_empty() {
                    cte_output_names(&cte.body)
                } else {
                    cte.columns.clone()
                };
                (cte.name.to_ascii_lowercase(), columns)
            })
            .collect::<Vec<_>>();
        for (name, columns) in recursive_names {
            visible.ctes.entry(name).or_insert(columns);
        }
        for cte in ctes {
            let mut statement = cte.body.clone().into_statement();
            self.bind_statement(&mut statement, outer, &visible)?;
            cte.body = statement.try_into()?;
            if let Some(cycle) = &mut cte.cycle {
                self.bind_expr(&mut cycle.mark_value, outer, &visible)?;
                self.bind_expr(&mut cycle.mark_default, outer, &visible)?;
            }
            let mut columns = cte_output_names(&cte.body);
            apply_positional_aliases(&mut columns, &cte.columns);
            if let Some(search) = &cte.search {
                columns.push(search.sequence_column.clone());
            }
            if let Some(cycle) = &cte.cycle {
                columns.push(cycle.mark_column.clone());
                columns.push(cycle.path_column.clone());
            }
            visible.ctes.insert(cte.name.to_ascii_lowercase(), columns);
        }
        Ok(visible)
    }

    fn bind_select(
        &mut self,
        select: &mut SelectStmt,
        outer: &[ColumnScope],
        context: &ColumnBindingContext,
    ) -> Result<(), SQLError> {
        let context = self.bind_ctes(&mut select.with, outer, context)?;
        let local = select
            .from
            .as_mut()
            .map(|source| self.bind_from(source, outer, &context))
            .transpose()?;
        let mut scopes = local.iter().cloned().collect::<Vec<_>>();
        scopes.extend_from_slice(outer);
        self.bind_projections(&mut select.projections, local.as_ref(), &scopes, &context)?;
        for expression in select.values.iter_mut().flatten() {
            self.bind_expr(expression, &scopes, &context)?;
        }
        if let Some(expression) = &mut select.r#where {
            self.bind_expr(expression, &scopes, &context)?;
        }
        for expression in &mut select.group_by {
            self.bind_expr(expression, &scopes, &context)?;
        }
        for expression in select.grouping_sets.iter_mut().flatten() {
            self.bind_expr(expression, &scopes, &context)?;
        }
        if let Some(expression) = &mut select.having {
            self.bind_expr(expression, &scopes, &context)?;
        }
        let output_names = select_output_names(select);
        for order in &mut select.order_by {
            if !is_output_alias(&order.expr, &output_names) {
                self.bind_expr(&mut order.expr, &scopes, &context)?;
            }
        }
        if let Some(expression) = &mut select.limit {
            self.bind_expr(expression, &scopes, &context)?;
        }
        if let Some(expression) = &mut select.offset {
            self.bind_expr(expression, &scopes, &context)?;
        }
        for expression in &mut select.distinct_on {
            self.bind_expr(expression, &scopes, &context)?;
        }
        if let Some(set) = &mut select.set_op {
            if let Some(left) = &mut set.left {
                self.bind_select(left, outer, &context)?;
            }
            self.bind_select(&mut set.right, outer, &context)?;
            let set_output = set
                .left
                .as_deref()
                .map_or_else(|| output_names.clone(), select_output_names);
            for order in &mut set.combined_order_by {
                if !is_output_alias(&order.expr, &set_output) {
                    self.bind_expr(&mut order.expr, outer, &context)?;
                }
            }
            if let Some(expression) = &mut set.combined_limit {
                self.bind_expr(expression, outer, &context)?;
            }
            if let Some(expression) = &mut set.combined_offset {
                self.bind_expr(expression, outer, &context)?;
            }
        }
        Ok(())
    }

    fn bind_from(
        &mut self,
        source: &mut FromClause,
        outer: &[ColumnScope],
        context: &ColumnBindingContext,
    ) -> Result<ColumnScope, SQLError> {
        match source {
            source @ FromClause::Table { .. } => self.bind_table_source(source, context),
            source @ FromClause::Join { .. } => self.bind_join(source, outer, context),
            FromClause::Values {
                rows,
                alias,
                column_aliases,
                ..
            } => {
                for expression in rows.iter_mut().flatten() {
                    self.bind_expr(expression, outer, context)?;
                }
                let mut columns = if column_aliases.is_empty() {
                    (1..=rows.first().map_or(0, Vec::len))
                        .map(|position| format!("column{position}"))
                        .collect::<Vec<_>>()
                } else {
                    column_aliases.clone()
                };
                apply_positional_aliases(&mut columns, column_aliases);
                Ok(opaque_scope(&columns, alias.as_deref()))
            }
            FromClause::Function {
                output_name,
                args,
                alias,
                column_aliases,
                ordinality,
                ..
            } => {
                for expression in args {
                    self.bind_expr(expression, outer, context)?;
                }
                let mut columns = vec![output_name.clone()];
                apply_positional_aliases(&mut columns, column_aliases);
                if *ordinality {
                    columns.push("ordinality".into());
                }
                Ok(opaque_scope(
                    &columns,
                    Some(alias.as_deref().unwrap_or(output_name)),
                ))
            }
            FromClause::FunctionGroup {
                functions,
                alias,
                column_aliases,
                ordinality,
            } => {
                for function in functions.iter_mut() {
                    for expression in &mut function.args {
                        self.bind_expr(expression, outer, context)?;
                    }
                }
                let mut columns = functions
                    .iter()
                    .flat_map(|function| {
                        if function.column_aliases.is_empty() {
                            vec![function.output_name.clone()]
                        } else {
                            function.column_aliases.clone()
                        }
                    })
                    .collect::<Vec<_>>();
                apply_positional_aliases(&mut columns, column_aliases);
                if *ordinality {
                    columns.push("ordinality".into());
                }
                Ok(opaque_scope(&columns, alias.as_deref()))
            }
            FromClause::Subquery {
                body,
                alias,
                column_aliases,
            } => {
                self.bind_select(body, outer, context)?;
                let mut columns = select_output_names(body);
                apply_positional_aliases(&mut columns, column_aliases);
                Ok(opaque_scope(&columns, alias.as_deref()))
            }
        }
    }

    fn bind_join(
        &mut self,
        source: &mut FromClause,
        outer: &[ColumnScope],
        context: &ColumnBindingContext,
    ) -> Result<ColumnScope, SQLError> {
        let FromClause::Join {
            left,
            right,
            on,
            using,
            natural,
            alias,
            column_aliases,
            lateral,
            ..
        } = source
        else {
            unreachable!("join column binding requires a join source")
        };
        if let ColumnBindingMode::Rename { relation, from, to } = self.mode {
            if *natural
                || using.as_ref().is_some_and(|using| {
                    using
                        .columns
                        .iter()
                        .any(|column| same_identifier(column, from))
                })
            {
                preserve_table_column_name(self.catalog, left, relation, from, to)?;
                preserve_table_column_name(self.catalog, right, relation, from, to)?;
            }
        }
        let left_scope = self.bind_from(left, outer, context)?;
        let mut right_outer = Vec::new();
        if *lateral {
            right_outer.push(left_scope.clone());
        }
        right_outer.extend_from_slice(outer);
        let right_scope = self.bind_from(right, &right_outer, context)?;
        let input_scope = ColumnScope::combined(&left_scope, &right_scope);
        let mut on_scopes = vec![input_scope];
        on_scopes.extend_from_slice(outer);
        if let Some(expression) = on {
            self.bind_expr(expression, &on_scopes, context)?;
        }
        if *natural {
            let columns = left_scope
                .output
                .iter()
                .filter(|left| {
                    right_scope
                        .output
                        .iter()
                        .any(|right| same_identifier(&left.name, &right.name))
                })
                .map(|column| column.name.clone())
                .collect();
            *using = Some(crate::ast::JoinUsing {
                columns,
                alias: None,
            });
            *natural = false;
        }
        let scope = self.join_scope(
            left_scope,
            right_scope,
            using.as_mut(),
            alias.as_deref(),
            column_aliases,
        )?;
        self.remove_dropped_column_aliases(column_aliases, &scope);
        Ok(scope)
    }

    fn join_scope(
        &mut self,
        left: ColumnScope,
        right: ColumnScope,
        using: Option<&mut crate::ast::JoinUsing>,
        alias: Option<&str>,
        column_aliases: &[String],
    ) -> Result<ColumnScope, SQLError> {
        let using_alias = using.as_ref().and_then(|using| using.alias.clone());
        let mut merged = Vec::new();
        if let Some(using) = using {
            for name in &mut using.columns {
                let left_matches = left.unqualified(name);
                let right_matches = right.unqualified(name);
                for column in left_matches.iter().chain(&right_matches) {
                    self.dependencies
                        .extend(column.dependencies.iter().cloned());
                }
                let left_current = unique_current_name(&left_matches);
                let right_current = unique_current_name(&right_matches);
                if left_current.is_some()
                    && right_current.is_some()
                    && left_current != right_current
                {
                    return Err(SQLError::Internal(format!(
                        "rule JOIN USING column \"{name}\" resolved to different visible names after column rebinding"
                    )));
                }
                let current_name = left_current
                    .or(right_current)
                    .unwrap_or_else(|| name.clone());
                let mut dependencies = BTreeSet::new();
                for column in left_matches.iter().chain(&right_matches) {
                    dependencies.extend(column.dependencies.iter().cloned());
                }
                let stored_name = name.clone();
                name.clone_from(&current_name);
                merged.push(ScopeColumn {
                    name: stored_name,
                    current_name: current_name.clone(),
                    reference: Expr::Column(current_name),
                    dependencies,
                });
            }
        }
        let merged_names = merged
            .iter()
            .map(|column| column.name.clone())
            .collect::<Vec<_>>();
        let mut output = merged;
        output.extend(
            left.output
                .iter()
                .chain(&right.output)
                .filter(|column| {
                    !merged_names
                        .iter()
                        .any(|name| same_identifier(name, &column.name))
                })
                .cloned(),
        );
        if let Some(alias) = alias {
            for (position, column) in output.iter_mut().enumerate() {
                if let Some(name) = column_aliases.get(position) {
                    column.name.clone_from(name);
                    column.current_name.clone_from(name);
                }
                column.reference = Expr::qualified_column(alias, &column.current_name);
            }
            let mut scope = ColumnScope {
                output: output.clone(),
                ..ColumnScope::default()
            };
            scope.insert_qualifier(alias, &output);
            return Ok(scope);
        }
        let mut qualifiers = left.qualifiers;
        qualifiers.extend(right.qualifiers);
        let mut scope = ColumnScope { output, qualifiers };
        if let Some(using_alias) = using_alias.as_deref() {
            let merged = scope
                .output
                .iter()
                .filter(|column| {
                    merged_names
                        .iter()
                        .any(|name| same_identifier(name, &column.name))
                })
                .cloned()
                .collect::<Vec<_>>();
            scope.insert_qualifier(using_alias, &merged);
        }
        Ok(scope)
    }

    fn bind_projections(
        &mut self,
        projections: &mut Vec<Projection>,
        local: Option<&ColumnScope>,
        scopes: &[ColumnScope],
        context: &ColumnBindingContext,
    ) -> Result<(), SQLError> {
        let mut bound = Vec::with_capacity(projections.len());
        for mut projection in projections.drain(..) {
            let expanded = match &projection.expr {
                Expr::Star => local.map(|scope| scope.output.as_slice()),
                Expr::QualifiedStar(qualifier) => {
                    scopes.iter().find_map(|scope| scope.qualified(qualifier))
                }
                _ => None,
            };
            if let Some(columns) = expanded {
                for column in columns {
                    self.dependencies
                        .extend(column.dependencies.iter().cloned());
                    bound.push(Projection {
                        expr: column.reference.clone(),
                        alias: Some(column.name.clone()),
                    });
                }
                continue;
            }
            let implicit_name = match &projection.expr {
                Expr::Column(name) | Expr::QualifiedColumn { column: name, .. } => {
                    Some(name.clone())
                }
                _ => None,
            };
            self.bind_expr(&mut projection.expr, scopes, context)?;
            if projection.alias.is_none() && self.mode.is_rename() {
                let current_name = match &projection.expr {
                    Expr::Column(name) | Expr::QualifiedColumn { column: name, .. } => Some(name),
                    _ => None,
                };
                if implicit_name
                    .as_ref()
                    .zip(current_name)
                    .is_some_and(|(stored, current)| !same_identifier(stored, current))
                {
                    projection.alias = implicit_name;
                }
            }
            bound.push(projection);
        }
        *projections = bound;
        Ok(())
    }
}
