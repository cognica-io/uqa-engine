//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relation and source-routine dependencies inside WITH definitions.

use super::{
    collect_canonical_relation, collect_query_relation_dependencies,
    collect_query_source_routine_dependencies, collect_source_relation_dependencies,
    collect_source_routine_dependencies, BTreeSet, RuleDependencies, SQLError,
};

pub(super) fn collect_cte_relation_dependencies(
    body: &uqa_planner::CtePlanBody,
    dependencies: &mut RuleDependencies,
    inherited: &BTreeSet<String>,
) -> Result<(), SQLError> {
    match body {
        uqa_planner::CtePlanBody::Query(query) => {
            collect_query_relation_dependencies(query, dependencies, inherited)
        }
        uqa_planner::CtePlanBody::Command(command) => {
            if let Some(target) = command.mutation_target() {
                collect_canonical_relation(target, dependencies)?;
            }
            let mut visible = inherited.clone();
            if command.ctes().iter().any(|cte| cte.recursive) {
                visible.extend(command.ctes().iter().map(|cte| cte.name.clone()));
            }
            for cte in command.ctes() {
                collect_cte_relation_dependencies(&cte.body, dependencies, &visible)?;
                visible.insert(cte.name.clone());
            }
            for query in command.query_inputs() {
                collect_query_relation_dependencies(query, dependencies, &visible)?;
            }
            if let Some(source) = command.source_input() {
                collect_source_relation_dependencies(source, dependencies, &visible)?;
            }
            Ok(())
        }
    }
}

pub(super) fn collect_cte_source_routine_dependencies(
    body: &uqa_planner::CtePlanBody,
    dependencies: &mut RuleDependencies,
) {
    match body {
        uqa_planner::CtePlanBody::Query(query) => {
            collect_query_source_routine_dependencies(query, dependencies);
        }
        uqa_planner::CtePlanBody::Command(command) => {
            for cte in command.ctes() {
                collect_cte_source_routine_dependencies(&cte.body, dependencies);
            }
            for query in command.query_inputs() {
                collect_query_source_routine_dependencies(query, dependencies);
            }
            if let Some(source) = command.source_input() {
                collect_source_routine_dependencies(source, dependencies);
            }
        }
    }
}
