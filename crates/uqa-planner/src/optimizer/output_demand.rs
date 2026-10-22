//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Input-value traversal for output liveness, distinct from row-count demand.

use uqa_sql::ScalarExpr;

/// Traverse values observed by an expression. COUNT(*) consumes rows, while its FILTER and ORDER BY still consume their own input values.
pub(super) fn visit(expression: &ScalarExpr, visitor: &mut impl FnMut(&ScalarExpr)) {
    let result = expression.try_visit(&mut |node| {
        if let ScalarExpr::Func {
            name,
            binding,
            args,
            order_by,
            filter,
            ..
        } = node
        {
            let selected = binding
                .as_ref()
                .map_or(name.as_str(), |binding| &binding.name);
            if matches!(args.as_slice(), [ScalarExpr::Star])
                && uqa_sql::semantics::is_builtin_aggregate(node)
                && selected
                    .rsplit('.')
                    .next()
                    .is_some_and(|name| name.eq_ignore_ascii_case("count"))
            {
                for expression in order_by
                    .iter()
                    .map(|order| &order.expr)
                    .chain(filter.as_deref())
                {
                    visit(expression, visitor);
                }
                return Ok::<_, std::convert::Infallible>(false);
            }
        }
        visitor(node);
        Ok(true)
    });
    match result {
        Ok(()) => {}
        Err(never) => match never {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expression(sql: &str) -> ScalarExpr {
        let crate::UnifiedPlan::Query(query) =
            crate::UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
        else {
            panic!("query");
        };
        let crate::RelationalPlan::QueryBlock(mut block) = query.root else {
            panic!("block");
        };
        block.projections.remove(0).expr
    }

    #[test]
    fn row_count_star_does_not_demand_values_but_keeps_filter_and_ordering() {
        for sql in [
            "SELECT count(*)",
            "SELECT pg_catalog.count(*)",
            "SELECT 2+count(*)",
        ] {
            let mut stars = 0;
            visit(&expression(sql), &mut |node| {
                stars += usize::from(matches!(node, ScalarExpr::Star));
            });
            assert_eq!(stars, 0, "{sql}");
        }
        let mut count = expression("SELECT count(*) FILTER (WHERE keep > 0)");
        let ScalarExpr::Func { order_by, .. } =
            expression("SELECT count(value ORDER BY rank DESC)")
        else {
            panic!("call");
        };
        if let ScalarExpr::Func {
            order_by: target, ..
        } = &mut count
        {
            *target = order_by;
        }
        let mut names = Vec::new();
        visit(&count, &mut |node| {
            if let ScalarExpr::Column(name) = node {
                names.push(name.clone());
            }
        });
        assert_eq!(names, ["rank", "keep"]);
    }

    #[test]
    fn selected_user_routine_and_whole_row_keep_their_value_demand() {
        let mut count = expression("SELECT count(*)");
        if let ScalarExpr::Func { binding, .. } = &mut count {
            let mut user = uqa_sql::ast::FunctionBinding::builtin_by_name("count", None);
            user.builtin = false;
            user.object_id = Some([7; 16]);
            *binding = Some(user);
        }
        let mut stars = 0;
        visit(&count, &mut |node| {
            stars += usize::from(matches!(node, ScalarExpr::Star));
        });
        assert_eq!(stars, 1);
        for sql in ["SELECT count(q)", "SELECT count(q.*)"] {
            let mut reads = 0;
            visit(&expression(sql), &mut |node| {
                reads += usize::from(matches!(
                    node,
                    ScalarExpr::Column(_) | ScalarExpr::QualifiedStar(_)
                ));
            });
            assert_eq!(reads, 1, "{sql}");
        }
    }
}
