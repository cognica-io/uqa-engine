//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SELECT, CTE, set-operation, and clause layout for view definitions.

use std::fmt::Write as _;

use uqa_planner::{OrderPlan, QueryBlockPlan, QueryPlan, RelationalPlan, ScalarExpr};
use uqa_sql::ast::{CteMaterialization, LockWait, NullsOrder, SetOpKind};

use super::sources::{identifier_list, source_count};
use super::{expression_name, query_columns, quote_ident, Deparser, SQLError, Scope};

impl Deparser<'_> {
    pub(super) fn query(
        &self,
        query: &QueryPlan,
        parent: &Scope,
        names: Option<&[String]>,
    ) -> Result<String, SQLError> {
        let mut scope = parent.clone();
        let mut rendered = String::new();
        if query.ctes.iter().any(|cte| cte.recursive) {
            for cte in &query.ctes {
                scope.ctes.insert(cte.name.clone(), cte_columns(cte));
            }
        }
        for (index, cte) in query.ctes.iter().enumerate() {
            if index == 0 {
                rendered.push_str(if query.ctes.iter().any(|cte| cte.recursive) {
                    " WITH RECURSIVE "
                } else {
                    " WITH "
                });
            } else {
                rendered.push_str(", ");
            }
            rendered.push_str(&quote_ident(&cte.name));
            if !cte.columns.is_empty() {
                write!(rendered, "({})", identifier_list(&cte.columns))
                    .expect("writing to a String cannot fail");
            }
            rendered.push_str(" AS ");
            match cte.materialization {
                CteMaterialization::Materialized => rendered.push_str("MATERIALIZED "),
                CteMaterialization::NotMaterialized => rendered.push_str("NOT MATERIALIZED "),
                CteMaterialization::Default => {}
            }
            let child = scope.child();
            write!(
                rendered,
                "(\n{}{}\n{})",
                " ".repeat(child.indent),
                self.query(&cte.query, &child, None)?,
                " ".repeat(child.indent)
            )
            .expect("writing to a String cannot fail");
            self.cte_search_cycle(&mut rendered, cte, &scope)?;
            scope.ctes.insert(cte.name.clone(), cte_columns(cte));
        }
        if !query.ctes.is_empty() {
            rendered.push('\n');
            rendered.push_str(&" ".repeat(scope.indent));
        }
        rendered.push_str(&self.query_root(&query.root, &scope, names)?);
        Ok(rendered)
    }

    fn query_root(
        &self,
        root: &RelationalPlan,
        scope: &Scope,
        names: Option<&[String]>,
    ) -> Result<String, SQLError> {
        Ok(match root {
            RelationalPlan::QueryBlock(block) => self.query_block(block, scope, names)?,
            RelationalPlan::Values { rows, subqueries } => {
                format!(" VALUES {}", self.values(rows, scope, subqueries)?)
            }
            RelationalPlan::SetOp {
                kind,
                all,
                left,
                right,
                order_by,
                limit,
                with_ties,
                offset,
                subqueries,
            } => {
                let mut member = scope.clone();
                member.nested = true;
                let left_sql = self.set_member(left, &member, names)?;
                let right_sql = self.set_member(right, &member, None)?;
                let keyword = match kind {
                    SetOpKind::Union => "UNION",
                    SetOpKind::Intersect => "INTERSECT",
                    SetOpKind::Except => "EXCEPT",
                };
                let mut rendered = format!(
                    "{left_sql}\n{}{keyword}{}\n{}{right_sql}",
                    " ".repeat(scope.indent),
                    if *all { " ALL" } else { "" },
                    " ".repeat(scope.indent)
                );
                let columns = names.map_or_else(|| query_columns(left), <[String]>::to_vec);
                let order = order_by
                    .iter()
                    .map(|order| {
                        let mut order = order.clone();
                        if let ScalarExpr::Column(name) = &order.expr {
                            if let Some(index) = columns.iter().position(|column| column == name) {
                                order.expr = ScalarExpr::Literal(uqa_core::Value::Int(
                                    i64::try_from(index + 1).expect("column ordinal fits i64"),
                                ));
                            }
                        }
                        order
                    })
                    .collect::<Vec<_>>();
                rendered.push_str(&self.order_limit(
                    &order,
                    limit.as_deref(),
                    *with_ties,
                    offset.as_deref(),
                    scope,
                    subqueries,
                )?);
                rendered
            }
        })
    }

    fn set_member(
        &self,
        query: &QueryPlan,
        scope: &Scope,
        names: Option<&[String]>,
    ) -> Result<String, SQLError> {
        let rendered = self.query(query, scope, names)?;
        let needs_parentheses = match &query.root {
            RelationalPlan::QueryBlock(block) => {
                !block.order_by.is_empty() || block.limit.is_some() || block.offset.is_some()
            }
            RelationalPlan::SetOp { .. } => true,
            RelationalPlan::Values { .. } => false,
        } || !query.ctes.is_empty();
        Ok(if needs_parentheses {
            format!("({rendered})")
        } else {
            rendered
        })
    }

    fn query_block(
        &self,
        block: &QueryBlockPlan,
        parent: &Scope,
        names: Option<&[String]>,
    ) -> Result<String, SQLError> {
        let mut scope = parent.clone();
        scope.columns = block
            .from
            .as_ref()
            .map(|source| self.source_columns(source, parent))
            .transpose()?
            .unwrap_or_default();
        scope.qualify = scope.nested
            || block
                .from
                .as_ref()
                .is_some_and(|source| source_count(source) > 1);
        let mut rendered = String::from(" SELECT");
        if !block.distinct_on.is_empty() {
            write!(
                rendered,
                " DISTINCT ON ({})",
                self.expressions(&block.distinct_on, &scope, &block.subqueries)?
            )
            .expect("writing to a String cannot fail");
        } else if block.distinct {
            rendered.push_str(" DISTINCT");
        }
        for (index, projection) in block.projections.iter().enumerate() {
            let mut expression = self.expression(&projection.expr, &scope, &block.subqueries)?;
            let name = names
                .and_then(|names| names.get(index))
                .cloned()
                .or_else(|| projection.alias.clone())
                .unwrap_or_else(|| expression_name(&projection.expr));
            let natural_column = match &projection.expr {
                ScalarExpr::Column(column) | ScalarExpr::QualifiedColumn { column, .. } => {
                    Some(column.as_str())
                }
                _ => None,
            };
            if natural_column != Some(name.as_str()) {
                write!(expression, " AS {}", quote_ident(&name))
                    .expect("writing to a String cannot fail");
            }
            self.projection(&mut rendered, &expression, index, scope.indent);
        }
        if let Some(source) = &block.from {
            clause(
                &mut rendered,
                "   FROM ",
                &self.source(source, &scope, &block.subqueries)?,
                scope.indent,
            );
        }
        if let Some(predicate) = &block.r#where {
            clause(
                &mut rendered,
                "  WHERE ",
                &self.expression(predicate, &scope, &block.subqueries)?,
                scope.indent,
            );
        }
        self.group_having(&mut rendered, block, &scope)?;
        rendered.push_str(&self.order_limit(
            &block.order_by,
            block.limit.as_ref(),
            block.with_ties,
            block.offset.as_ref(),
            &scope,
            &block.subqueries,
        )?);
        for locking in &block.locking {
            let mut lock = locking.strength.sql_name().to_string();
            if !locking.relations.is_empty() {
                write!(lock, " OF {}", identifier_list(&locking.relations))
                    .expect("writing to a String cannot fail");
            }
            lock.push_str(match locking.wait {
                LockWait::Block => "",
                LockWait::NoWait => " NOWAIT",
                LockWait::SkipLocked => " SKIP LOCKED",
            });
            clause(&mut rendered, "  ", &lock, scope.indent);
        }
        Ok(rendered)
    }

    fn projection(&self, rendered: &mut String, expression: &str, index: usize, indent: usize) {
        if expression.starts_with('\n') {
            if index > 0 {
                rendered.push(',');
            }
            rendered.push_str(expression);
            return;
        }
        if index == 0 {
            rendered.push(' ');
        } else {
            rendered.push(',');
            let current = rendered
                .rsplit('\n')
                .next()
                .unwrap_or(rendered)
                .chars()
                .count();
            let length = current + 1 + expression.chars().count();
            if self.wrap < 0
                || (self.wrap > 0
                    && i64::try_from(length).is_ok_and(|length| length <= self.wrap)
                    && !expression.contains('\n'))
            {
                rendered.push(' ');
            } else {
                rendered.push('\n');
                rendered.push_str(&" ".repeat(indent + 4));
            }
        }
        rendered.push_str(expression);
    }

    fn group_having(
        &self,
        rendered: &mut String,
        block: &QueryBlockPlan,
        scope: &Scope,
    ) -> Result<(), SQLError> {
        let grouping = if block.grouping_sets.is_empty() {
            self.expressions(&block.group_by, scope, &block.subqueries)?
        } else {
            let sets = block
                .grouping_sets
                .iter()
                .map(|set| {
                    self.expressions(set, scope, &block.subqueries)
                        .map(|set| format!("({set})"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            format!("GROUPING SETS ({})", sets.join(", "))
        };
        if !grouping.is_empty() {
            clause(
                rendered,
                if block.group_distinct {
                    "  GROUP BY DISTINCT "
                } else {
                    "  GROUP BY "
                },
                &grouping,
                scope.indent,
            );
        }
        if let Some(having) = &block.having {
            clause(
                rendered,
                " HAVING ",
                &self.expression(having, scope, &block.subqueries)?,
                scope.indent,
            );
        }
        Ok(())
    }

    fn order_limit(
        &self,
        order: &[OrderPlan],
        limit: Option<&ScalarExpr>,
        with_ties: bool,
        offset: Option<&ScalarExpr>,
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<String, SQLError> {
        let mut rendered = String::new();
        if !order.is_empty() {
            let order = order
                .iter()
                .map(|order| {
                    self.order_expression(
                        &order.expr,
                        order.descending,
                        order.nulls,
                        scope,
                        subqueries,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?;
            clause(
                &mut rendered,
                "  ORDER BY ",
                &order.join(", "),
                scope.indent,
            );
        }
        if let Some(offset) = offset {
            clause(
                &mut rendered,
                " OFFSET ",
                &self.expression(offset, scope, subqueries)?,
                scope.indent,
            );
        }
        if let Some(limit) = limit {
            let limit = self.expression(limit, scope, subqueries)?;
            if with_ties {
                clause(
                    &mut rendered,
                    " FETCH FIRST (",
                    &format!("{limit}) ROWS WITH TIES"),
                    scope.indent,
                );
            } else {
                clause(&mut rendered, " LIMIT ", &limit, scope.indent);
            }
        }
        Ok(rendered)
    }

    pub(super) fn order_expression(
        &self,
        expr: &ScalarExpr,
        descending: bool,
        nulls: Option<NullsOrder>,
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<String, SQLError> {
        let mut rendered = self.expression(expr, scope, subqueries)?;
        if descending {
            rendered.push_str(" DESC");
        }
        match nulls {
            Some(NullsOrder::First) if !descending => rendered.push_str(" NULLS FIRST"),
            Some(NullsOrder::Last) if descending => rendered.push_str(" NULLS LAST"),
            _ => {}
        }
        Ok(rendered)
    }

    pub(super) fn values(
        &self,
        rows: &[Vec<ScalarExpr>],
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<String, SQLError> {
        rows.iter()
            .map(|row| {
                let cells = row
                    .iter()
                    .map(|value| self.expression(value, scope, subqueries))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(format!("({})", cells.join(",")))
            })
            .collect::<Result<Vec<_>, SQLError>>()
            .map(|rows| rows.join(", "))
    }

    fn cte_search_cycle(
        &self,
        rendered: &mut String,
        cte: &uqa_planner::CtePlan,
        scope: &Scope,
    ) -> Result<(), SQLError> {
        if let Some(search) = &cte.search {
            write!(
                rendered,
                " SEARCH {} FIRST BY {} SET {}",
                if search.breadth_first {
                    "BREADTH"
                } else {
                    "DEPTH"
                },
                identifier_list(&search.columns),
                quote_ident(&search.sequence_column)
            )
            .expect("writing to a String cannot fail");
        }
        if let Some(cycle) = &cte.cycle {
            write!(
                rendered,
                " CYCLE {} SET {} TO {} DEFAULT {} USING {}",
                identifier_list(&cycle.columns),
                quote_ident(&cycle.mark_column),
                self.expression(&cycle.mark_value, scope, &[])?,
                self.expression(&cycle.mark_default, scope, &[])?,
                quote_ident(&cycle.path_column)
            )
            .expect("writing to a String cannot fail");
        }
        Ok(())
    }
}

fn cte_columns(cte: &uqa_planner::CtePlan) -> Vec<String> {
    let mut columns = query_columns(&cte.query);
    for (column, alias) in columns.iter_mut().zip(&cte.columns) {
        column.clone_from(alias);
    }
    if let Some(search) = &cte.search {
        columns.push(search.sequence_column.clone());
    }
    if let Some(cycle) = &cte.cycle {
        columns.extend([cycle.mark_column.clone(), cycle.path_column.clone()]);
    }
    columns
}

fn clause(rendered: &mut String, keyword: &str, body: &str, indent: usize) {
    rendered.push('\n');
    rendered.push_str(&" ".repeat(indent));
    rendered.push_str(keyword);
    rendered.push_str(body);
}
