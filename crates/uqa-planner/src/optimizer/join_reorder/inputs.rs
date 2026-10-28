//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrow join inputs and retain admitted predicate-reference buffers during selection.

use super::{BTreeSet, ColumnOwners, JoinRelation, ScalarExpr, SourcePlan};
use uqa_core::memory::{ProductionControl, ProductionSmallVec, ProductionVec};

pub(super) fn flatten_reorderable_inner_join<'p>(
    source: &'p SourcePlan,
    atoms: &mut ProductionSmallVec<'_, [&'p SourcePlan; 64]>,
    predicates: &mut ProductionVec<'_, &'p ScalarExpr>,
    control: &ProductionControl<'_>,
) -> Result<bool, uqa_sql::SQLError> {
    control.check()?;
    Ok(match source {
        SourcePlan::Join {
            left,
            right,
            kind: uqa_sql::ast::JoinKind::Inner | uqa_sql::ast::JoinKind::Cross,
            on,
            using: None,
            natural: false,
            alias: None,
            column_aliases,
            lateral: false,
            ..
        } => {
            if !column_aliases.is_empty()
                || !flatten_reorderable_inner_join(left, atoms, predicates, control)?
                || !flatten_reorderable_inner_join(right, atoms, predicates, control)?
            {
                return Ok(false);
            }
            if let Some(on) = on {
                collect_conjuncts(on, predicates, control)?;
            }
            true
        }
        SourcePlan::Table { .. } | SourcePlan::Function { .. } => {
            if atoms.len() == 64 {
                return Ok(false);
            }
            atoms.push(source)?;
            true
        }
        SourcePlan::RetainedRows(_)
        | SourcePlan::Join { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::FunctionGroup { .. }
        | SourcePlan::Subquery { .. } => false,
    })
}

pub(super) fn collect_conjuncts<'p>(
    expression: &'p ScalarExpr,
    output: &mut ProductionVec<'_, &'p ScalarExpr>,
    control: &ProductionControl<'_>,
) -> Result<(), uqa_sql::SQLError> {
    control.check()?;
    if let ScalarExpr::And(items) = expression {
        for item in items {
            collect_conjuncts(item, output, control)?;
        }
    } else {
        output.push_copy(expression)?;
    }
    Ok(())
}

type PredicateGroups<'p, 'c> = [Option<ProductionVec<'c, &'p ScalarExpr>>; 64];

/// The eligible join's existing 64-relation domain bounds the alias index and bucket headers; only predicate references require growing storage.
pub(super) fn local_predicate_groups<'p, 'c>(
    relations: &[JoinRelation],
    predicates: &[&'p ScalarExpr],
    aliases: &BTreeSet<String>,
    column_owners: &ColumnOwners,
    control: &ProductionControl<'c>,
) -> Result<PredicateGroups<'p, 'c>, uqa_sql::SQLError> {
    control.check()?;
    let mut groups = std::array::from_fn(|_| None);
    let mut index = [("", 0_usize); 64];
    let index = &mut index[..relations.len()];
    for (position, relation) in relations.iter().enumerate() {
        index[position] = (relation.alias.as_str(), position);
        groups[position] = Some(ProductionVec::new(*control));
    }
    index.sort_unstable_by_key(|(alias, _)| *alias);
    for predicate in predicates {
        control.check()?;
        let mut alias = None;
        if !collect_local_alias(predicate, aliases, column_owners, &mut alias) {
            continue;
        }
        let Some(alias) = alias else {
            continue;
        };
        let Ok(position) = index.binary_search_by_key(&alias, |(name, _)| *name) else {
            continue;
        };
        groups[index[position].1]
            .as_mut()
            .expect("indexed relation bucket")
            .push_copy(*predicate)?;
    }
    Ok(groups)
}

fn collect_local_alias<'a>(
    expression: &'a ScalarExpr,
    aliases: &BTreeSet<String>,
    column_owners: &'a ColumnOwners,
    output: &mut Option<&'a str>,
) -> bool {
    let mut recur = |expression: &'a ScalarExpr| {
        collect_local_alias(expression, aliases, column_owners, output)
    };
    match expression {
        ScalarExpr::Column(column) => {
            let Some(Some((alias, _))) = column_owners.get(column.as_str()) else {
                return false;
            };
            match output {
                Some(previous) => *previous == alias,
                None => {
                    *output = Some(alias);
                    true
                }
            }
        }
        ScalarExpr::QualifiedColumn { qualifier, .. } => {
            if !aliases.contains(qualifier) {
                return false;
            }
            match output {
                Some(previous) => *previous == qualifier,
                None => {
                    *output = Some(qualifier);
                    true
                }
            }
        }
        ScalarExpr::Literal(_)
        | ScalarExpr::TypedLiteral { .. }
        | ScalarExpr::RetainedLiteral(_)
        | ScalarExpr::Param(_) => true,
        ScalarExpr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            args.iter().all(&mut recur)
                && order_by.iter().all(|order| recur(&order.expr))
                && filter.as_deref().is_none_or(recur)
        }
        ScalarExpr::Array(items)
        | ScalarExpr::Row(items)
        | ScalarExpr::CompositeRow { items, .. }
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => items.iter().all(recur),
        ScalarExpr::Binary { lhs, rhs, .. } => recur(lhs) && recur(rhs),
        ScalarExpr::UnaryMinus(inner)
        | ScalarExpr::Not(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. }
        | ScalarExpr::Collated { expr: inner, .. } => recur(inner),
        ScalarExpr::Between { expr, low, high } => recur(expr) && recur(low) && recur(high),
        ScalarExpr::InList { expr, list, .. } => recur(expr) && list.iter().all(recur),
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            base.as_deref().is_none_or(&mut recur)
                && when
                    .iter()
                    .all(|(condition, result)| recur(condition) && recur(result))
                && else_branch.as_deref().is_none_or(recur)
        }
        ScalarExpr::Default
        | ScalarExpr::Star
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::Position(_)
        | ScalarExpr::InternalColumn(_)
        | ScalarExpr::WindowCall { .. }
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. }
        | ScalarExpr::InSubquery { .. } => false,
    }
}
