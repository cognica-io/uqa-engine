//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stable SQL rendering for compiler-owned statement trees.

use std::fmt::Write as _;

use uqa_core::{TemporalValue, Value};

use crate::ast::{
    CteBody, CteMaterialization, Expr, FromClause, JoinKind, LockWait, NullsOrder,
    OperatorJoinRelations, OrderBy, Projection, ReturningAliases, SelectStmt, SetOpKind, Statement,
    TableFunction, CTE,
};
use crate::SQLError;

mod commands;
use commands::{delete_sql, insert_sql, merge_sql, update_sql};
mod legacy_vector;
pub use legacy_vector::legacy_vector_expression;
mod window;
pub use window::frame_clause_sql;
use window::window_sql;

/// Render one executable statement represented by UQA's durable SQL AST.
pub fn statement_sql(statement: &Statement) -> Result<String, SQLError> {
    match statement {
        Statement::Select(select) => select_sql(select),
        Statement::Insert(insert) => insert_sql(insert),
        Statement::Update(update) => update_sql(update),
        Statement::Delete(delete) => delete_sql(delete),
        Statement::Merge(merge) => merge_sql(merge),
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

fn select_sql(statement: &SelectStmt) -> Result<String, SQLError> {
    let mut rendered = with_sql(&statement.with)?;
    if let Some(set) = statement.set_op.as_deref() {
        let left = set
            .left
            .as_deref()
            .map_or_else(|| select_body_sql(statement), select_sql)?;
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
        rendered.push_str(&select_sql(&set.right)?);
        rendered.push(')');
        render_order_limit_offset(
            &mut rendered,
            &set.combined_order_by,
            set.combined_limit.as_ref(),
            set.combined_with_ties,
            set.combined_offset.as_ref(),
        )?;
        return Ok(rendered);
    }
    rendered.push_str(&select_body_sql(statement)?);
    Ok(rendered)
}

fn select_body_sql(statement: &SelectStmt) -> Result<String, SQLError> {
    let mut rendered = String::new();
    if statement.values.is_empty() {
        rendered.push_str("SELECT");
        if !statement.distinct_on.is_empty() {
            rendered.push_str(" DISTINCT ON (");
            rendered.push_str(&expr_list(&statement.distinct_on)?);
            rendered.push(')');
        } else if statement.distinct {
            rendered.push_str(" DISTINCT");
        }
        rendered.push(' ');
        rendered.push_str(&projections_sql(&statement.projections)?);
        if let Some(source) = &statement.from {
            rendered.push_str(" FROM ");
            rendered.push_str(&from_sql(source)?);
        }
        if let Some(predicate) = &statement.r#where {
            rendered.push_str(" WHERE ");
            rendered.push_str(&render_expr(predicate)?);
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
                    .map(|set| Ok(format!("({})", expr_list(set)?)))
                    .collect::<Result<Vec<_>, SQLError>>()?
                    .join(", "),
            );
            rendered.push(')');
        } else if !statement.group_by.is_empty() {
            rendered.push_str(" GROUP BY ");
            if statement.group_distinct {
                rendered.push_str("DISTINCT ");
            }
            rendered.push_str(&expr_list(&statement.group_by)?);
        }
        if let Some(predicate) = &statement.having {
            rendered.push_str(" HAVING ");
            rendered.push_str(&render_expr(predicate)?);
        }
    } else {
        rendered.push_str("VALUES ");
        rendered.push_str(&rows_sql(&statement.values)?);
    }
    render_order_limit_offset(
        &mut rendered,
        &statement.order_by,
        statement.limit.as_ref(),
        statement.with_ties,
        statement.offset.as_ref(),
    )?;
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
    Ok(rendered)
}

#[expect(
    clippy::too_many_lines,
    reason = "exhaustive FROM rendering keeps each AST variant visibly complete"
)]
fn from_sql(source: &FromClause) -> Result<String, SQLError> {
    Ok(match source {
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
            rendered.push_str(&from_sql(left)?);
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
            rendered.push_str(&from_sql(right)?);
            if let Some(predicate) = on {
                rendered.push_str(" ON ");
                rendered.push_str(&render_expr(predicate)?);
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
            let mut rendered = format!("(VALUES {})", rows_sql(rows)?);
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
            let mut rendered = table_function_call_sql(name, relations.as_ref(), args)?;
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
                    .collect::<Result<Vec<_>, _>>()?
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
            let mut rendered = format!("({})", select_sql(body)?);
            render_relation_alias(&mut rendered, alias.as_deref(), column_aliases);
            rendered
        }
    })
}

fn table_function_sql(function: &TableFunction) -> Result<String, SQLError> {
    let mut rendered =
        table_function_call_sql(&function.name, function.relations.as_ref(), &function.args)?;
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
    Ok(rendered)
}

/// A table function call whose operator-join relations precede the operands bound to them.
fn table_function_call_sql(
    name: &str,
    relations: Option<&OperatorJoinRelations>,
    args: &[Expr],
) -> Result<String, SQLError> {
    let mut arguments = args
        .iter()
        .map(render_expr)
        .collect::<Result<Vec<_>, _>>()?;
    if let Some(relations) = relations {
        if arguments.is_empty() {
            return Err(SQLError::Internal(format!(
                "operator join table function `{name}` has no left operand"
            )));
        }
        arguments.insert(0, relations.left.clone());
        arguments.insert(2, relations.right.clone());
    }
    Ok(format!("{name}({})", arguments.join(", ")))
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
        Expr::Literal(value) => value_sql(value)?,
        Expr::TypedLiteral { value, ty } => format!("({})::{ty}", value_sql(value)?),
        Expr::Param(index) => format!("${index}"),
        Expr::Func {
            binding:
                Some(crate::ast::FunctionBinding {
                    dispatch: Some(crate::ast::FunctionDispatch::JsonExtract { as_text, path }),
                    ..
                }),
            args,
            ..
        } => {
            let [lhs, rhs] = args.as_slice() else {
                return Err(SQLError::Internal(
                    "JSON extraction requires two operands".into(),
                ));
            };
            let operator = match (*path, *as_text) {
                (false, false) => "->",
                (false, true) => "->>",
                (true, false) => "#>",
                (true, true) => "#>>",
            };
            format!("({} {operator} {})", render_expr(lhs)?, render_expr(rhs)?)
        }
        Expr::Func {
            name,
            binding,
            args,
            distinct,
            order_by,
            order_syntax,
            filter,
            ..
        } => {
            if let Some(crate::ast::FunctionDispatch::NumericOperator(operator)) =
                binding.as_ref().and_then(|binding| binding.dispatch)
            {
                return match args.as_slice() {
                    [argument] if operator.arity() == 1 => Ok(format!(
                        "({} {})",
                        operator.symbol(),
                        render_expr(argument)?
                    )),
                    [left, right] if operator.arity() == 2 => Ok(format!(
                        "({} {} {})",
                        render_expr(left)?,
                        operator.symbol(),
                        render_expr(right)?
                    )),
                    _ => Err(SQLError::Internal(
                        "invalid numeric operator operands".into(),
                    )),
                };
            }
            let mut arguments = expr_list(args)?;
            if *distinct {
                arguments = format!("DISTINCT {arguments}");
            }
            if !order_by.is_empty() && *order_syntax != crate::ast::FunctionOrderSyntax::WithinGroup
            {
                if !arguments.is_empty() {
                    arguments.push(' ');
                }
                arguments.push_str("ORDER BY ");
                arguments.push_str(&order_by_sql(order_by)?);
            }
            let mut rendered = format!("{name}({arguments})");
            if *order_syntax == crate::ast::FunctionOrderSyntax::WithinGroup {
                write!(
                    &mut rendered,
                    " WITHIN GROUP (ORDER BY {})",
                    order_by_sql(order_by)?
                )
                .expect("writing to a String cannot fail");
            }
            if let Some(filter) = filter {
                write!(&mut rendered, " FILTER (WHERE {})", render_expr(filter)?)
                    .expect("writing to a String cannot fail");
            }
            rendered
        }
        Expr::Array(items) => format!("ARRAY[{}]", expr_list(items)?),
        Expr::Row(items) => format!("ROW({})", expr_list(items)?),
        Expr::Binary { op, lhs, rhs } => format!(
            "({} {} {})",
            render_expr(lhs)?,
            binary_operator_sql(*op),
            render_expr(rhs)?
        ),
        Expr::UnaryMinus(inner) => format!("(-{})", render_expr(inner)?),
        Expr::Not(inner) => format!("(NOT {})", render_expr(inner)?),
        Expr::And(items) => format!(
            "({})",
            items
                .iter()
                .map(render_expr)
                .collect::<Result<Vec<_>, _>>()?
                .join(" AND ")
        ),
        Expr::Or(items) => format!(
            "({})",
            items
                .iter()
                .map(render_expr)
                .collect::<Result<Vec<_>, _>>()?
                .join(" OR ")
        ),
        Expr::IsNull { expr, negated } => format!(
            "({} IS {}NULL)",
            render_expr(expr)?,
            if *negated { "NOT " } else { "" }
        ),
        Expr::Between { expr, low, high } => format!(
            "({} BETWEEN {} AND {})",
            render_expr(expr)?,
            render_expr(low)?,
            render_expr(high)?
        ),
        Expr::InList {
            expr,
            list,
            negated,
        } => format!(
            "({} {}IN ({}))",
            render_expr(expr)?,
            if *negated { "NOT " } else { "" },
            expr_list(list)?
        ),
        Expr::WindowCall {
            name,
            args,
            spec,
            filter,
            ..
        } => format!(
            "{name}({}){} OVER {}",
            expr_list(args)?,
            filter
                .as_deref()
                .map(render_expr)
                .transpose()?
                .map_or_else(String::new, |filter| format!(" FILTER (WHERE {filter})")),
            window_sql(spec)?
        ),
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            let mut rendered = String::from("CASE");
            if let Some(base) = base {
                rendered.push(' ');
                rendered.push_str(&render_expr(base)?);
            }
            for (condition, result) in when {
                write!(
                    &mut rendered,
                    " WHEN {} THEN {}",
                    render_expr(condition)?,
                    render_expr(result)?
                )
                .expect("writing to a String cannot fail");
            }
            if let Some(branch) = else_branch {
                rendered.push_str(" ELSE ");
                rendered.push_str(&render_expr(branch)?);
            }
            rendered.push_str(" END");
            rendered
        }
        Expr::Cast { expr, ty } => format!("CAST({} AS {ty})", render_expr(expr)?),
        Expr::ScalarSubquery(body) => format!("({})", select_sql(body)?),
        Expr::Exists { body, negated } => format!(
            "{}EXISTS ({})",
            if *negated { "NOT " } else { "" },
            select_sql(body)?
        ),
        Expr::InSubquery {
            expr,
            body,
            negated,
        } => format!(
            "({} {}IN ({}))",
            render_expr(expr)?,
            if *negated { "NOT " } else { "" },
            select_sql(body)?
        ),
    })
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

fn with_sql(ctes: &[CTE]) -> Result<String, SQLError> {
    if ctes.is_empty() {
        return Ok(String::new());
    }
    let recursive = ctes.iter().any(|cte| cte.recursive);
    Ok(format!(
        "WITH {}{} ",
        if recursive { "RECURSIVE " } else { "" },
        ctes.iter()
            .map(cte_sql)
            .collect::<Result<Vec<_>, _>>()?
            .join(", ")
    ))
}

fn cte_sql(cte: &CTE) -> Result<String, SQLError> {
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
    rendered.push_str(&match &cte.body {
        CteBody::Query(query) => select_sql(query),
        CteBody::Insert(command) => insert_sql(command),
        CteBody::Update(command) => update_sql(command),
        CteBody::Delete(command) => delete_sql(command),
        CteBody::Merge(command) => merge_sql(command),
    }?);
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
            render_expr(&cycle.mark_value)?,
            render_expr(&cycle.mark_default)?,
            ident(&cycle.path_column)
        )
        .expect("writing to a String cannot fail");
    }
    Ok(rendered)
}

fn render_order_limit_offset(
    rendered: &mut String,
    order_by: &[OrderBy],
    limit: Option<&Expr>,
    with_ties: bool,
    offset: Option<&Expr>,
) -> Result<(), SQLError> {
    if !order_by.is_empty() {
        rendered.push_str(" ORDER BY ");
        rendered.push_str(&order_by_sql(order_by)?);
    }
    if with_ties {
        if let Some(offset) = offset {
            rendered.push_str(" OFFSET ");
            rendered.push_str(&render_expr(offset)?);
        }
        if let Some(limit) = limit {
            rendered.push_str(" FETCH FIRST ");
            rendered.push_str(&render_expr(limit)?);
            rendered.push_str(" ROWS WITH TIES");
        }
    } else {
        if let Some(limit) = limit {
            rendered.push_str(" LIMIT ");
            rendered.push_str(&render_expr(limit)?);
        }
        if let Some(offset) = offset {
            rendered.push_str(" OFFSET ");
            rendered.push_str(&render_expr(offset)?);
        }
    }
    Ok(())
}

fn render_returning(
    rendered: &mut String,
    aliases: &ReturningAliases,
    projections: &[Projection],
) -> Result<(), SQLError> {
    if projections.is_empty() {
        return Ok(());
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
    rendered.push_str(&projections_sql(projections)?);
    Ok(())
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

fn assignment_target_sql(target: &crate::ast::AssignmentTarget) -> Result<String, SQLError> {
    use crate::ast::AssignmentStep;
    let mut sql = ident(&target.column);
    for step in &target.indirection {
        match step {
            AssignmentStep::Field(field) => {
                sql.push('.');
                sql.push_str(&ident(field));
            }
            AssignmentStep::Index(index) => {
                sql.push('[');
                sql.push_str(&render_expr(index)?);
                sql.push(']');
            }
            AssignmentStep::Slice { lower, upper } => {
                sql.push('[');
                if let Some(lower) = lower {
                    sql.push_str(&render_expr(lower)?);
                }
                sql.push(':');
                if let Some(upper) = upper {
                    sql.push_str(&render_expr(upper)?);
                }
                sql.push(']');
            }
        }
    }
    Ok(sql)
}

fn assignment_targets_sql(targets: &[crate::ast::AssignmentTarget]) -> Result<String, SQLError> {
    Ok(targets
        .iter()
        .map(assignment_target_sql)
        .collect::<Result<Vec<_>, _>>()?
        .join(", "))
}

fn assignments_sql(
    assignments: &[(crate::ast::AssignmentTarget, Expr)],
) -> Result<String, SQLError> {
    Ok(assignments
        .iter()
        .map(|(target, expression)| {
            Ok(format!(
                "{} = {}",
                assignment_target_sql(target)?,
                render_expr(expression)?
            ))
        })
        .collect::<Result<Vec<_>, SQLError>>()?
        .join(", "))
}

fn projections_sql(projections: &[Projection]) -> Result<String, SQLError> {
    Ok(projections
        .iter()
        .map(|projection| {
            let mut rendered = render_expr(&projection.expr)?;
            if let Some(alias) = &projection.alias {
                rendered.push_str(" AS ");
                rendered.push_str(&ident(alias));
            }
            Ok(rendered)
        })
        .collect::<Result<Vec<_>, SQLError>>()?
        .join(", "))
}

fn order_by_sql(order_by: &[OrderBy]) -> Result<String, SQLError> {
    Ok(order_by
        .iter()
        .map(|order| {
            let mut rendered = render_expr(&order.expr)?;
            if order.descending {
                rendered.push_str(" DESC");
            }
            match order.nulls {
                Some(NullsOrder::First) => rendered.push_str(" NULLS FIRST"),
                Some(NullsOrder::Last) => rendered.push_str(" NULLS LAST"),
                None => {}
            }
            Ok(rendered)
        })
        .collect::<Result<Vec<_>, SQLError>>()?
        .join(", "))
}

fn rows_sql(rows: &[Vec<Expr>]) -> Result<String, SQLError> {
    Ok(rows
        .iter()
        .map(|row| Ok(format!("({})", expr_list(row)?)))
        .collect::<Result<Vec<_>, SQLError>>()?
        .join(", "))
}

fn expr_list(expressions: &[Expr]) -> Result<String, SQLError> {
    Ok(expressions
        .iter()
        .map(render_expr)
        .collect::<Result<Vec<_>, _>>()?
        .join(", "))
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

fn value_sql(value: &Value) -> Result<String, SQLError> {
    Ok(match value {
        Value::Null => "NULL".into(),
        Value::Void => "''::void".into(),
        Value::Bool(value) => if *value { "true" } else { "false" }.into(),
        Value::Int(value) => value.to_string(),
        Value::Float(value) if value.is_finite() => value.to_string(),
        Value::Float(value) => format!("{}::double precision", string_literal(&value.to_string())),
        Value::Str(value) => string_literal(value),
        Value::Enum(value) => return Err(crate::expr::catalog_output_required(value)),
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
        Value::LegacyVector(vector) => legacy_vector_expression(vector)?,
        Value::Array(array) => format!(
            "ARRAY[{}]",
            array
                .elements()
                .iter()
                .map(value_sql)
                .collect::<Result<Vec<_>, _>>()?
                .join(", ")
        ),
        Value::List(values) => format!(
            "ARRAY[{}]",
            values
                .iter()
                .map(value_sql)
                .collect::<Result<Vec<_>, _>>()?
                .join(", ")
        ),
        Value::Row(values) => format!(
            "ROW({})",
            values
                .iter()
                .map(value_sql)
                .collect::<Result<Vec<_>, _>>()?
                .join(", ")
        ),
        Value::Record(fields) => format!(
            "ROW({})",
            fields
                .iter()
                .map(|(_, value)| value_sql(value))
                .collect::<Result<Vec<_>, _>>()?
                .join(", ")
        ),
        Value::Map(value) => format!(
            "{}::jsonb",
            string_literal(&serde_json::to_string(value).map_err(|error| {
                SQLError::Internal(format!("serialize map literal as jsonb: {error}"))
            })?)
        ),
    })
}

#[cfg(test)]
mod tests;
