//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Join demand retains the original right-before-left LATERAL observation order.

use super::{references, Context, Demand, SQLError};
use crate::{QueryPlan, SourcePlan};
use uqa_sql::plan::{AnalyzedSource, MutableQueryPlan, MutableSourcePlan, SourceChildSlot};

impl Context<'_, '_> {
    pub(super) fn source_queries_owned(
        &self,
        mut source: MutableSourcePlan,
    ) -> Result<MutableSourcePlan, SQLError> {
        let control = &self.config.production;
        match &*source {
            SourcePlan::Subquery { .. } => {
                source.map_query(control, |query| self.query_owned(query))
            }
            SourcePlan::Table {
                name,
                executable_query: Some(_),
                ..
            } => {
                let inherited = self.referenced_cte(name).is_some();
                source.map_query(control, |query| {
                    if inherited {
                        self.query_owned(query)
                    } else {
                        Context {
                            config: self.config,
                            aggregates: self.aggregates,
                            ctes: None,
                        }
                        .query_owned(query)
                    }
                })
            }
            SourcePlan::Join { .. } => {
                source = source.map_child(SourceChildSlot::Left, control, |source| {
                    self.source_queries_owned(source)
                })?;
                source.map_child(SourceChildSlot::Right, control, |source| {
                    self.source_queries_owned(source)
                })
            }
            _ => Ok(source),
        }
    }

    pub(super) fn source_owned(
        &self,
        source: MutableSourcePlan,
        analysis: &AnalyzedSource,
        demand: &Demand<'_>,
        subqueries: &[QueryPlan],
    ) -> Result<MutableSourcePlan, SQLError> {
        let control = &self.config.production;
        control.check()?;
        if let SourcePlan::Table { name, .. } = &*source {
            if let Some(selected) = self.referenced_cte(name) {
                if let Some(positions) = &mut *selected.positions.borrow_mut() {
                    selected.observed.set(true);
                    positions.extend((0..analysis.schema.len()).filter(|position| {
                        analysis
                            .schema
                            .slot(*position)
                            .is_some_and(|slot| demand.contains(slot))
                    }))?;
                }
            }
        }
        match &*source {
            SourcePlan::Subquery {
                captured_output, ..
            } => {
                let captured = *captured_output;
                source.map_query(control, |query| {
                    self.query_owned(self.outputs_owned(query, analysis, demand, captured)?)
                })
            }
            SourcePlan::Table {
                name,
                executable_query: Some(_),
                ..
            } => {
                let inherited = self.referenced_cte(name).is_some();
                source.map_query(control, |query| {
                    if inherited {
                        self.query_owned(self.outputs_owned(query, analysis, demand, None)?)
                    } else {
                        let context = Context {
                            config: self.config,
                            aggregates: self.aggregates,
                            ctes: None,
                        };
                        context.query_owned(context.outputs_owned(query, analysis, demand, None)?)
                    }
                })
            }
            SourcePlan::Join { .. } => self.join_owned(source, analysis, demand, subqueries),
            _ => Ok(source),
        }
    }

    fn outputs_owned(
        &self,
        query: MutableQueryPlan,
        analysis: &AnalyzedSource,
        demand: &Demand<'_>,
        captured: Option<uqa_sql::ast::InternalRelationId>,
    ) -> Result<MutableQueryPlan, SQLError> {
        super::super::super::derived_projection::owned::prune_positions(
            query,
            &|position| {
                captured
                    .map_or_else(
                        || analysis.schema.slot(position),
                        |relation| analysis.schema.internal_slot(relation.column(position)),
                    )
                    .is_some_and(|slot| demand.contains(slot))
            },
            None,
            self.output_context(&|name| self.visible_cte(name)),
        )
    }

    fn join_owned(
        &self,
        mut source: MutableSourcePlan,
        analysis: &AnalyzedSource,
        demanded: &Demand<'_>,
        subqueries: &[QueryPlan],
    ) -> Result<MutableSourcePlan, SQLError> {
        let SourcePlan::Join {
            on,
            using,
            natural,
            lateral,
            ..
        } = &*source
        else {
            unreachable!()
        };
        let Some(children) = &analysis.children else {
            return Ok(source);
        };
        let control = &self.config.production;
        let lateral = *lateral;
        let (left_schema, right_schema) = (&children.0.schema, &children.1.schema);
        let mut demand = demanded.copy()?;
        if let Some(on) = on {
            references::References::join(left_schema, right_schema, &mut demand)?
                .expression(on, subqueries)?;
        }
        if let Some(using) = uqa_sql::semantics::resolve_join_using_with_control(
            using.as_ref(),
            *natural,
            left_schema,
            right_schema,
            control,
        )? {
            using.visit_input_slots(left_schema, right_schema, control, &mut |slot| {
                demand.extend([slot])
            })?;
        }
        let boundary = left_schema.physical_width();
        let mut left_demand = Demand::new(*control);
        left_demand.extend(demand.iter().copied().filter(|slot| *slot < boundary))?;
        let mut right_demand = Demand::new(*control);
        right_demand.extend(demand.iter().filter_map(|slot| slot.checked_sub(boundary)))?;
        source = source.map_child(SourceChildSlot::Right, control, |source| {
            self.source_owned(source, &children.1, &right_demand, subqueries)
        })?;
        let SourcePlan::Join { right, .. } = &*source else {
            unreachable!()
        };
        references::References::new(left_schema, &mut left_demand).source(
            right,
            Some(&children.1),
            subqueries,
            &[],
            lateral,
        )?;
        source.map_child(SourceChildSlot::Left, control, |source| {
            self.source_owned(source, &children.0, &left_demand, subqueries)
        })
    }
}
