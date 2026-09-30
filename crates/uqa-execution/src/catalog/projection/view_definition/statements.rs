//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `INSERT`, `UPDATE`, `DELETE` and `MERGE` statements of `BEGIN ATOMIC` bodies, as `get_insert_query_def`, `get_update_query_def`, `get_delete_query_def` and `get_merge_query_def` print them. Every statement has the target relation in its range table, so routine parameters print qualified and column references carry their relation's name.

use std::fmt::Write as _;

use uqa_sql::ast::{AssignmentStep, AssignmentTarget, ReturningAliases};
use uqa_sql::ir::ScalarExpr;
use uqa_sql::plan::{
    AssignmentPlan, CommandPlan, ConflictActionPlan, ConflictPlan, CtePlan, DeletePlan, InsertPlan,
    MergePlan, MergeWhenPlan, ProjectionPlan, QueryPlan, SourcePlan, UnifiedPlan, UpdatePlan,
};

use super::{quote_ident, Column, Deparser, RelationIdentity, SQLError, Scope};

/// The relation a data-modifying statement writes.
struct Target<'a> {
    table: &'a str,
    alias: Option<&'a str>,
    include_descendants: bool,
}

impl Deparser<'_> {
    /// One statement of a `BEGIN ATOMIC` body, as `get_query_def` prints it.
    pub fn statement(&self, plan: &UnifiedPlan, scope: &Scope) -> Result<String, SQLError> {
        match plan {
            UnifiedPlan::Query(query) => self.query(query, scope, None),
            UnifiedPlan::Command(command) => match command.as_ref() {
                CommandPlan::Insert(insert) => self.insert(insert, scope),
                CommandPlan::Update(update) => self.update(update, scope),
                CommandPlan::Delete(delete) => self.delete(delete, scope),
                CommandPlan::Merge(merge) => self.merge(merge, scope),
                other => Err(SQLError::Internal(format!(
                    "a routine body holds a {} statement, which SQL-standard bodies cannot contain",
                    other.name()
                ))),
            },
        }
    }

    /// `get_insert_query_def`.
    fn insert(&self, insert: &InsertPlan, parent: &Scope) -> Result<String, SQLError> {
        let (mut rendered, scope) = self.statement_scope(&insert.ctes, parent)?;
        let target = Target {
            table: &insert.table,
            alias: insert.target_alias.as_deref(),
            include_descendants: true,
        };
        let columns = self.target_columns(&target, &scope)?;
        rendered.push_str(if self.indent {
            " INSERT INTO "
        } else {
            "INSERT INTO "
        });
        rendered.push_str(&self.relation_name(target.table, &Scope::default())?);
        if let Some(alias) = target.alias {
            write!(rendered, " AS {}", quote_ident(alias))
                .expect("writing to a String cannot fail");
        }
        // The column list names the target columns the statement supplies, which are the leading columns when none were written.
        let width = match (&insert.source, insert.rows.first()) {
            (Some(source), _) => super::query_columns(source).len(),
            (None, Some(row)) => row.len(),
            (None, None) => 0,
        };
        let supplied = if insert.columns.is_empty() {
            columns
                .iter()
                .take(width)
                .map(|column| quote_ident(&column.name))
                .collect::<Vec<_>>()
        } else {
            insert
                .columns
                .iter()
                .map(|target| self.assignment_target(target, &scope, &insert.subqueries))
                .collect::<Result<Vec<_>, _>>()?
        };
        if !supplied.is_empty() {
            write!(rendered, " ({})", supplied.join(", "))
                .expect("writing to a String cannot fail");
        }
        match (&insert.source, insert.rows.as_slice()) {
            (Some(source), _) => {
                rendered.push(' ');
                rendered.push_str(&self.query(source, &scope.child(), None)?);
            }
            (None, [row]) if row.is_empty() => rendered.push_str(" DEFAULT VALUES"),
            (None, [row]) => {
                let values = self.expressions(row, &scope, &insert.subqueries)?;
                self.clause(
                    &mut rendered,
                    "  VALUES (",
                    &format!("{values})"),
                    scope.indent,
                );
            }
            (None, rows) => {
                write!(
                    rendered,
                    " VALUES {}",
                    self.values(rows, &scope, &insert.subqueries)?
                )
                .expect("writing to a String cannot fail");
            }
        }
        let mut target_scope = scope.clone();
        target_scope.columns = columns;
        if let Some(conflict) = &insert.on_conflict {
            self.on_conflict(&mut rendered, conflict, &target_scope, &insert.subqueries)?;
        }
        self.returning(
            &mut rendered,
            (&insert.returning, &insert.returning_aliases),
            &target_scope,
            &insert.subqueries,
        )?;
        Ok(rendered)
    }

    /// The `ON CONFLICT` clause of `get_insert_query_def`. An arbiter can only name the target relation, so its columns print without a relation name.
    fn on_conflict(
        &self,
        rendered: &mut String,
        conflict: &ConflictPlan,
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<(), SQLError> {
        rendered.push_str(" ON CONFLICT");
        let mut arbiter_scope = scope.clone();
        arbiter_scope.qualify = false;
        if !conflict.expressions.is_empty() || !conflict.conflict_columns.is_empty() {
            let elements = if conflict.expressions.is_empty() {
                conflict
                    .conflict_columns
                    .iter()
                    .map(|column| quote_ident(column))
                    .collect::<Vec<_>>()
            } else {
                conflict
                    .expressions
                    .iter()
                    .map(|expression| {
                        let rendered = self.expression(expression, &arbiter_scope, subqueries)?;
                        Ok(match expression {
                            ScalarExpr::Column(_)
                            | ScalarExpr::QualifiedColumn { .. }
                            | ScalarExpr::Func { .. } => rendered,
                            _ => format!("({rendered})"),
                        })
                    })
                    .collect::<Result<Vec<_>, SQLError>>()?
            };
            write!(rendered, "({})", elements.join(", ")).expect("writing to a String cannot fail");
            if let Some(predicate) = &conflict.predicate {
                let predicate = self.expression(predicate, &arbiter_scope, subqueries)?;
                self.clause(rendered, "  WHERE ", &predicate, scope.indent);
            }
        } else if let Some(constraint) = &conflict.constraint {
            write!(rendered, " ON CONSTRAINT {}", quote_ident(constraint))
                .expect("writing to a String cannot fail");
        }
        match &conflict.action {
            ConflictActionPlan::Nothing => rendered.push_str(" DO NOTHING"),
            ConflictActionPlan::Update {
                assignments,
                predicate,
            } => {
                let mut update_scope = scope.clone();
                update_scope.columns.extend(
                    scope
                        .columns
                        .iter()
                        .map(|column| Column {
                            qualifier: "excluded".into(),
                            rendered_qualifier: "excluded".into(),
                            merged: None,
                            merged_expression: None,
                            ..column.clone()
                        })
                        .collect::<Vec<_>>(),
                );
                rendered.push_str(" DO UPDATE SET ");
                rendered.push_str(&self.assignments(assignments, &update_scope, subqueries)?);
                if let Some(predicate) = predicate {
                    let predicate = self.expression(predicate, &update_scope, subqueries)?;
                    self.clause(rendered, "  WHERE ", &predicate, scope.indent);
                }
            }
        }
        Ok(())
    }

    /// `get_update_query_def`.
    fn update(&self, update: &UpdatePlan, parent: &Scope) -> Result<String, SQLError> {
        let (mut rendered, mut scope) = self.statement_scope(&update.ctes, parent)?;
        let target = Target {
            table: &update.table,
            alias: update.target_alias.as_deref(),
            include_descendants: update.include_descendants,
        };
        scope.columns = self.target_columns(&target, &scope)?;
        if let Some(source) = &update.source {
            let columns = self.source_columns(source, &scope)?;
            scope.columns.extend(columns);
        }
        rendered.push_str(if self.indent { " UPDATE " } else { "UPDATE " });
        rendered.push_str(&self.target_name(&target)?);
        rendered.push_str(" SET ");
        rendered.push_str(&self.assignments(&update.assignments, &scope, &update.subqueries)?);
        if let Some(source) = &update.source {
            let source = self.source(source, &scope, &update.subqueries)?;
            self.clause(&mut rendered, "   FROM ", &source, scope.indent);
        }
        if let Some(predicate) = &update.predicate {
            let predicate = self.expression(predicate, &scope, &update.subqueries)?;
            self.clause(&mut rendered, "  WHERE ", &predicate, scope.indent);
        }
        self.returning(
            &mut rendered,
            (&update.returning, &update.returning_aliases),
            &scope,
            &update.subqueries,
        )?;
        Ok(rendered)
    }

    /// `get_delete_query_def`.
    fn delete(&self, delete: &DeletePlan, parent: &Scope) -> Result<String, SQLError> {
        let (mut rendered, mut scope) = self.statement_scope(&delete.ctes, parent)?;
        let target = Target {
            table: &delete.table,
            alias: delete.target_alias.as_deref(),
            include_descendants: delete.include_descendants,
        };
        scope.columns = self.target_columns(&target, &scope)?;
        if let Some(source) = &delete.source {
            let columns = self.source_columns(source, &scope)?;
            scope.columns.extend(columns);
        }
        rendered.push_str(if self.indent {
            " DELETE FROM "
        } else {
            "DELETE FROM "
        });
        rendered.push_str(&self.target_name(&target)?);
        if let Some(source) = &delete.source {
            let source = self.source(source, &scope, &delete.subqueries)?;
            self.clause(&mut rendered, "   USING ", &source, scope.indent);
        }
        if let Some(predicate) = &delete.predicate {
            let predicate = self.expression(predicate, &scope, &delete.subqueries)?;
            self.clause(&mut rendered, "  WHERE ", &predicate, scope.indent);
        }
        self.returning(
            &mut rendered,
            (&delete.returning, &delete.returning_aliases),
            &scope,
            &delete.subqueries,
        )?;
        Ok(rendered)
    }

    /// `get_merge_query_def`. `NOT MATCHED` actions say `BY TARGET` once any action is `NOT MATCHED BY SOURCE`.
    fn merge(&self, merge: &MergePlan, parent: &Scope) -> Result<String, SQLError> {
        let (mut rendered, mut scope) = self.statement_scope(&merge.ctes, parent)?;
        let target = Target {
            table: &merge.target,
            alias: merge.target_alias.as_deref(),
            include_descendants: merge.include_descendants,
        };
        scope.columns = self.target_columns(&target, &scope)?;
        let source_columns = self.source_columns(&merge.source, &scope)?;
        scope.columns.extend(source_columns);
        rendered.push_str(if self.indent {
            " MERGE INTO "
        } else {
            "MERGE INTO "
        });
        rendered.push_str(&self.target_name(&target)?);
        let source = self.source(&merge.source, &scope, &merge.subqueries)?;
        self.clause(&mut rendered, "   USING ", &source, scope.indent);
        let condition = self.expression(&merge.join_condition, &scope, &merge.subqueries)?;
        self.clause(&mut rendered, "   ON ", &condition, scope.indent);
        let by_source = merge.when_clauses.iter().any(|when| {
            matches!(
                when,
                MergeWhenPlan::UpdateNotMatchedBySource { .. }
                    | MergeWhenPlan::DeleteNotMatchedBySource { .. }
                    | MergeWhenPlan::NothingNotMatchedBySource { .. }
            )
        });
        let not_matched = if by_source {
            "NOT MATCHED BY TARGET"
        } else {
            "NOT MATCHED"
        };
        for when in &merge.when_clauses {
            let (kind, condition) = match when {
                MergeWhenPlan::UpdateMatched { condition, .. }
                | MergeWhenPlan::DeleteMatched { condition }
                | MergeWhenPlan::NothingMatched { condition } => ("MATCHED", condition),
                MergeWhenPlan::UpdateNotMatchedBySource { condition, .. }
                | MergeWhenPlan::DeleteNotMatchedBySource { condition }
                | MergeWhenPlan::NothingNotMatchedBySource { condition } => {
                    ("NOT MATCHED BY SOURCE", condition)
                }
                MergeWhenPlan::InsertNotMatched { condition, .. }
                | MergeWhenPlan::NothingNotMatched { condition } => (not_matched, condition),
            };
            self.clause(&mut rendered, "   WHEN ", kind, scope.indent);
            if let Some(condition) = condition {
                let condition = self.expression(condition, &scope, &merge.subqueries)?;
                self.clause(&mut rendered, "    AND ", &condition, scope.indent);
            }
            let action = self.merge_action(when, &scope, &merge.subqueries)?;
            self.clause(&mut rendered, "    THEN ", &action, scope.indent);
        }
        self.returning(
            &mut rendered,
            (&merge.returning, &merge.returning_aliases),
            &scope,
            &merge.subqueries,
        )?;
        Ok(rendered)
    }

    fn merge_action(
        &self,
        when: &MergeWhenPlan,
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<String, SQLError> {
        Ok(match when {
            MergeWhenPlan::UpdateMatched { assignments, .. }
            | MergeWhenPlan::UpdateNotMatchedBySource { assignments, .. } => {
                format!(
                    "UPDATE SET {}",
                    self.assignments(assignments, scope, subqueries)?
                )
            }
            MergeWhenPlan::DeleteMatched { .. }
            | MergeWhenPlan::DeleteNotMatchedBySource { .. } => "DELETE".into(),
            MergeWhenPlan::NothingMatched { .. }
            | MergeWhenPlan::NothingNotMatched { .. }
            | MergeWhenPlan::NothingNotMatchedBySource { .. } => "DO NOTHING".into(),
            MergeWhenPlan::InsertNotMatched {
                columns, values, ..
            } => {
                let mut rendered = String::from("INSERT");
                if !columns.is_empty() {
                    let columns = columns
                        .iter()
                        .map(|target| self.assignment_target(target, scope, subqueries))
                        .collect::<Result<Vec<_>, _>>()?;
                    write!(rendered, " ({})", columns.join(", "))
                        .expect("writing to a String cannot fail");
                }
                if values.is_empty() {
                    rendered.push_str(" DEFAULT VALUES");
                } else {
                    let values = self.expressions(values, scope, subqueries)?;
                    self.clause(
                        &mut rendered,
                        "     VALUES (",
                        &format!("{values})"),
                        scope.indent,
                    );
                }
                rendered
            }
        })
    }

    /// The scope of a data-modifying statement: its target is in its range table, and column references carry their relation's name, as they do below any enclosing namespace. Its `WITH` clause is printed first.
    fn statement_scope(
        &self,
        ctes: &[CtePlan],
        parent: &Scope,
    ) -> Result<(String, Scope), SQLError> {
        let mut scope = parent.clone();
        scope.range_table = true;
        scope.qualify = true;
        scope.column_names_visible = false;
        let rendered = self.with_clause(ctes, &mut scope, true)?;
        Ok((rendered, scope))
    }

    /// The target relation's columns under its alias or its own name.
    fn target_columns(&self, target: &Target<'_>, scope: &Scope) -> Result<Vec<Column>, SQLError> {
        let local = RelationIdentity::parse_reference(target.table)
            .map_err(SQLError::Internal)?
            .1;
        self.source_columns(
            &SourcePlan::Table {
                name: target.table.to_string(),
                qualifier: target.alias.map_or(local, str::to_string),
                alias: target.alias.map(str::to_string),
                column_aliases: Vec::new(),
                bound_columns: None,
                include_descendants: target.include_descendants,
            },
            scope,
        )
    }

    /// `ONLY`, the relation's name as `generate_relation_name` prints it and the written alias.
    fn target_name(&self, target: &Target<'_>) -> Result<String, SQLError> {
        let mut rendered = String::new();
        if !target.include_descendants {
            rendered.push_str("ONLY ");
        }
        rendered.push_str(&self.relation_name(target.table, &Scope::default())?);
        if let Some(alias) = target.alias {
            rendered.push(' ');
            rendered.push_str(&quote_ident(alias));
        }
        Ok(rendered)
    }

    /// `get_update_query_targetlist_def`: each assigned column with the subscripts or fields it assigns through, then its value.
    fn assignments(
        &self,
        assignments: &[AssignmentPlan],
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<String, SQLError> {
        Ok(assignments
            .iter()
            .map(|assignment| {
                Ok(format!(
                    "{} = {}",
                    self.assignment_target(&assignment.target, scope, subqueries)?,
                    self.expression(&assignment.value, scope, subqueries)?
                ))
            })
            .collect::<Result<Vec<_>, SQLError>>()?
            .join(", "))
    }

    /// `processIndirection`: a target column with the field names and subscripts it assigns through.
    fn assignment_target(
        &self,
        target: &AssignmentTarget<ScalarExpr>,
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<String, SQLError> {
        let mut rendered = quote_ident(&target.column);
        for step in &target.indirection {
            match step {
                AssignmentStep::Field(field) => {
                    rendered.push('.');
                    rendered.push_str(&quote_ident(field));
                }
                AssignmentStep::Index(index) => {
                    write!(rendered, "[{}]", self.expression(index, scope, subqueries)?)
                        .expect("writing to a String cannot fail");
                }
                AssignmentStep::Slice { lower, upper } => {
                    let bound = |bound: &Option<Box<ScalarExpr>>| {
                        bound.as_deref().map_or(Ok(String::new()), |bound| {
                            self.expression(bound, scope, subqueries)
                        })
                    };
                    write!(rendered, "[{}:{}]", bound(lower)?, bound(upper)?)
                        .expect("writing to a String cannot fail");
                }
            }
        }
        Ok(rendered)
    }

    /// `get_returning_clause`: `RETURNING` with any renamed `OLD` and `NEW` aliases, then the target list with `*` expanded to the statement's columns.
    fn returning(
        &self,
        rendered: &mut String,
        (returning, aliases): (&[ProjectionPlan], &ReturningAliases),
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<(), SQLError> {
        if returning.is_empty() {
            return Ok(());
        }
        self.clause(rendered, "  RETURNING", "", scope.indent);
        let renamed = [
            (aliases.old != "old").then(|| format!("OLD AS {}", quote_ident(&aliases.old))),
            (aliases.new != "new").then(|| format!("NEW AS {}", quote_ident(&aliases.new))),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        if !renamed.is_empty() {
            write!(rendered, " WITH ({})", renamed.join(", "))
                .expect("writing to a String cannot fail");
        }
        let expanded = expand_stars(returning, scope);
        self.target_list(rendered, &expanded, None, scope, subqueries)
    }
}

/// A target list with `*` and `relation.*` replaced by the columns they denote, as the parser expands them.
pub(super) fn expand_stars(projections: &[ProjectionPlan], scope: &Scope) -> Vec<ProjectionPlan> {
    projections
        .iter()
        .flat_map(|projection| {
            let columns: Vec<&Column> = match &projection.expr {
                ScalarExpr::Star => scope
                    .columns
                    .iter()
                    .filter(|column| !column.hidden)
                    .collect(),
                // An input of a USING join lists its columns in its own order among the hidden copies.
                ScalarExpr::QualifiedStar(qualifier) => {
                    let named = |hidden: bool| {
                        scope
                            .columns
                            .iter()
                            .filter(|column| {
                                &column.qualifier == qualifier && column.hidden == hidden
                            })
                            .collect::<Vec<_>>()
                    };
                    let hidden = named(true);
                    if hidden.is_empty() {
                        named(false)
                    } else {
                        hidden
                    }
                }
                _ => return vec![projection.clone()],
            };
            columns
                .into_iter()
                .map(|column| ProjectionPlan {
                    expr: ScalarExpr::QualifiedColumn {
                        qualifier: column.qualifier.clone(),
                        column: column.name.clone(),
                    },
                    alias: None,
                })
                .collect()
        })
        .collect()
}
