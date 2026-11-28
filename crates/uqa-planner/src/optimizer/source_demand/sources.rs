//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Join demand retains the original right-before-left LATERAL observation order.

use super::{references, Context, Demand, SQLError};
use crate::{QueryPlan, SourcePlan};
use uqa_sql::plan::{AnalyzedSource, MutableQueryPlan, MutableSourcePlan, SourceChildSlot};

impl<'catalog> Context<'_, 'catalog> {
    pub(super) fn source_queries(
        &self,
        mut source: MutableSourcePlan,
    ) -> Result<MutableSourcePlan, SQLError> {
        self.config.production.check()?;
        match &*source {
            SourcePlan::Subquery { .. } => {
                source.map_query(&self.storage, |query| self.query(query))
            }
            SourcePlan::Table {
                name,
                executable_query: Some(_),
                ..
            } => {
                let context = Context {
                    ctes: self.referenced_cte(name).and(self.ctes),
                    ..*self
                };
                source.map_query(&self.storage, |query| context.query(query))
            }
            SourcePlan::Join { .. } => {
                source = source.map_child(SourceChildSlot::Left, &self.storage, |source| {
                    self.source_queries(source)
                })?;
                source.map_child(SourceChildSlot::Right, &self.storage, |source| {
                    self.source_queries(source)
                })
            }
            _ => Ok(source),
        }
    }

    pub(super) fn source(
        &self,
        source: MutableSourcePlan,
        analysis: &AnalyzedSource,
        demand: &Demand<'_>,
        subqueries: &[QueryPlan],
    ) -> Result<MutableSourcePlan, SQLError> {
        let control = &self.config.production;
        control.check()?;
        let cte = match &*source {
            SourcePlan::Table { name, .. } => self.referenced_cte(name),
            _ => None,
        };
        if let Some(selected) = cte {
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
        match &*source {
            SourcePlan::Subquery {
                captured_output, ..
            } => {
                let captured = *captured_output;
                source.map_query(&self.storage, |query| {
                    self.query(self.outputs(query, analysis, demand, captured)?)
                })
            }
            SourcePlan::Table {
                executable_query: Some(_),
                ..
            } => {
                let context = Context {
                    ctes: cte.and(self.ctes),
                    ..*self
                };
                source.map_query(&self.storage, |query| {
                    let query = context.outputs(query, analysis, demand, None)?;
                    context.query(query)
                })
            }
            SourcePlan::Join { .. } => self.join(source, analysis, demand, subqueries),
            _ => Ok(source),
        }
    }

    pub(super) fn outputs(
        &self,
        query: MutableQueryPlan,
        analysis: &AnalyzedSource,
        demand: &Demand<'_>,
        captured: Option<uqa_sql::ast::InternalRelationId>,
    ) -> Result<MutableQueryPlan, SQLError> {
        self.prune_positions(query, &|position| {
            captured
                .map_or_else(
                    || analysis.schema.slot(position),
                    |relation| analysis.schema.internal_slot(relation.column(position)),
                )
                .is_some_and(|slot| demand.contains(slot))
        })
    }

    fn join(
        &self,
        mut source: MutableSourcePlan,
        analysis: &AnalyzedSource,
        demanded: &Demand<'_>,
        subqueries: &[QueryPlan],
    ) -> Result<MutableSourcePlan, SQLError> {
        let Some(children) = analysis.children.as_deref() else {
            return Ok(source);
        };
        let (mut left_demand, right_demand) =
            self.join_demand(&source, children, demanded, subqueries)?;
        source = source.map_child(SourceChildSlot::Right, &self.storage, |source| {
            self.source(source, &children.1, &right_demand, subqueries)
        })?;
        let SourcePlan::Join { right, lateral, .. } = &*source else {
            unreachable!()
        };
        references::References::new(&children.0.schema, &mut left_demand).source(
            right,
            Some(&children.1),
            subqueries,
            &[],
            *lateral,
        )?;
        source.map_child(SourceChildSlot::Left, &self.storage, |source| {
            self.source(source, &children.0, &left_demand, subqueries)
        })
    }
    pub(super) fn join_demand(
        &self,
        source: &SourcePlan,
        children: &(AnalyzedSource, AnalyzedSource),
        demanded: &Demand<'_>,
        subqueries: &[QueryPlan],
    ) -> Result<(Demand<'catalog>, Demand<'catalog>), SQLError> {
        let SourcePlan::Join {
            on, using, natural, ..
        } = source
        else {
            unreachable!("join source")
        };
        let control = &self.config.production;
        let (left, right) = (&children.0.schema, &children.1.schema);
        let mut demand = demanded.copy()?;
        if let Some(on) = on {
            references::References::join(left, right, &mut demand)?.expression(on, subqueries)?;
        }
        if let Some(using) = uqa_sql::semantics::resolve_join_using_with_control(
            using.as_ref(),
            *natural,
            left,
            right,
            control,
        )? {
            using.visit_input_slots(left, right, control, &mut |slot| demand.extend([slot]))?;
        }
        let boundary = left.physical_width();
        let mut left_demand = Demand::new(*control);
        left_demand.extend(demand.iter().copied().filter(|slot| *slot < boundary))?;
        let mut right_demand = Demand::new(*control);
        right_demand.extend(demand.iter().filter_map(|slot| slot.checked_sub(boundary)))?;
        Ok((left_demand, right_demand))
    }
}
