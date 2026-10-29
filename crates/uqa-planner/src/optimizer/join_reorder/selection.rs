//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared borrowed search selects the same graph before either ordinary or mutable owners move sources.

use super::{
    estimate_local_filters, flatten_reorderable_inner_join, implied_join_predicates,
    join_predicate, ColumnOwners, JoinOrderOptimizer, OptimizerResult, Produced, ProductionControl,
    ProductionSmallVec, ProductionVec, RelationMetadata, ScalarExpr, SourcePlan, SourceStatistics,
    DEFAULT_JOIN_CARDINALITY,
};

pub(super) struct SelectedGraph<'a> {
    pub tree: Produced<crate::JoinPlan>,
    pub masks: Produced<Vec<u64>>,
    pub inferred: Produced<Vec<&'a ScalarExpr>>,
    pub original_count: usize,
}

pub(super) fn select_graph<'a>(
    source: &SourcePlan,
    external_predicates: &[&'a ScalarExpr],
    statistics: &dyn SourceStatistics,
    control: &ProductionControl<'_>,
) -> OptimizerResult<Option<SelectedGraph<'a>>> {
    let mut atoms = ProductionSmallVec::<[&SourcePlan; 64]>::new(*control);
    let mut expressions = ProductionVec::new(*control);
    if !flatten_reorderable_inner_join(source, &mut atoms, &mut expressions, control)?
        || !(2..=64).contains(&atoms.len())
    {
        return Ok(None);
    }

    let Some((mut relation_storage, relation_tables)) = relation_metadata(&atoms, statistics)
    else {
        return Ok(None);
    };
    let relations = &mut relation_storage[..atoms.len()];
    let column_owners = ColumnOwners::new(relations);
    let local_estimates = estimate_local_filters(
        relations,
        external_predicates,
        &column_owners,
        &relation_tables,
        statistics,
        control,
    )?;

    // SQL's comma-separated FROM form lowers to a tree of cross joins while
    // its join equalities remain in the query-block WHERE expression. Treat
    // only top-level conjunctive column equalities as join-graph edges. The
    // WHERE expression stays in place as a post-join semantic guard; adding
    // the same equality to the hash join changes evaluation shape, not rows.
    let mut inferred = ProductionVec::new(*control);
    for expression in external_predicates {
        let decisions = implied_join_predicates(expression, &column_owners, control)?;
        for (_, decision) in &decisions {
            inferred
                .push_copy(*decision)
                .map_err(uqa_sql::SQLError::from)?;
        }
    }

    let original_count = expressions.len();
    let count = original_count
        .checked_add(inferred.len())
        .ok_or(uqa_core::memory::MemoryError::SizeOverflow)
        .map_err(uqa_sql::SQLError::from)?;
    let mut masks = ProductionVec::new(*control);
    masks.reserve(count).map_err(uqa_sql::SQLError::from)?;
    let mut graph_predicates = ProductionVec::new(*control);
    for expression in expressions.iter().copied().chain(inferred.iter().copied()) {
        control.check().map_err(uqa_sql::SQLError::from)?;
        let graph_predicate = join_predicate(expression, &column_owners);
        let mut relations = 0;
        if let Some(predicate) = graph_predicate {
            for alias in [predicate.left_alias, predicate.right_alias] {
                relations |= 1_u64
                    << column_owners
                        .alias_index(alias)
                        .expect("resolved join alias");
            }
            graph_predicates
                .push_copy(predicate)
                .map_err(uqa_sql::SQLError::from)?;
        }
        masks
            .push_copy(relations)
            .map_err(uqa_sql::SQLError::from)?;
    }

    let selected = JoinOrderOptimizer::new().select_relations_with_control(
        relations.iter().enumerate().map(|(index, relation)| {
            let mut input = relation.as_join_relation();
            if let Some(estimate) = local_estimates[index] {
                input.cardinality = estimate.output_rows;
                input.access_cost = estimate.cost;
            }
            input
        }),
        graph_predicates.iter().copied(),
        control,
    )?;
    Ok(Some(SelectedGraph {
        tree: selected,
        masks: masks.finish().map_err(uqa_sql::SQLError::from)?,
        inferred: inferred.finish().map_err(uqa_sql::SQLError::from)?,
        original_count,
    }))
}

fn relation_metadata<'a>(
    atoms: &[&'a SourcePlan],
    statistics: &dyn SourceStatistics,
) -> Option<([RelationMetadata<'a>; 64], [Option<&'a str>; 64])> {
    let mut relation_storage: [RelationMetadata<'_>; 64] =
        std::array::from_fn(|_| RelationMetadata::default());
    let relations = &mut relation_storage[..atoms.len()];
    let mut relation_tables = [None; 64];
    for (source_id, atom) in atoms.iter().enumerate() {
        let (qualifier, cardinality, column_stats, access_cost, paradigm) = match atom {
            SourcePlan::RetainedRows(_) => return None,
            SourcePlan::Table {
                name,
                qualifier,
                alias,
                ..
            } => {
                let qualifier = alias.as_deref().unwrap_or(qualifier);
                relation_tables[source_id] = Some(name.as_str());
                let relation_stats = statistics.relation_statistics(name);
                let cardinality = relation_stats
                    .as_ref()
                    .map_or(DEFAULT_JOIN_CARDINALITY, |stats| stats.row_count)
                    as f64;
                let column_stats = relation_stats
                    .map_or_else(crate::ColumnStatistics::default, |stats| stats.columns);
                let access_cost = crate::CostEstimator::default()
                    .estimate_unary(crate::OperatorKind::TableScan, cardinality)
                    .total();
                (
                    qualifier,
                    cardinality,
                    column_stats,
                    access_cost,
                    crate::AccessParadigm::Relational,
                )
            }
            SourcePlan::Function { .. } => {
                let Some(estimate) = statistics.source_access_estimate(atom) else {
                    return None;
                };
                (
                    atom.visible_qualifier()
                        .expect("a function source always has a visible qualifier"),
                    estimate.output_rows,
                    crate::ColumnStatistics::default(),
                    estimate.cost,
                    estimate.paradigm,
                )
            }
            SourcePlan::Join { .. }
            | SourcePlan::Values { .. }
            | SourcePlan::FunctionGroup { .. }
            | SourcePlan::Subquery { .. } => return None,
        };
        if qualifier.is_empty()
            || relations[..source_id]
                .iter()
                .any(|relation| relation.alias == qualifier)
        {
            return None;
        }
        relations[source_id] = RelationMetadata {
            alias: qualifier.into(),
            cardinality,
            column_stats,
            access_cost,
            paradigm,
        };
    }
    Some((relation_storage, relation_tables))
}
