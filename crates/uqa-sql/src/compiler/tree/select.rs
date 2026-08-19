//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SELECT clauses, grouping, ordering, set operations, and CTEs.

use super::{
    compile_expr, compile_from_node, extract_strings, right_is_lateral, Expr, FromClause, JoinKind,
    Node, NodeEnum, OrderBy, Projection, Result, SQLError, SelectStmt, SetOp, SetOpKind, Value,
    CTE,
};

pub(in crate::compiler) fn compile_select(
    stmt: &pg_query::protobuf::SelectStmt,
) -> Result<SelectStmt> {
    let locking = super::locking::compile_locking_clauses(&stmt.locking_clause)?;
    if stmt.into_clause.is_some() {
        return Err(SQLError::Unsupported("SELECT INTO is not supported".into()));
    }
    if stmt.group_distinct {
        return Err(SQLError::Unsupported(
            "GROUP BY DISTINCT is not supported".into(),
        ));
    }
    if !stmt.window_clause.is_empty() {
        return Err(SQLError::Unsupported(
            "named WINDOW clauses are not supported".into(),
        ));
    }
    if stmt.limit_option() == pg_query::protobuf::LimitOption::WithTies {
        return Err(SQLError::Unsupported(
            "FETCH ... WITH TIES is not supported".into(),
        ));
    }
    if stmt.op != pg_query::protobuf::SetOperation::SetopNone as i32 && !locking.is_empty() {
        return Err(SQLError::Unsupported(format!(
            "{} is not allowed with UNION/INTERSECT/EXCEPT",
            locking[0].strength.sql_name()
        )));
    }
    if stmt.op != pg_query::protobuf::SetOperation::SetopNone as i32 {
        for operand in [stmt.larg.as_deref(), stmt.rarg.as_deref()]
            .into_iter()
            .flatten()
        {
            let operand_locking = super::locking::compile_locking_clauses(&operand.locking_clause)?;
            if let Some(clause) = operand_locking.first() {
                return Err(SQLError::Unsupported(format!(
                    "{} is not allowed with UNION/INTERSECT/EXCEPT",
                    clause.strength.sql_name()
                )));
            }
        }
    }
    let from = compile_from_list(&stmt.from_clause)?;
    let projections = compile_projections(&stmt.target_list)?;
    let values = compile_values_lists(&stmt.values_lists)?;
    let r#where = stmt
        .where_clause
        .as_ref()
        .map(|w| compile_expr(w))
        .transpose()?;
    let order_by = compile_order_by(&stmt.sort_clause)?;
    let limit = compile_limit_offset_expr(stmt.limit_count.as_deref())?;
    let offset = compile_limit_offset_expr(stmt.limit_offset.as_deref())?;
    let (group_by, grouping_sets) = compile_group_clause(&stmt.group_clause)?;
    // Resolve GROUP BY 1 / GROUP BY <alias> against the SELECT list.
    // Postgres prefers a real column when one matches, falling back to
    // the alias; we don't have schema info here, so we only rewrite
    // when the alias clearly cannot be a column on the source row
    // (i.e., the projection's expression is something other than a
    // bare reference to that same name).
    let group_by = resolve_group_by_aliases(group_by, &projections);
    let grouping_sets: Vec<Vec<Expr>> = grouping_sets
        .into_iter()
        .map(|s| resolve_group_by_aliases(s, &projections))
        .collect();
    let having = stmt
        .having_clause
        .as_ref()
        .map(|h| compile_expr(h))
        .transpose()?;
    let with = match stmt.with_clause.as_ref() {
        Some(wc) => compile_with_clause(wc)?,
        None => Vec::new(),
    };
    let mut set_op = compile_set_op(stmt)?;

    // For UNION / INTERSECT / EXCEPT shapes the outer SelectStmt carries:
    //   * its own `sortClause` / `limitCount` / `limitOffset` -> the
    //     *combined* ORDER BY / LIMIT / OFFSET applied to `lhs <op> rhs`
    //     (those land on `set_op.combined_*`).
    //   * empty `targetList` / `fromClause`; the LHS branch (with its
    //     own clauses, including its own optional `ORDER BY` / `LIMIT`)
    //     lives in `stmt.larg`. We preserve that full subtree on `SetOp::left`
    //     and mirror its basic clauses on the parent for output-column
    //     discovery and backward compatibility with serialized AST users.
    let (projections, values, mut from, r#where, group_by, order_by, limit, offset) =
        if set_op.is_some() {
            // Promote the outer (combined) clauses onto the SetOp and
            // replace the parent's clauses with the LHS branch's.
            if let Some(so) = set_op.as_mut() {
                so.combined_order_by = order_by;
                so.combined_limit = limit;
                so.combined_offset = offset;
            }
            let lhs_node = stmt
                .larg
                .as_deref()
                .ok_or_else(|| SQLError::Internal("set op missing left".into()))?;
            let lhs = compile_select(lhs_node)?;
            if let Some(so) = set_op.as_mut() {
                so.left = Some(Box::new(lhs.clone()));
            }
            (
                lhs.projections,
                lhs.values,
                lhs.from,
                lhs.r#where,
                lhs.group_by,
                lhs.order_by,
                lhs.limit,
                lhs.offset,
            )
        } else {
            (
                projections,
                values,
                from,
                r#where,
                group_by,
                order_by,
                limit,
                offset,
            )
        };

    if let Some(from) = from.as_mut() {
        reduce_null_rejected_outer_joins_to_fixpoint(from, r#where.as_ref());
    }

    let (distinct, distinct_on) = compile_distinct_clause(&stmt.distinct_clause)?;
    let mut compiled = SelectStmt {
        projections,
        values,
        from,
        r#where,
        group_by,
        grouping_sets,
        having,
        order_by,
        limit,
        offset,
        with,
        set_op,
        distinct,
        distinct_on,
        locking,
    };
    super::locking::propagate_select_locking(&mut compiled)?;
    super::locking::validate_select_locking(&compiled)?;
    Ok(compiled)
}

/// `PostgreSQL`'s planner reduces an outer join before row-lock validation when
/// a qualification cannot be true for the join's null-extended side. The
/// WHERE clause always qualifies; once a join is (or becomes) inner, its ON
/// condition also filters every row and joins nested below it can reduce
/// through it, so the rewrite iterates to a fixpoint. Keeping the rewrite in
/// the typed tree lets execution and validation see the same effective join
/// kind.
fn reduce_null_rejected_outer_joins_to_fixpoint(from: &mut FromClause, predicate: Option<&Expr>) {
    loop {
        let mut quals: Vec<Expr> = predicate.iter().map(|expr| (*expr).clone()).collect();
        collect_inner_join_quals(from, &mut quals);
        let mut changed = false;
        for qual in &quals {
            changed |= reduce_null_rejected_outer_joins(from, qual);
        }
        if !changed {
            break;
        }
    }
}

/// ON conditions of joins that are inner (or reduced to inner) and every
/// join above them is inner as well, so the condition applies to all rows the
/// enclosing FROM item can produce.
fn collect_inner_join_quals(from: &FromClause, quals: &mut Vec<Expr>) {
    let FromClause::Join {
        left,
        right,
        kind,
        on,
        ..
    } = from
    else {
        return;
    };
    if !matches!(kind, JoinKind::Inner) {
        return;
    }
    if let Some(on) = on.as_ref() {
        quals.push(on.clone());
    }
    collect_inner_join_quals(left, quals);
    collect_inner_join_quals(right, quals);
}

fn reduce_null_rejected_outer_joins(from: &mut FromClause, predicate: &Expr) -> bool {
    let FromClause::Join {
        left, right, kind, ..
    } = from
    else {
        return false;
    };
    let mut left_names = std::collections::BTreeSet::new();
    let mut right_names = std::collections::BTreeSet::new();
    collect_visible_qualifiers(left, &mut left_names);
    collect_visible_qualifiers(right, &mut right_names);
    let rejects_left = predicate_rejects_null_extended_side(predicate, &left_names);
    let rejects_right = predicate_rejects_null_extended_side(predicate, &right_names);
    let reduced = match (*kind, rejects_left, rejects_right) {
        (JoinKind::Left, _, true) | (JoinKind::Right, true, _) => JoinKind::Inner,
        (JoinKind::Full, true, true) => JoinKind::Inner,
        (JoinKind::Full, true, false) => JoinKind::Left,
        (JoinKind::Full, false, true) => JoinKind::Right,
        (kind, _, _) => kind,
    };
    let mut changed = reduced != *kind;
    *kind = reduced;
    changed |= reduce_null_rejected_outer_joins(left, predicate);
    changed |= reduce_null_rejected_outer_joins(right, predicate);
    changed
}

fn collect_visible_qualifiers(from: &FromClause, names: &mut std::collections::BTreeSet<String>) {
    match from {
        FromClause::Table {
            name,
            qualifier,
            alias,
        } => {
            if let Some(alias) = alias {
                names.insert(alias.clone());
            } else {
                names.insert(qualifier.clone());
                names.insert(name.clone());
                if let Some((_, local)) = name.rsplit_once('.') {
                    names.insert(local.to_string());
                }
            }
        }
        FromClause::Join { left, right, .. } => {
            collect_visible_qualifiers(left, names);
            collect_visible_qualifiers(right, names);
        }
        FromClause::Values { alias, .. }
        | FromClause::Subquery { alias, .. }
        | FromClause::Function { alias, .. } => {
            if let Some(alias) = alias {
                names.insert(alias.clone());
            }
        }
    }
}

const TRUTH_FALSE: u8 = 1;
const TRUTH_TRUE: u8 = 2;
const TRUTH_NULL: u8 = 4;
const TRUTH_ANY: u8 = TRUTH_FALSE | TRUTH_TRUE | TRUTH_NULL;

fn predicate_rejects_null_extended_side(
    expression: &Expr,
    qualifiers: &std::collections::BTreeSet<String>,
) -> bool {
    !qualifiers.is_empty() && truth_values_with_null_side(expression, qualifiers) & TRUTH_TRUE == 0
}

fn truth_values_with_null_side(
    expression: &Expr,
    qualifiers: &std::collections::BTreeSet<String>,
) -> u8 {
    match expression {
        Expr::Literal(Value::Bool(value)) => {
            if *value {
                TRUTH_TRUE
            } else {
                TRUTH_FALSE
            }
        }
        Expr::Literal(Value::Null) => TRUTH_NULL,
        Expr::IsNull { expr, negated } if expression_is_null_with_side(expr, qualifiers) => {
            if *negated {
                TRUTH_FALSE
            } else {
                TRUTH_TRUE
            }
        }
        Expr::Between {
            expr, low, high, ..
        } => {
            let value_is_null = expression_is_null_with_side(expr, qualifiers);
            let low_is_null = expression_is_null_with_side(low, qualifiers);
            let high_is_null = expression_is_null_with_side(high, qualifiers);
            if value_is_null || (low_is_null && high_is_null) {
                TRUTH_NULL
            } else if low_is_null || high_is_null {
                TRUTH_FALSE | TRUTH_NULL
            } else {
                TRUTH_ANY
            }
        }
        expression if expression_is_null_with_side(expression, qualifiers) => TRUTH_NULL,
        Expr::Not(inner) => negate_truth_values(truth_values_with_null_side(inner, qualifiers)),
        Expr::And(items) => items.iter().fold(TRUTH_TRUE, |left, right| {
            combine_truth_values(left, truth_values_with_null_side(right, qualifiers), true)
        }),
        Expr::Or(items) => items.iter().fold(TRUTH_FALSE, |left, right| {
            combine_truth_values(left, truth_values_with_null_side(right, qualifiers), false)
        }),
        _ => TRUTH_ANY,
    }
}

/// Whether `expression` is certainly NULL when every column of the given qualifiers is NULL.
fn expression_is_null_with_side(
    expression: &Expr,
    qualifiers: &std::collections::BTreeSet<String>,
) -> bool {
    match expression {
        Expr::Literal(Value::Null) => true,
        Expr::QualifiedColumn { qualifier, .. } => qualifiers.contains(qualifier),
        Expr::Binary { lhs, rhs, .. } => {
            expression_is_null_with_side(lhs, qualifiers)
                || expression_is_null_with_side(rhs, qualifiers)
        }
        Expr::UnaryMinus(inner) | Expr::Cast { expr: inner, .. } => {
            expression_is_null_with_side(inner, qualifiers)
        }
        Expr::Between {
            expr, low, high, ..
        } => {
            expression_is_null_with_side(expr, qualifiers)
                || (expression_is_null_with_side(low, qualifiers)
                    && expression_is_null_with_side(high, qualifiers))
        }
        Expr::InList { expr, .. } => expression_is_null_with_side(expr, qualifiers),
        Expr::Func {
            name,
            args,
            distinct: _,
            order_by: _,
            filter: _,
            binding: _,
        } if crate::expr::builtin_scalar_function_strictness(name, args.len()) == Some(true) => {
            args.iter()
                .map(function_argument_value)
                .any(|argument| expression_is_null_with_side(argument, qualifiers))
        }
        _ => false,
    }
}

fn function_argument_value(expression: &Expr) -> &Expr {
    let Expr::Func { name, args, .. } = expression else {
        return expression;
    };
    if name == crate::expr::NAMED_ARG_FUNCTION {
        args.get(1).unwrap_or(expression)
    } else {
        expression
    }
}

fn negate_truth_values(values: u8) -> u8 {
    (u8::from(values & TRUTH_FALSE != 0) * TRUTH_TRUE)
        | (u8::from(values & TRUTH_TRUE != 0) * TRUTH_FALSE)
        | (values & TRUTH_NULL)
}

fn combine_truth_values(left: u8, right: u8, and: bool) -> u8 {
    let mut output = 0;
    for lhs in [TRUTH_FALSE, TRUTH_TRUE, TRUTH_NULL] {
        if left & lhs == 0 {
            continue;
        }
        for rhs in [TRUTH_FALSE, TRUTH_TRUE, TRUTH_NULL] {
            if right & rhs == 0 {
                continue;
            }
            output |= if and {
                match (lhs, rhs) {
                    (TRUTH_FALSE, _) | (_, TRUTH_FALSE) => TRUTH_FALSE,
                    (TRUTH_TRUE, TRUTH_TRUE) => TRUTH_TRUE,
                    _ => TRUTH_NULL,
                }
            } else {
                match (lhs, rhs) {
                    (TRUTH_TRUE, _) | (_, TRUTH_TRUE) => TRUTH_TRUE,
                    (TRUTH_FALSE, TRUTH_FALSE) => TRUTH_FALSE,
                    _ => TRUTH_NULL,
                }
            };
        }
    }
    output
}

pub(in crate::compiler) fn compile_values_lists(nodes: &[Node]) -> Result<Vec<Vec<Expr>>> {
    nodes
        .iter()
        .map(|node| {
            let Some(NodeEnum::List(list)) = node.node.as_ref() else {
                return Err(SQLError::Internal("VALUES contains a malformed row".into()));
            };
            list.items.iter().map(compile_expr).collect()
        })
        .collect()
}

pub(in crate::compiler) fn compile_distinct_clause(nodes: &[Node]) -> Result<(bool, Vec<Expr>)> {
    if nodes.is_empty() {
        return Ok((false, Vec::new()));
    }
    let mut distinct_on = Vec::new();
    for node in nodes {
        match node.node.as_ref() {
            None => return Ok((true, Vec::new())),
            Some(NodeEnum::AConst(c)) if c.isnull || c.val.is_none() => {
                return Ok((true, Vec::new()));
            }
            Some(_) => distinct_on.push(compile_expr(node)?),
        }
    }
    Ok((true, distinct_on))
}

pub(in crate::compiler) fn compile_from_list(nodes: &[Node]) -> Result<Option<FromClause>> {
    let Some(first) = nodes.first() else {
        return Ok(None);
    };
    let mut current = compile_from_node(first)?;
    for node in nodes.iter().skip(1) {
        let lateral = right_is_lateral(node);
        current = FromClause::Join {
            left: Box::new(current),
            right: Box::new(compile_from_node(node)?),
            kind: JoinKind::Cross,
            on: None,
            using: None,
            natural: false,
            lateral,
        };
    }
    Ok(Some(current))
}

pub(in crate::compiler) fn resolve_group_by_aliases(
    group_by: Vec<Expr>,
    projections: &[Projection],
) -> Vec<Expr> {
    group_by
        .into_iter()
        .map(|g| match &g {
            // GROUP BY <ordinal>: refers to the Nth projection.
            Expr::Literal(Value::Int(n)) if *n >= 1 => match usize::try_from(*n) {
                Ok(position) if position <= projections.len() => {
                    projections[position - 1].expr.clone()
                }
                _ => g,
            },
            // GROUP BY <alias>: only rewrite when the alias points at
            // a non-trivial expression. If the projection is just a
            // column reference with the same name the original AST is
            // already correct.
            Expr::Column(name) => {
                for p in projections {
                    if let Some(alias) = &p.alias {
                        if alias == name {
                            if let Expr::Column(col_name) = &p.expr {
                                if col_name == name {
                                    return g;
                                }
                            }
                            return p.expr.clone();
                        }
                    }
                }
                g
            }
            _ => g,
        })
        .collect()
}

pub(in crate::compiler) fn compile_group_clause(
    nodes: &[pg_query::protobuf::Node],
) -> Result<(Vec<Expr>, Vec<Vec<Expr>>)> {
    use pg_query::protobuf::GroupingSetKind;

    fn simple_item(node: &pg_query::protobuf::Node) -> Result<Vec<Expr>> {
        match node.node.as_ref() {
            Some(NodeEnum::GroupingSet(grouping)) => match grouping.kind() {
                GroupingSetKind::GroupingSetEmpty => Ok(Vec::new()),
                GroupingSetKind::GroupingSetSimple => grouping
                    .content
                    .iter()
                    .map(compile_expr)
                    .collect::<Result<Vec<_>>>(),
                other => Err(SQLError::Unsupported(format!(
                    "nested grouping item {other:?} is not a simple grouping key"
                ))),
            },
            Some(_) => Ok(vec![compile_expr(node)?]),
            None => Err(SQLError::Internal(
                "GROUP BY contains an empty parse node".into(),
            )),
        }
    }

    fn expand(grouping: &pg_query::protobuf::GroupingSet) -> Result<Vec<Vec<Expr>>> {
        match grouping.kind() {
            GroupingSetKind::GroupingSetEmpty => Ok(vec![Vec::new()]),
            GroupingSetKind::GroupingSetSimple => Ok(vec![grouping
                .content
                .iter()
                .map(simple_item)
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .flatten()
                .collect()]),
            GroupingSetKind::GroupingSetRollup => {
                let items = grouping
                    .content
                    .iter()
                    .map(simple_item)
                    .collect::<Result<Vec<_>>>()?;
                let set_count = items.len().checked_add(1).ok_or_else(|| {
                    SQLError::Unsupported("ROLLUP has too many grouping items".into())
                })?;
                let mut sets = Vec::new();
                sets.try_reserve(set_count).map_err(|error| {
                    SQLError::Unsupported(format!(
                        "ROLLUP expansion of {set_count} grouping sets is too large: {error}"
                    ))
                })?;
                for prefix in (0..=items.len()).rev() {
                    sets.push(items[..prefix].iter().flatten().cloned().collect());
                }
                Ok(sets)
            }
            GroupingSetKind::GroupingSetCube => {
                let items = grouping
                    .content
                    .iter()
                    .map(simple_item)
                    .collect::<Result<Vec<_>>>()?;
                let shift = u32::try_from(items.len()).map_err(|_| {
                    SQLError::Unsupported(format!(
                        "CUBE has too many grouping items: {}",
                        items.len()
                    ))
                })?;
                let set_count = 1_usize.checked_shl(shift).ok_or_else(|| {
                    SQLError::Unsupported(format!(
                        "CUBE has too many grouping items: {}",
                        items.len()
                    ))
                })?;
                let mut sets = Vec::new();
                sets.try_reserve(set_count).map_err(|error| {
                    SQLError::Unsupported(format!(
                        "CUBE expansion of {set_count} grouping sets is too large: {error}"
                    ))
                })?;
                for mask in 0..set_count {
                    let mut set = Vec::new();
                    for (index, item) in items.iter().enumerate() {
                        if mask & (1_usize << index) != 0 {
                            set.extend(item.iter().cloned());
                        }
                    }
                    sets.push(set);
                }
                Ok(sets)
            }
            GroupingSetKind::GroupingSetSets => {
                let mut sets = Vec::new();
                for child in &grouping.content {
                    match child.node.as_ref() {
                        Some(NodeEnum::GroupingSet(nested)) => sets.extend(expand(nested)?),
                        Some(_) => sets.push(vec![compile_expr(child)?]),
                        None => {
                            return Err(SQLError::Internal(
                                "GROUPING SETS contains an empty parse node".into(),
                            ))
                        }
                    }
                }
                Ok(sets)
            }
            other => Err(SQLError::Unsupported(format!(
                "GROUP BY grouping-set kind {other:?}"
            ))),
        }
    }

    let mut plain = Vec::new();
    let mut combined_sets = vec![Vec::new()];
    let mut has_grouping_set = false;
    for node in nodes {
        let alternatives = match node.node.as_ref() {
            Some(NodeEnum::GroupingSet(grouping)) => {
                has_grouping_set = true;
                expand(grouping)?
            }
            Some(_) => {
                let expression = compile_expr(node)?;
                plain.push(expression.clone());
                vec![vec![expression]]
            }
            None => {
                return Err(SQLError::Internal(
                    "GROUP BY contains an empty parse node".into(),
                ))
            }
        };

        let mut product = Vec::new();
        let product_count = combined_sets
            .len()
            .checked_mul(alternatives.len())
            .ok_or_else(|| {
                SQLError::Unsupported("GROUP BY expansion count overflowed usize".into())
            })?;
        product.try_reserve(product_count).map_err(|error| {
            SQLError::Unsupported(format!("GROUP BY expansion is too large: {error}"))
        })?;
        for prefix in &combined_sets {
            for alternative in &alternatives {
                let mut set = prefix.clone();
                set.extend(alternative.iter().cloned());
                product.push(set);
            }
        }
        combined_sets = product;
    }

    if has_grouping_set {
        Ok((Vec::new(), combined_sets))
    } else {
        Ok((plain, Vec::new()))
    }
}

pub(in crate::compiler) fn compile_projections(
    targets: &[pg_query::protobuf::Node],
) -> Result<Vec<Projection>> {
    let mut out = Vec::with_capacity(targets.len());
    for target_node in targets {
        let inner = target_node
            .node
            .as_ref()
            .ok_or_else(|| SQLError::Internal("SELECT contains an empty target".into()))?;
        let res_target = match inner {
            NodeEnum::ResTarget(t) => t,
            _ => return Err(SQLError::Internal(format!("unexpected target {inner:?}"))),
        };
        let alias = if res_target.name.is_empty() {
            None
        } else {
            Some(res_target.name.clone())
        };
        let expr = match &res_target.val {
            Some(node) => compile_expr(node)?,
            None => return Err(SQLError::Internal("ResTarget without value".into())),
        };
        out.push(Projection { expr, alias });
    }
    Ok(out)
}

pub(in crate::compiler) fn compile_order_by(
    sort_clause: &[pg_query::protobuf::Node],
) -> Result<Vec<OrderBy>> {
    let mut out = Vec::with_capacity(sort_clause.len());
    for sort_node in sort_clause {
        let inner = sort_node
            .node
            .as_ref()
            .ok_or_else(|| SQLError::Internal("ORDER BY contains an empty item".into()))?;
        let NodeEnum::SortBy(sb) = inner else {
            return Err(SQLError::Internal(format!(
                "ORDER BY expected SortBy, got {inner:?}"
            )));
        };
        let expr_node = sb
            .node
            .as_ref()
            .ok_or_else(|| SQLError::Internal("SortBy without expr".into()))?;
        let expr = compile_expr(expr_node)?;
        let (descending, nulls) = compile_sort_options(sb, "ORDER BY")?;
        out.push(OrderBy {
            expr,
            descending,
            nulls,
        });
    }
    Ok(out)
}

pub(in crate::compiler) fn compile_sort_options(
    sort: &pg_query::protobuf::SortBy,
    context: &str,
) -> Result<(bool, Option<crate::ast::NullsOrder>)> {
    use pg_query::protobuf::{SortByDir, SortByNulls};

    let direction = SortByDir::try_from(sort.sortby_dir).map_err(|_| {
        SQLError::Internal(format!(
            "{context} has invalid sort direction {}",
            sort.sortby_dir
        ))
    })?;
    let descending = match direction {
        SortByDir::SortbyDefault | SortByDir::SortbyAsc => false,
        SortByDir::SortbyDesc => true,
        SortByDir::SortbyUsing => {
            return Err(SQLError::Unsupported(format!(
                "{context} USING operators are not represented by OrderBy"
            )));
        }
        SortByDir::Undefined => {
            return Err(SQLError::Internal(format!(
                "{context} has an undefined sort direction"
            )));
        }
    };
    let null_order = SortByNulls::try_from(sort.sortby_nulls).map_err(|_| {
        SQLError::Internal(format!(
            "{context} has invalid NULLS ordering {}",
            sort.sortby_nulls
        ))
    })?;
    let nulls = match null_order {
        SortByNulls::SortbyNullsDefault => None,
        SortByNulls::SortbyNullsFirst => Some(crate::ast::NullsOrder::First),
        SortByNulls::SortbyNullsLast => Some(crate::ast::NullsOrder::Last),
        SortByNulls::Undefined => {
            return Err(SQLError::Internal(format!(
                "{context} has an undefined NULLS ordering"
            )));
        }
    };
    Ok((descending, nulls))
}

pub(in crate::compiler) fn compile_set_op(
    stmt: &pg_query::protobuf::SelectStmt,
) -> Result<Option<Box<SetOp>>> {
    let kind = match stmt.op() {
        pg_query::protobuf::SetOperation::SetopNone => return Ok(None),
        pg_query::protobuf::SetOperation::SetopUnion => SetOpKind::Union,
        pg_query::protobuf::SetOperation::SetopIntersect => SetOpKind::Intersect,
        pg_query::protobuf::SetOperation::SetopExcept => SetOpKind::Except,
        other => return Err(SQLError::Unsupported(format!("set op {other:?}"))),
    };
    if stmt.larg.is_none() {
        return Err(SQLError::Internal("set op missing left".into()));
    }
    let right_node = stmt
        .rarg
        .as_deref()
        .ok_or_else(|| SQLError::Internal("set op missing right".into()))?;
    let right = compile_select(right_node)?;
    Ok(Some(Box::new(SetOp {
        kind,
        all: stmt.all,
        left: None,
        right,
        // The outer SelectStmt's ORDER BY / LIMIT / OFFSET land here
        // when `compile_select` finishes - the caller fills these in
        // because at this point we don't have the parent's clauses
        // resolved yet. Default to empty / None until then.
        combined_order_by: Vec::new(),
        combined_limit: None,
        combined_offset: None,
    })))
}

pub(in crate::compiler) fn compile_with_clause(
    wc: &pg_query::protobuf::WithClause,
) -> Result<Vec<CTE>> {
    let mut out = Vec::with_capacity(wc.ctes.len());
    for cte_node in &wc.ctes {
        let inner = cte_node
            .node
            .as_ref()
            .ok_or_else(|| SQLError::Internal("WITH contains an empty CTE".into()))?;
        let cte = match inner {
            NodeEnum::CommonTableExpr(c) => c,
            _ => return Err(SQLError::Internal("expected CommonTableExpr".into())),
        };
        if cte.ctename.is_empty() {
            return Err(SQLError::Internal("CTE name is empty".into()));
        }
        if cte.search_clause.is_some() {
            return Err(SQLError::Unsupported(
                "recursive CTE SEARCH clauses are not supported".into(),
            ));
        }
        if cte.cycle_clause.is_some() {
            return Err(SQLError::Unsupported(
                "recursive CTE CYCLE clauses are not supported".into(),
            ));
        }
        match cte.ctematerialized() {
            pg_query::protobuf::CteMaterialize::CtematerializeUndefined
            | pg_query::protobuf::CteMaterialize::Default
            | pg_query::protobuf::CteMaterialize::Always => {}
            pg_query::protobuf::CteMaterialize::Never => {
                return Err(SQLError::Unsupported(
                    "CTE NOT MATERIALIZED is not supported".into(),
                ));
            }
        }
        let select_node = cte
            .ctequery
            .as_ref()
            .ok_or_else(|| SQLError::Internal("CTE without query".into()))?;
        let select_inner = select_node
            .node
            .as_ref()
            .ok_or_else(|| SQLError::Internal("CTE query node empty".into()))?;
        let select = match select_inner {
            NodeEnum::SelectStmt(s) => s,
            _ => return Err(SQLError::Unsupported("CTE body must be SELECT".into())),
        };
        let columns = extract_strings(&cte.aliascolnames)?;
        out.push(CTE {
            name: cte.ctename.clone(),
            columns,
            recursive: wc.recursive,
            query: Box::new(compile_select(select)?),
        });
    }
    Ok(out)
}

/// Compile a `LIMIT` / `OFFSET` operand into an [`Expr`]. The
/// expression is resolved to an integer at execute time, so `LIMIT $1`
/// and other parameter-bearing forms work end-to-end. `None` means the
/// clause was absent entirely (`SELECT ... LIMIT NULL` is also `None`
/// because PG treats `NULL` as "no limit").
pub(in crate::compiler) fn compile_limit_offset_expr(node: Option<&Node>) -> Result<Option<Expr>> {
    use pg_query::protobuf::a_const::Val;
    let Some(node) = node else { return Ok(None) };
    let inner = node
        .node
        .as_ref()
        .ok_or_else(|| SQLError::Internal("LIMIT/OFFSET contains an empty expression".into()))?;
    // `SELECT ... LIMIT NULL` parses as an `AConst` with no `val` --
    // treat it like an absent clause.
    if let NodeEnum::AConst(c) = inner {
        if c.val.is_none() {
            return Ok(None);
        }
        if let Some(Val::Ival(i)) = &c.val {
            if i.ival < 0 {
                return Err(SQLError::Internal("negative LIMIT/OFFSET".into()));
            }
        }
    }
    Ok(Some(compile_expr(node)?))
}

// -------------------------------------------------------------------------
// Expression compiler
// -------------------------------------------------------------------------
