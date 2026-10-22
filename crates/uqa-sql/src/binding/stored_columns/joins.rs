//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::collections::BTreeSet;

use crate::ast::{Expr, FromClause};
use crate::SQLError;

use super::helpers::{preserve_table_column_name, unique_current_name};
use super::{
    same_identifier, ColumnBindingContext, ColumnBindingMode, ColumnScope, ScopeColumn,
    StoredColumnBinder,
};

impl StoredColumnBinder<'_> {
    pub(super) fn bind_join(
        &mut self,
        source: &mut FromClause,
        outer: &[ColumnScope],
        preceding: Option<&ColumnScope>,
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
        let left_scope = self.bind_from_in_scope(left, outer, preceding, false, context)?;
        let right_preceding = preceding.map_or_else(
            || left_scope.clone(),
            |preceding| ColumnScope::combined(preceding, &left_scope),
        );
        let right_scope =
            self.bind_from_in_scope(right, outer, Some(&right_preceding), *lateral, context)?;
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
                    reference: Expr::Column(current_name.into()),
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
}
