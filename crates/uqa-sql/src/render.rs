//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stable SQL rendering for compiler-owned statement trees.

use std::fmt::Write as _;

use uqa_core::{TemporalValue, Value};

use crate::ast::{
    CteMaterialization, DeleteStmt, Expr, FrameBound, FrameMode, FromClause, InsertStmt, JoinKind,
    LockWait, NullsOrder, OnConflictAction, OrderBy, Projection, ReturningAliases, SelectStmt,
    SetOpKind, Statement, TableFunction, UpdateStmt, WindowReferenceKind, WindowSpec, CTE,
};
use crate::SQLError;

/// Render one executable statement represented by UQA's durable SQL AST.
pub fn statement_sql(statement: &Statement) -> Result<String, SQLError> {
    match statement {
        Statement::Select(select) => Ok(select_sql(select)),
        Statement::Insert(insert) => Ok(insert_sql(insert)),
        Statement::Update(update) => Ok(update_sql(update)),
        Statement::Delete(delete) => Ok(delete_sql(delete)),
        Statement::Notify { channel, payload } => {
            let payload = if payload.is_empty() {
                String::new()
            } else {
                format!(", {}", string_literal(payload))
            };
            Ok(format!("NOTIFY {}{payload}", ident(channel)))
        }
        _ => Err(SQLError::Internal(
            "durable rewrite-rule action has an unsupported statement kind".into(),
        )),
    }
}

/// Render one compiler-owned scalar expression without consulting runtime state.
pub fn expression_sql(expression: &Expr) -> Result<String, SQLError> {
    render_expr(expression)
}

fn insert_sql(statement: &InsertStmt) -> String {
    let mut rendered = with_sql(&statement.with);
    rendered.push_str("INSERT INTO ");
    rendered.push_str(&only_relation(
        &statement.table,
        statement.include_descendants,
    ));
    render_target_alias(&mut rendered, &statement.table, &statement.target_qualifier);
    if !statement.columns.is_empty() {
        rendered.push_str(" (");
        rendered.push_str(&ident_list(&statement.columns));
        rendered.push(')');
    }
    if statement.rows.as_slice() == [Vec::new()] {
        rendered.push_str(" DEFAULT VALUES");
    } else if !statement.rows.is_empty() {
        rendered.push_str(" VALUES ");
        rendered.push_str(&rows_sql(&statement.rows));
    } else if let Some(select) = statement.select_source.as_deref() {
        rendered.push(' ');
        rendered.push_str(&select_sql(select));
    }
    if let Some(conflict) = &statement.on_conflict {
        rendered.push_str(" ON CONFLICT");
        if !conflict.conflict_columns.is_empty() {
            rendered.push_str(" (");
            rendered.push_str(&ident_list(&conflict.conflict_columns));
            rendered.push(')');
        }
        match &conflict.action {
            OnConflictAction::Nothing => rendered.push_str(" DO NOTHING"),
            OnConflictAction::Update {
                assignments,
                r#where,
            } => {
                rendered.push_str(" DO UPDATE SET ");
                rendered.push_str(&assignments_sql(assignments));
                if let Some(predicate) = r#where {
                    rendered.push_str(" WHERE ");
                    rendered.push_str(&expr_sql(predicate));
                }
            }
        }
    }
    render_returning(
        &mut rendered,
        &statement.returning_aliases,
        &statement.returning,
    );
    rendered
}

fn update_sql(statement: &UpdateStmt) -> String {
    let mut rendered = with_sql(&statement.with);
    rendered.push_str("UPDATE ");
    rendered.push_str(&only_relation(
        &statement.table,
        statement.include_descendants,
    ));
    render_target_alias(&mut rendered, &statement.table, &statement.target_qualifier);
    rendered.push_str(" SET ");
    rendered.push_str(&assignments_sql(&statement.assignments));
    if let Some(source) = &statement.from {
        rendered.push_str(" FROM ");
        rendered.push_str(&from_sql(source));
    }
    if let Some(predicate) = &statement.r#where {
        rendered.push_str(" WHERE ");
        rendered.push_str(&expr_sql(predicate));
    }
    render_returning(
        &mut rendered,
        &statement.returning_aliases,
        &statement.returning,
    );
    rendered
}

fn delete_sql(statement: &DeleteStmt) -> String {
    let mut rendered = with_sql(&statement.with);
    rendered.push_str("DELETE FROM ");
    rendered.push_str(&only_relation(
        &statement.table,
        statement.include_descendants,
    ));
    render_target_alias(&mut rendered, &statement.table, &statement.target_qualifier);
    if let Some(source) = &statement.using {
        rendered.push_str(" USING ");
        rendered.push_str(&from_sql(source));
    }
    if let Some(predicate) = &statement.r#where {
        rendered.push_str(" WHERE ");
        rendered.push_str(&expr_sql(predicate));
    }
    render_returning(
        &mut rendered,
        &statement.returning_aliases,
        &statement.returning,
    );
    rendered
}

fn select_sql(statement: &SelectStmt) -> String {
    let mut rendered = with_sql(&statement.with);
    if let Some(set) = statement.set_op.as_deref() {
        let left = set
            .left
            .as_deref()
            .map_or_else(|| select_body_sql(statement), select_sql);
        rendered.push('(');
        rendered.push_str(&left);
        rendered.push_str(") ");
        rendered.push_str(match set.kind {
            SetOpKind::Union => "UNION",
            SetOpKind::Intersect => "INTERSECT",
            SetOpKind::Except => "EXCEPT",
        });
        if set.all {
            rendered.push_str(" ALL");
        }
        rendered.push_str(" (");
        rendered.push_str(&select_sql(&set.right));
        rendered.push(')');
        render_order_limit_offset(
            &mut rendered,
            &set.combined_order_by,
            set.combined_limit.as_ref(),
            set.combined_with_ties,
            set.combined_offset.as_ref(),
        );
        return rendered;
    }
    rendered.push_str(&select_body_sql(statement));
    rendered
}

fn select_body_sql(statement: &SelectStmt) -> String {
    let mut rendered = String::new();
    if statement.values.is_empty() {
        rendered.push_str("SELECT");
        if !statement.distinct_on.is_empty() {
            rendered.push_str(" DISTINCT ON (");
            rendered.push_str(&expr_list(&statement.distinct_on));
            rendered.push(')');
        } else if statement.distinct {
            rendered.push_str(" DISTINCT");
        }
        rendered.push(' ');
        rendered.push_str(&projections_sql(&statement.projections));
        if let Some(source) = &statement.from {
            rendered.push_str(" FROM ");
            rendered.push_str(&from_sql(source));
        }
        if let Some(predicate) = &statement.r#where {
            rendered.push_str(" WHERE ");
            rendered.push_str(&expr_sql(predicate));
        }
        if !statement.grouping_sets.is_empty() {
            rendered.push_str(" GROUP BY ");
            if statement.group_distinct {
                rendered.push_str("DISTINCT ");
            }
            rendered.push_str("GROUPING SETS (");
            rendered.push_str(
                &statement
                    .grouping_sets
                    .iter()
                    .map(|set| format!("({})", expr_list(set)))
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            rendered.push(')');
        } else if !statement.group_by.is_empty() {
            rendered.push_str(" GROUP BY ");
            if statement.group_distinct {
                rendered.push_str("DISTINCT ");
            }
            rendered.push_str(&expr_list(&statement.group_by));
        }
        if let Some(predicate) = &statement.having {
            rendered.push_str(" HAVING ");
            rendered.push_str(&expr_sql(predicate));
        }
    } else {
        rendered.push_str("VALUES ");
        rendered.push_str(&rows_sql(&statement.values));
    }
    render_order_limit_offset(
        &mut rendered,
        &statement.order_by,
        statement.limit.as_ref(),
        statement.with_ties,
        statement.offset.as_ref(),
    );
    for locking in &statement.locking {
        rendered.push(' ');
        rendered.push_str(locking.strength.sql_name());
        if !locking.relations.is_empty() {
            rendered.push_str(" OF ");
            rendered.push_str(&ident_list(&locking.relations));
        }
        rendered.push_str(match locking.wait {
            LockWait::Block => "",
            LockWait::SkipLocked => " SKIP LOCKED",
            LockWait::NoWait => " NOWAIT",
        });
    }
    rendered
}

#[expect(
    clippy::too_many_lines,
    reason = "exhaustive FROM rendering keeps each AST variant visibly complete"
)]
fn from_sql(source: &FromClause) -> String {
    match source {
        FromClause::Table {
            name,
            alias,
            column_aliases,
            include_descendants,
            ..
        } => {
            let mut rendered = only_relation(name, *include_descendants);
            render_relation_alias(&mut rendered, alias.as_deref(), column_aliases);
            rendered
        }
        FromClause::Join {
            left,
            right,
            kind,
            on,
            using,
            natural,
            alias,
            column_aliases,
            lateral,
        } => {
            let mut rendered = String::from("(");
            rendered.push_str(&from_sql(left));
            rendered.push(' ');
            if *natural {
                rendered.push_str("NATURAL ");
            }
            rendered.push_str(match kind {
                JoinKind::Inner => "JOIN",
                JoinKind::Left => "LEFT JOIN",
                JoinKind::Right => "RIGHT JOIN",
                JoinKind::Full => "FULL JOIN",
                JoinKind::Cross => "CROSS JOIN",
            });
            rendered.push(' ');
            if *lateral {
                rendered.push_str("LATERAL ");
            }
            rendered.push_str(&from_sql(right));
            if let Some(predicate) = on {
                rendered.push_str(" ON ");
                rendered.push_str(&expr_sql(predicate));
            } else if let Some(using) = using {
                rendered.push_str(" USING (");
                rendered.push_str(&ident_list(&using.columns));
                rendered.push(')');
                if let Some(alias) = &using.alias {
                    rendered.push_str(" AS ");
                    rendered.push_str(&ident(alias));
                }
            }
            rendered.push(')');
            render_relation_alias(&mut rendered, alias.as_deref(), column_aliases);
            rendered
        }
        FromClause::Values {
            rows,
            alias,
            column_aliases,
            ..
        } => {
            let mut rendered = format!("(VALUES {})", rows_sql(rows));
            render_relation_alias(&mut rendered, alias.as_deref(), column_aliases);
            rendered
        }
        FromClause::Function {
            name,
            output_name: _,
            relations,
            args,
            alias,
            column_aliases,
            ordinality,
            column_types,
            ..
        } => {
            let mut arguments = args.iter().map(expr_sql).collect::<Vec<_>>();
            if let Some(relations) = relations {
                arguments.insert(0, relations.left.clone());
                arguments.insert(2, relations.right.clone());
            }
            let mut rendered = format!("{name}({})", arguments.join(", "));
            if *ordinality {
                rendered.push_str(" WITH ORDINALITY");
            }
            render_function_alias(
                &mut rendered,
                alias.as_deref(),
                column_aliases,
                column_types,
            );
            rendered
        }
        FromClause::FunctionGroup {
            functions,
            alias,
            column_aliases,
            ordinality,
        } => {
            let mut rendered = format!(
                "ROWS FROM ({})",
                functions
                    .iter()
                    .map(table_function_sql)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            if *ordinality {
                rendered.push_str(" WITH ORDINALITY");
            }
            render_relation_alias(&mut rendered, alias.as_deref(), column_aliases);
            rendered
        }
        FromClause::Subquery {
            body,
            alias,
            column_aliases,
        } => {
            let mut rendered = format!("({})", select_sql(body));
            render_relation_alias(&mut rendered, alias.as_deref(), column_aliases);
            rendered
        }
    }
}

fn table_function_sql(function: &TableFunction) -> String {
    let mut arguments = function.args.iter().map(expr_sql).collect::<Vec<_>>();
    if let Some(relations) = &function.relations {
        arguments.insert(0, relations.left.clone());
        arguments.insert(2, relations.right.clone());
    }
    let mut rendered = format!("{}({})", function.name, arguments.join(", "));
    if !function.column_types.is_empty() {
        rendered.push_str(" AS (");
        rendered.push_str(
            &function
                .column_aliases
                .iter()
                .zip(&function.column_types)
                .map(|(name, ty)| format!("{} {ty}", ident(name)))
                .collect::<Vec<_>>()
                .join(", "),
        );
        rendered.push(')');
    }
    rendered
}

#[expect(
    clippy::too_many_lines,
    reason = "exhaustive scalar rendering keeps every durable AST variant explicit"
)]
fn render_expr(expression: &Expr) -> Result<String, SQLError> {
    Ok(match expression {
        Expr::Star => "*".into(),
        Expr::QualifiedStar(qualifier) => format!("{}.*", ident(qualifier)),
        Expr::Default => "DEFAULT".into(),
        Expr::Column(name) => ident(name),
        Expr::QualifiedColumn { qualifier, column } => {
            format!("{}.{}", ident(qualifier), ident(column))
        }
        Expr::InternalColumn(column) => {
            return Err(SQLError::Internal(format!(
                "executor-only column {column:?} reached durable SQL rendering"
            )))
        }
        Expr::Literal(value) => value_sql(value),
        Expr::Param(index) => format!("${index}"),
        Expr::Func {
            name,
            args,
            distinct,
            order_by,
            filter,
            ..
        } => {
            let mut arguments = args.iter().map(expr_sql).collect::<Vec<_>>().join(", ");
            if *distinct {
                arguments = format!("DISTINCT {arguments}");
            }
            if !order_by.is_empty() {
                if !arguments.is_empty() {
                    arguments.push(' ');
                }
                arguments.push_str("ORDER BY ");
                arguments.push_str(&order_by_sql(order_by));
            }
            let mut rendered = format!("{name}({arguments})");
            if let Some(filter) = filter {
                write!(&mut rendered, " FILTER (WHERE {})", expr_sql(filter))
                    .expect("writing to a String cannot fail");
            }
            rendered
        }
        Expr::Array(items) => format!("ARRAY[{}]", expr_list(items)),
        Expr::Row(items) => format!("ROW({})", expr_list(items)),
        Expr::Binary { op, lhs, rhs } => format!(
            "({} {} {})",
            expr_sql(lhs),
            binary_operator_sql(*op),
            expr_sql(rhs)
        ),
        Expr::UnaryMinus(inner) => format!("(-{})", expr_sql(inner)),
        Expr::Not(inner) => format!("(NOT {})", expr_sql(inner)),
        Expr::And(items) => format!(
            "({})",
            items.iter().map(expr_sql).collect::<Vec<_>>().join(" AND ")
        ),
        Expr::Or(items) => format!(
            "({})",
            items.iter().map(expr_sql).collect::<Vec<_>>().join(" OR ")
        ),
        Expr::IsNull { expr, negated } => format!(
            "({} IS {}NULL)",
            expr_sql(expr),
            if *negated { "NOT " } else { "" }
        ),
        Expr::Between { expr, low, high } => format!(
            "({} BETWEEN {} AND {})",
            expr_sql(expr),
            expr_sql(low),
            expr_sql(high)
        ),
        Expr::InList {
            expr,
            list,
            negated,
        } => format!(
            "({} {}IN ({}))",
            expr_sql(expr),
            if *negated { "NOT " } else { "" },
            expr_list(list)
        ),
        Expr::WindowCall { name, args, spec } => {
            format!("{name}({}) OVER {}", expr_list(args), window_sql(spec))
        }
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            let mut rendered = String::from("CASE");
            if let Some(base) = base {
                rendered.push(' ');
                rendered.push_str(&expr_sql(base));
            }
            for (condition, result) in when {
                write!(
                    &mut rendered,
                    " WHEN {} THEN {}",
                    expr_sql(condition),
                    expr_sql(result)
                )
                .expect("writing to a String cannot fail");
            }
            if let Some(branch) = else_branch {
                rendered.push_str(" ELSE ");
                rendered.push_str(&expr_sql(branch));
            }
            rendered.push_str(" END");
            rendered
        }
        Expr::Cast { expr, ty } => format!("CAST({} AS {ty})", expr_sql(expr)),
        Expr::ScalarSubquery(body) => format!("({})", select_sql(body)),
        Expr::Exists { body, negated } => format!(
            "{}EXISTS ({})",
            if *negated { "NOT " } else { "" },
            select_sql(body)
        ),
        Expr::InSubquery {
            expr,
            body,
            negated,
        } => format!(
            "({} {}IN ({}))",
            expr_sql(expr),
            if *negated { "NOT " } else { "" },
            select_sql(body)
        ),
    })
}

fn window_sql(spec: &WindowSpec) -> String {
    if let Some(reference) = &spec.reference {
        if reference.kind == WindowReferenceKind::Direct
            && spec.partition_by.is_empty()
            && spec.order_by.is_empty()
            && spec.frame.is_none()
        {
            return ident(&reference.name);
        }
    }
    let mut parts = Vec::new();
    if let Some(reference) = &spec.reference {
        parts.push(ident(&reference.name));
    }
    if !spec.partition_by.is_empty() {
        parts.push(format!("PARTITION BY {}", expr_list(&spec.partition_by)));
    }
    if !spec.order_by.is_empty() {
        parts.push(format!("ORDER BY {}", order_by_sql(&spec.order_by)));
    }
    if let Some(frame) = &spec.frame {
        parts.push(format!(
            "{} BETWEEN {} AND {}",
            match frame.mode {
                FrameMode::Rows => "ROWS",
                FrameMode::Range => "RANGE",
                FrameMode::Groups => "GROUPS",
            },
            frame_bound_sql(&frame.start),
            frame_bound_sql(&frame.end)
        ));
    }
    format!("({})", parts.join(" "))
}

const fn binary_operator_sql(operator: crate::ast::BinaryOp) -> &'static str {
    match operator {
        crate::ast::BinaryOp::Equal => "=",
        crate::ast::BinaryOp::NotEqual => "<>",
        crate::ast::BinaryOp::Less => "<",
        crate::ast::BinaryOp::LessEqual => "<=",
        crate::ast::BinaryOp::Greater => ">",
        crate::ast::BinaryOp::GreaterEqual => ">=",
        crate::ast::BinaryOp::Add => "+",
        crate::ast::BinaryOp::Subtract => "-",
        crate::ast::BinaryOp::Multiply => "*",
        crate::ast::BinaryOp::Divide => "/",
    }
}

fn frame_bound_sql(bound: &FrameBound) -> String {
    match bound {
        FrameBound::UnboundedPreceding => "UNBOUNDED PRECEDING".into(),
        FrameBound::UnboundedFollowing => "UNBOUNDED FOLLOWING".into(),
        FrameBound::CurrentRow => "CURRENT ROW".into(),
        FrameBound::Preceding(expression) => format!("{} PRECEDING", expr_sql(expression)),
        FrameBound::Following(expression) => format!("{} FOLLOWING", expr_sql(expression)),
    }
}

fn with_sql(ctes: &[CTE]) -> String {
    if ctes.is_empty() {
        return String::new();
    }
    let recursive = ctes.iter().any(|cte| cte.recursive);
    format!(
        "WITH {}{} ",
        if recursive { "RECURSIVE " } else { "" },
        ctes.iter().map(cte_sql).collect::<Vec<_>>().join(", ")
    )
}

fn cte_sql(cte: &CTE) -> String {
    let mut rendered = ident(&cte.name);
    if !cte.columns.is_empty() {
        rendered.push_str(" (");
        rendered.push_str(&ident_list(&cte.columns));
        rendered.push(')');
    }
    rendered.push_str(" AS ");
    rendered.push_str(match cte.materialization {
        CteMaterialization::Default => "",
        CteMaterialization::Materialized => "MATERIALIZED ",
        CteMaterialization::NotMaterialized => "NOT MATERIALIZED ",
    });
    rendered.push('(');
    rendered.push_str(&select_sql(&cte.query));
    rendered.push(')');
    if let Some(search) = &cte.search {
        write!(
            &mut rendered,
            " SEARCH {} FIRST BY {} SET {}",
            if search.breadth_first {
                "BREADTH"
            } else {
                "DEPTH"
            },
            ident_list(&search.columns),
            ident(&search.sequence_column)
        )
        .expect("writing to a String cannot fail");
    }
    if let Some(cycle) = &cte.cycle {
        write!(
            &mut rendered,
            " CYCLE {} SET {} TO {} DEFAULT {} USING {}",
            ident_list(&cycle.columns),
            ident(&cycle.mark_column),
            expr_sql(&cycle.mark_value),
            expr_sql(&cycle.mark_default),
            ident(&cycle.path_column)
        )
        .expect("writing to a String cannot fail");
    }
    rendered
}

fn render_order_limit_offset(
    rendered: &mut String,
    order_by: &[OrderBy],
    limit: Option<&Expr>,
    with_ties: bool,
    offset: Option<&Expr>,
) {
    if !order_by.is_empty() {
        rendered.push_str(" ORDER BY ");
        rendered.push_str(&order_by_sql(order_by));
    }
    if with_ties {
        if let Some(offset) = offset {
            rendered.push_str(" OFFSET ");
            rendered.push_str(&expr_sql(offset));
        }
        if let Some(limit) = limit {
            rendered.push_str(" FETCH FIRST ");
            rendered.push_str(&expr_sql(limit));
            rendered.push_str(" ROWS WITH TIES");
        }
    } else {
        if let Some(limit) = limit {
            rendered.push_str(" LIMIT ");
            rendered.push_str(&expr_sql(limit));
        }
        if let Some(offset) = offset {
            rendered.push_str(" OFFSET ");
            rendered.push_str(&expr_sql(offset));
        }
    }
}

fn render_returning(rendered: &mut String, aliases: &ReturningAliases, projections: &[Projection]) {
    if projections.is_empty() {
        return;
    }
    rendered.push_str(" RETURNING ");
    if aliases.old_explicit || aliases.new_explicit {
        rendered.push_str("WITH (");
        let mut names = Vec::new();
        if aliases.old_explicit {
            names.push(format!("OLD AS {}", ident(&aliases.old)));
        }
        if aliases.new_explicit {
            names.push(format!("NEW AS {}", ident(&aliases.new)));
        }
        rendered.push_str(&names.join(", "));
        rendered.push_str(") ");
    }
    rendered.push_str(&projections_sql(projections));
}

fn render_target_alias(rendered: &mut String, relation: &str, qualifier: &str) {
    if relation_local_name(relation) != qualifier {
        rendered.push_str(" AS ");
        rendered.push_str(&ident(qualifier));
    }
}

fn render_relation_alias(rendered: &mut String, alias: Option<&str>, columns: &[String]) {
    if let Some(alias) = alias {
        rendered.push_str(" AS ");
        rendered.push_str(&ident(alias));
        if !columns.is_empty() {
            rendered.push('(');
            rendered.push_str(&ident_list(columns));
            rendered.push(')');
        }
    }
}

fn render_function_alias(
    rendered: &mut String,
    alias: Option<&str>,
    columns: &[String],
    types: &[String],
) {
    if let Some(alias) = alias {
        rendered.push_str(" AS ");
        rendered.push_str(&ident(alias));
    } else if !types.is_empty() {
        rendered.push_str(" AS");
    }
    if !types.is_empty() {
        rendered.push_str(" (");
        rendered.push_str(
            &columns
                .iter()
                .zip(types)
                .map(|(name, ty)| format!("{} {ty}", ident(name)))
                .collect::<Vec<_>>()
                .join(", "),
        );
        rendered.push(')');
    } else if !columns.is_empty() {
        rendered.push('(');
        rendered.push_str(&ident_list(columns));
        rendered.push(')');
    }
}

fn assignments_sql(assignments: &[(String, Expr)]) -> String {
    assignments
        .iter()
        .map(|(column, expression)| format!("{} = {}", ident(column), expr_sql(expression)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn projections_sql(projections: &[Projection]) -> String {
    projections
        .iter()
        .map(|projection| {
            let mut rendered = expr_sql(&projection.expr);
            if let Some(alias) = &projection.alias {
                rendered.push_str(" AS ");
                rendered.push_str(&ident(alias));
            }
            rendered
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn order_by_sql(order_by: &[OrderBy]) -> String {
    order_by
        .iter()
        .map(|order| {
            let mut rendered = expr_sql(&order.expr);
            if order.descending {
                rendered.push_str(" DESC");
            }
            match order.nulls {
                Some(NullsOrder::First) => rendered.push_str(" NULLS FIRST"),
                Some(NullsOrder::Last) => rendered.push_str(" NULLS LAST"),
                None => {}
            }
            rendered
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn rows_sql(rows: &[Vec<Expr>]) -> String {
    rows.iter()
        .map(|row| format!("({})", expr_list(row)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn expr_list(expressions: &[Expr]) -> String {
    expressions
        .iter()
        .map(expr_sql)
        .collect::<Vec<_>>()
        .join(", ")
}

fn expr_sql(expression: &Expr) -> String {
    render_expr(expression).expect("durable SQL AST cannot contain executor-only columns")
}

fn only_relation(name: &str, include_descendants: bool) -> String {
    if include_descendants {
        name.to_string()
    } else {
        format!("ONLY {name}")
    }
}

fn ident_list(names: &[String]) -> String {
    names
        .iter()
        .map(|name| ident(name))
        .collect::<Vec<_>>()
        .join(", ")
}

fn ident(name: &str) -> String {
    crate::expr::quote_ident(name)
}

fn relation_local_name(name: &str) -> &str {
    let mut quoted = false;
    let mut last_dot = None;
    let bytes = name.as_bytes();
    let mut position = 0;
    while position < bytes.len() {
        match bytes[position] {
            b'"' if quoted && bytes.get(position + 1) == Some(&b'"') => position += 2,
            b'"' => {
                quoted = !quoted;
                position += 1;
            }
            b'.' if !quoted => {
                last_dot = Some(position);
                position += 1;
            }
            _ => position += 1,
        }
    }
    let component = &name[last_dot.map_or(0, |dot| dot + 1)..];
    component
        .strip_prefix('"')
        .and_then(|component| component.strip_suffix('"'))
        .unwrap_or(component)
}

fn string_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn value_sql(value: &Value) -> String {
    match value {
        Value::Null => "NULL".into(),
        Value::Void => "''::void".into(),
        Value::Bool(value) => if *value { "true" } else { "false" }.into(),
        Value::Int(value) => value.to_string(),
        Value::Float(value) if value.is_finite() => value.to_string(),
        Value::Float(value) => format!("{}::double precision", string_literal(&value.to_string())),
        Value::Str(value) => string_literal(value),
        Value::FixedChar(value) => format!("{}::character", string_literal(value)),
        Value::Bytes(value) => {
            let mut hex = String::new();
            for byte in value {
                write!(&mut hex, "{byte:02x}").expect("writing to a String cannot fail");
            }
            format!("{}::bytea", string_literal(&format!("\\x{hex}")))
        }
        Value::Temporal(value) => {
            let ty = match value {
                TemporalValue::Date { .. } => "date",
                TemporalValue::Time { .. } => "time",
                TemporalValue::TimeTz { .. } => "time with time zone",
                TemporalValue::Timestamp { .. } => "timestamp",
                TemporalValue::TimestampTz { .. } => "timestamp with time zone",
                TemporalValue::Interval { .. } => "interval",
            };
            format!("{}::{ty}", string_literal(&value.to_sql_string()))
        }
        Value::Decimal(value) if value.is_nan() || value.is_infinite() => {
            format!("{}::numeric", string_literal(&value.to_sql_string()))
        }
        Value::Decimal(value) => format!("{}::numeric", value.to_sql_string()),
        Value::Json(value) => format!("{}::json", string_literal(value)),
        Value::JsonB(value) => format!("{}::jsonb", string_literal(value)),
        Value::Array(array) => format!(
            "ARRAY[{}]",
            array
                .elements()
                .iter()
                .map(value_sql)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::List(values) => format!(
            "ARRAY[{}]",
            values.iter().map(value_sql).collect::<Vec<_>>().join(", ")
        ),
        Value::Row(values) => format!(
            "ROW({})",
            values.iter().map(value_sql).collect::<Vec<_>>().join(", ")
        ),
        Value::Record(fields) => format!(
            "ROW({})",
            fields
                .iter()
                .map(|(_, value)| value_sql(value))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Map(value) => format!(
            "{}::jsonb",
            string_literal(
                &serde_json::to_string(value)
                    .expect("serializing an in-memory Value map cannot fail")
            )
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::statement_sql;

    #[test]
    fn rendered_rule_action_shapes_round_trip_stably() {
        for sql in [
            "SELECT source.key_value, row_number() OVER (ORDER BY source.key_value ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) AS sequence FROM left_table AS source(key_value, payload) JOIN right_table AS other USING (key_value) WHERE source.payload IS NOT NULL ORDER BY sequence LIMIT 2 OFFSET 1",
            "WITH source(value) AS MATERIALIZED (SELECT 1) SELECT value FROM source UNION ALL SELECT 2 ORDER BY value",
            "INSERT INTO target_table AS target(id, value) VALUES (1, 'one') ON CONFLICT (id) DO UPDATE SET value = excluded.value WHERE target.id = 1 RETURNING WITH (OLD AS before, NEW AS after) after.id",
            "UPDATE target_table AS target SET value = source.value FROM source_table AS source(id, value) WHERE target.id = source.id RETURNING target.id",
            "DELETE FROM target_table AS target USING source_table AS source(id) WHERE target.id = source.id RETURNING target.id",
            "NOTIFY rule_channel, 'payload'",
        ] {
            let mut statements = crate::compile(sql).unwrap_or_else(|error| panic!("{sql}: {error}"));
            let rendered = statement_sql(&statements.remove(0))
                .unwrap_or_else(|error| panic!("render {sql}: {error}"));
            let mut reparsed = crate::compile(&rendered)
                .unwrap_or_else(|error| panic!("reparse `{rendered}` from `{sql}`: {error}"));
            let rerendered = statement_sql(&reparsed.remove(0))
                .unwrap_or_else(|error| panic!("rerender `{rendered}`: {error}"));
            assert_eq!(rerendered, rendered, "unstable SQL rendering for `{sql}`");
        }
    }
}
