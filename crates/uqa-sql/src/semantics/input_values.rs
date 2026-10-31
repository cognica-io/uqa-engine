//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Input-value traversal for output liveness, distinct from row-count demand.

use crate::{SQLError, ScalarExpr};
use uqa_core::memory::ProductionControl;

/// Traverse values observed by an expression. COUNT(*) consumes rows, while its FILTER and ORDER BY still consume their own input values.
pub fn visit_input_values(expression: &ScalarExpr, visitor: &mut impl FnMut(&ScalarExpr)) {
    try_visit_input_values(expression, &mut |node| {
        visitor(node);
        Ok::<_, std::convert::Infallible>(())
    })
    .unwrap_or_else(|never| match never {});
}

/// Lend observed input values in traversal order and stop on the first callback error. COUNT(*) keeps its row demand separate from the values used by its FILTER and ORDER BY.
pub fn try_visit_input_values<'a, E>(
    expression: &'a ScalarExpr,
    visitor: &mut impl FnMut(&'a ScalarExpr) -> Result<(), E>,
) -> Result<(), E> {
    visit_inputs(expression, &mut || Ok(()), visitor)
}

/// Preserve both cancellation owners at every visited node, including COUNT(*) nodes that contribute no input-value callback.
pub fn try_visit_input_values_with_control<'a>(
    expression: &'a ScalarExpr,
    control: &ProductionControl<'_>,
    visitor: &mut impl FnMut(&'a ScalarExpr) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    visit_inputs(
        expression,
        &mut || control.check().map_err(Into::into),
        visitor,
    )?;
    control.check()?;
    Ok(())
}

fn visit_inputs<'a, E>(
    expression: &'a ScalarExpr,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
    visitor: &mut impl FnMut(&'a ScalarExpr) -> Result<(), E>,
) -> Result<(), E> {
    expression.try_visit(&mut |node| {
        checkpoint()?;
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
                && crate::semantics::is_builtin_aggregate(node)
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
                    visit_inputs(expression, checkpoint, visitor)?;
                }
                return Ok(false);
            }
        }
        visitor(node)?;
        Ok(true)
    })
}

#[cfg(test)]
mod tests {
    use super::visit_input_values as visit;
    use super::*;

    fn expression(sql: &str) -> ScalarExpr {
        let crate::plan::UnifiedPlan::Query(query) =
            crate::plan::UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
        else {
            panic!("query");
        };
        let crate::plan::RelationalPlan::QueryBlock(mut block) = query.root else {
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
            let mut user = crate::ast::FunctionBinding::builtin_by_name("count", None);
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

    fn ordered_count() -> ScalarExpr {
        let mut count = expression("SELECT count(*) FILTER (WHERE keep > 0)");
        let ScalarExpr::Func { order_by, .. } =
            expression("SELECT count(value ORDER BY rank DESC)")
        else {
            panic!("call");
        };
        let ScalarExpr::Func {
            order_by: target, ..
        } = &mut count
        else {
            panic!("count");
        };
        *target = order_by;
        ScalarExpr::Row(vec![count, ScalarExpr::Column("later".into())])
    }

    #[test]
    fn input_value_observation_lends_names_without_copying_or_demanding_count_star() {
        let expression = ordered_count();
        let mut names = [None; 3];
        let mut index = 0;
        let allocation = allocation_counter::measure(|| {
            try_visit_input_values(&expression, &mut |node| {
                assert!(!matches!(node, ScalarExpr::Star));
                if let ScalarExpr::Column(name) = node {
                    names[index] = Some(name.as_str());
                    index += 1;
                }
                Ok::<_, std::convert::Infallible>(())
            })
            .unwrap();
        });
        assert_eq!(names, [Some("rank"), Some("keep"), Some("later")]);
        assert_eq!(allocation.bytes_total, 0, "{allocation:?}");
        let ScalarExpr::Row(items) = &expression else {
            panic!("row");
        };
        let ScalarExpr::Column(last) = &items[1] else {
            panic!("column");
        };
        assert_eq!(names[2].unwrap().as_ptr(), last.as_ptr());
    }

    #[test]
    fn input_value_observation_stops_at_the_first_callback_failure() {
        let expression = ordered_count();
        let mut names = Vec::new();
        let error = try_visit_input_values(&expression, &mut |node| {
            if let ScalarExpr::Column(name) = node {
                names.push(name.as_str());
                if name == "keep" {
                    return Err("filter observation failed");
                }
            }
            Ok(())
        })
        .unwrap_err();
        assert_eq!(error, "filter observation failed");
        assert_eq!(names, ["rank", "keep"]);
    }

    #[test]
    fn unobserved_row_count_nodes_still_preserve_cancellation_checkpoints() {
        use uqa_core::{memory::MemoryBudget, CancellationToken};
        let expression = ScalarExpr::Row(vec![expression("SELECT count(*)"); 4096]);
        for cancel_original in [false, true] {
            let budget = MemoryBudget::new(0);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let control = ProductionControl::new(&budget, &original, &invoking);
            let mut checkpoints = 0;
            let mut observed = 0;
            let error = visit_inputs(
                &expression,
                &mut || {
                    checkpoints += 1;
                    if checkpoints == 3 {
                        if cancel_original {
                            original.cancel();
                        } else {
                            invoking.cancel();
                        }
                    }
                    control.check().map_err(SQLError::from)
                },
                &mut |node| {
                    assert!(matches!(node, ScalarExpr::Row(_)));
                    observed += 1;
                    Ok(())
                },
            )
            .unwrap_err();
            assert_eq!(error.sqlstate(), Some("57014"));
            assert_eq!(checkpoints, 3);
            assert_eq!(observed, 1);
            let error = try_visit_input_values_with_control(&expression, &control, &mut |_| {
                panic!("cancelled traversal must not observe an input")
            })
            .unwrap_err();
            assert_eq!(error.sqlstate(), Some("57014"));
            assert_eq!(budget.peak(), 0);
        }
    }
}
