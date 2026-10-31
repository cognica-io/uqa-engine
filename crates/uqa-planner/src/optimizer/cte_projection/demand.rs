//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrow CTE output names while collecting consumer demand under the invoking allowance.

use crate::{QueryBlockPlan, QueryPlan, RelationalPlan, SourcePlan};
use uqa_core::{
    memory::{ProductionControl, ProductionMap},
    RelationIdentity, RelationName,
};
use uqa_sql::{semantics::ProjectionLabels, SQLError, ScalarExpr};

pub(super) struct Demand<'n, 'c> {
    name: &'n str,
    names: &'n ProjectionLabels<'n>,
    required: ProductionMap<'c, &'n str, ()>,
    pub all: bool,
    control: ProductionControl<'c>,
}

impl<'n, 'c> Demand<'n, 'c> {
    pub fn new(
        name: &'n str,
        names: &'n ProjectionLabels<'n>,
        control: ProductionControl<'c>,
    ) -> Self {
        Self {
            name,
            names,
            required: ProductionMap::new(control),
            all: false,
            control,
        }
    }

    pub fn required(&self, name: &str) -> bool {
        self.required.get(name).is_some()
    }

    pub fn collect_query(&mut self, query: &QueryPlan) -> Result<(), SQLError> {
        self.control.check()?;
        for cte in &query.ctes {
            self.control.check()?;
            if cte.name == self.name {
                return Ok(());
            }
        }
        self.collect_root(query)?;
        for cte in &query.ctes {
            self.control.check()?;
            if let Some(query) = cte.body.query() {
                self.collect_query(query)?;
            } else {
                self.all = true;
            }
        }
        Ok(())
    }

    pub fn collect_root(&mut self, query: &QueryPlan) -> Result<(), SQLError> {
        self.control.check()?;
        match &query.root {
            RelationalPlan::QueryBlock(block) => {
                if let Some(source) = &block.from {
                    self.collect_source(source, block)?;
                }
                for query in &block.subqueries {
                    self.collect_query(query)?;
                }
            }
            RelationalPlan::SetOp {
                left,
                right,
                subqueries,
                ..
            } => {
                self.collect_query(left)?;
                self.collect_query(right)?;
                for query in subqueries {
                    self.collect_query(query)?;
                }
            }
            RelationalPlan::Values { subqueries, .. } => {
                for query in subqueries {
                    self.collect_query(query)?;
                }
            }
        }
        Ok(())
    }

    fn collect_source(
        &mut self,
        source: &SourcePlan,
        block: &QueryBlockPlan,
    ) -> Result<(), SQLError> {
        self.control.check()?;
        match source {
            SourcePlan::Table {
                name: reference,
                qualifier,
                alias,
                column_aliases,
                ..
            } if RelationIdentity::borrowed_reference(reference).is_some_and(|reference| {
                reference.schema.is_none() && reference.name == RelationName::decoded(self.name)
            }) =>
            {
                let qualifier = alias.as_deref().unwrap_or(qualifier);
                if !block.subqueries.is_empty() {
                    self.all = true;
                }
                block.try_visit_expressions(&mut |expression| {
                    super::super::output_demand::try_visit(
                        expression,
                        &self.control,
                        &mut |part| {
                            let column = match part {
                                ScalarExpr::Column(column) => Some(column),
                                ScalarExpr::QualifiedColumn {
                                    qualifier: selected,
                                    column,
                                } if selected == qualifier => Some(column),
                                ScalarExpr::QualifiedStar(selected) if selected == qualifier => {
                                    self.all = true;
                                    None
                                }
                                ScalarExpr::Star | ScalarExpr::Position(_) => {
                                    self.all = true;
                                    None
                                }
                                _ => None,
                            };
                            if let Some(column) = column {
                                let mut matched = false;
                                for (position, original) in self.names.iter().enumerate() {
                                    self.control.check()?;
                                    let selected = column_aliases
                                        .get(position)
                                        .map_or(original, String::as_str);
                                    if selected == column.as_str() {
                                        matched = true;
                                        self.required.insert_copy(original, ())?;
                                    }
                                }
                                if matches!(part, ScalarExpr::Column(_))
                                    && column == qualifier
                                    && !matched
                                {
                                    self.all = true;
                                }
                            }
                            Ok(())
                        },
                    )?;
                    Ok::<_, SQLError>(true)
                })?;
            }
            SourcePlan::Join {
                left,
                right,
                alias,
                on,
                using,
                natural,
                ..
            } => {
                if alias.is_some() || on.is_some() || using.is_some() || *natural {
                    self.all = true;
                }
                self.collect_source(left, block)?;
                self.collect_source(right, block)?;
            }
            SourcePlan::Subquery { body, .. } => self.collect_query(body)?,
            _ => {}
        }
        Ok(())
    }
}
