//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Only canonical generated-name suffixes can collide; all other names and complete query payloads remain borrowed.

use crate::{
    plan::{CtePlanBody, QueryPlan, RelationalPlan, SourcePlan},
    SQLError,
};
use uqa_core::memory::{Produced, ProductionControl, ProductionMap};

const PREFIX: &str = "__uqa_view_subquery_local_";

pub(super) fn fresh(
    query: &QueryPlan,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, SQLError> {
    control.check()?;
    let mut names = Names {
        indices: ProductionMap::new(*control),
        control: *control,
    };
    names.query(query)?;
    query.try_visit_scalar_expressions(&mut |expression| {
        expression.try_visit(&mut |node| {
            names.control.check()?;
            if let Some(qualifier) = node.reference_qualifier() {
                names.insert(qualifier)?;
            }
            Ok::<_, SQLError>(true)
        })
    })?;
    // The first missing nonnegative integer is at most the number of distinct retained suffixes.
    let index = (0..=names.indices.len())
        .find(|index| names.indices.get(index).is_none())
        .expect("N generated suffixes cannot cover N+1 candidates");
    drop(names);
    Ok(control.format(format_args!("{PREFIX}{index}"))?)
}

struct Names<'a> {
    indices: ProductionMap<'a, usize, ()>,
    control: ProductionControl<'a>,
}

impl Names<'_> {
    fn insert(&mut self, name: &str) -> Result<(), SQLError> {
        self.control.check()?;
        let Some(suffix) = name.strip_prefix(PREFIX) else {
            return Ok(());
        };
        // Decimal output never has a sign or leading zero; long or noncanonical identifiers cannot collide.
        if suffix.is_empty()
            || suffix.len() > usize::MAX.ilog10() as usize + 1
            || suffix.len() > 1 && suffix.starts_with('0')
            || !suffix.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Ok(());
        }
        if let Ok(index) = suffix.parse::<usize>() {
            self.indices.insert_copy(index, ())?;
        }
        Ok(())
    }

    fn cte(&mut self, body: &CtePlanBody) -> Result<(), SQLError> {
        self.control.check()?;
        match body {
            CtePlanBody::Query(query) => self.query(query)?,
            CtePlanBody::Command(command) => {
                if let Some(qualifier) = command.target_qualifier() {
                    self.insert(qualifier)?;
                }
                for cte in command.ctes() {
                    self.insert(&cte.name)?;
                    self.cte(&cte.body)?;
                }
                for query in command.query_inputs() {
                    self.query(query)?;
                }
                if let Some(source) = command.source_input() {
                    self.source(source)?;
                }
            }
        }
        Ok(())
    }

    fn query(&mut self, query: &QueryPlan) -> Result<(), SQLError> {
        self.control.check()?;
        for cte in &query.ctes {
            self.insert(&cte.name)?;
            self.cte(&cte.body)?;
        }
        let subqueries = match &query.root {
            RelationalPlan::QueryBlock(block) => {
                if let Some(source) = &block.from {
                    self.source(source)?;
                }
                &block.subqueries
            }
            RelationalPlan::SetOp {
                left,
                right,
                subqueries,
                ..
            } => {
                self.query(left)?;
                self.query(right)?;
                subqueries
            }
            RelationalPlan::Values { subqueries, .. } => subqueries,
        };
        for query in subqueries {
            self.query(query)?;
        }
        Ok(())
    }

    fn source(&mut self, source: &SourcePlan) -> Result<(), SQLError> {
        self.control.check()?;
        if let Some(qualifier) = source.visible_qualifier() {
            self.insert(qualifier)?;
        }
        match source {
            SourcePlan::Join { left, right, .. } => {
                self.source(left)?;
                self.source(right)?;
            }
            SourcePlan::Subquery { body, .. } => self.query(body)?,
            SourcePlan::RetainedRows(_)
            | SourcePlan::Table { .. }
            | SourcePlan::Values { .. }
            | SourcePlan::Function { .. }
            | SourcePlan::FunctionGroup { .. } => {}
        }
        Ok(())
    }
}
