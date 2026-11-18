//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Owned relation binding reuses scoped plan traversal and existing source-field replacements.

use crate::{
    plan::{
        MutableCommandPlan, MutableExecutablePlan, MutableQueryPlan, MutableSourcePlan,
        OwnedScopedPlanVisitor, SourcePlan,
    },
    SQLError,
};
use uqa_core::memory::{Produced, ProductionControl};

type Resolver<'a> =
    dyn FnMut(&str, bool, &ProductionControl<'_>) -> Result<Produced<String>, SQLError> + 'a;

pub(crate) fn bind_query_relation_targets_owned(
    query: MutableQueryPlan,
    inherited: &dyn Fn(&str) -> bool,
    bind_mutations: bool,
    control: &ProductionControl<'_>,
    resolve: &mut Resolver<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    struct Binding<'a, 'r, 'c> {
        resolve: &'a mut Resolver<'r>,
        control: ProductionControl<'c>,
        bind_mutations: bool,
    }
    impl OwnedScopedPlanVisitor for Binding<'_, '_, '_> {
        fn source(
            &mut self,
            source: MutableSourcePlan,
            visible: &dyn Fn(&str) -> bool,
        ) -> Result<(MutableSourcePlan, bool), SQLError> {
            let source = bind_source(source, visible, self.resolve, &self.control)?;
            let descend = !matches!(&*source, SourcePlan::Table { .. });
            Ok((source, descend))
        }
        fn query_complete(
            &mut self,
            query: MutableQueryPlan,
        ) -> Result<MutableQueryPlan, SQLError> {
            query.set_relations_bound(true, &self.control)
        }
        fn command(
            &mut self,
            command: MutableCommandPlan,
            outer: &dyn Fn(&str) -> bool,
        ) -> Result<(MutableCommandPlan, bool), SQLError> {
            if !self.bind_mutations {
                let command = command.map_queries_while(&self.control, &mut |query| {
                    let query = bind_query_relation_targets_owned(
                        query,
                        outer,
                        false,
                        &self.control,
                        self.resolve,
                    )?;
                    Ok((query, false))
                })?;
                return Ok((command, false));
            }
            let mut command = command;
            let mut input = fields::Immediate {
                visible: outer,
                resolve: self.resolve,
                work: (),
            };
            crate::catalog::work::inline(fields::command(&mut command, &self.control, &mut input))?;
            Ok((command, true))
        }
    }
    let mut binding = Binding {
        resolve,
        control: *control,
        bind_mutations,
    };
    let MutableExecutablePlan::Query(query) =
        MutableExecutablePlan::Query(query).visit_scoped_in(control, inherited, &mut binding)?
    else {
        unreachable!("query binding preserves its root");
    };
    Ok(query)
}

pub(crate) mod fields;

fn bind_source(
    mut source: MutableSourcePlan,
    visible: &dyn Fn(&str) -> bool,
    resolve: &mut Resolver<'_>,
    control: &ProductionControl<'_>,
) -> Result<MutableSourcePlan, SQLError> {
    let mut input = fields::Immediate {
        visible,
        resolve,
        work: (),
    };
    crate::catalog::work::inline(fields::source(&mut source, control, &mut input))?;
    Ok(source)
}
