//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read-only parent observations remain available while a child owns its moved payload.

use super::{
    memory::RewriteMemory, sources::query_source_memory, MutableQueryPlan, MutableSourcePlan,
    OwnedScalarExpr, QueryChildSlot, QueryPlan, QueryScalarSlot, RelationalPlan,
};
use crate::{SQLError, ScalarExpr};
use uqa_core::{memory::ProductionControl, Value};

impl MutableQueryPlan {
    /// Rewrite a scalar with a read-only view of the enclosing query. The selected parent slot temporarily contains an allocation-free NULL; the callback receives its actual previous scalar and owner separately.
    pub fn map_scalar_with_parent(
        mut self,
        slot: QueryScalarSlot,
        control: &ProductionControl<'_>,
        map: impl FnOnce(&QueryPlan, OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let Some(expression) = slot.expression_mut(&mut self.query) else {
            return Ok(self);
        };
        let memory = self
            .memory
            .as_mut()
            .and_then(|memory| slot.memory_mut(memory).take())
            .map(|memory| match memory {
                RewriteMemory::Scalar(memory) => memory,
                RewriteMemory::Fields { .. } => unreachable!("scalar owner"),
            });
        let input = OwnedScalarExpr::from_rewrite_parts(
            std::mem::replace(expression, ScalarExpr::Literal(Value::Null)),
            memory,
        );
        let output = input.rewrite(control, |input| map(&self.query, input))?;
        let (expression, memory) = output.into_rewrite_parts();
        *slot
            .expression_mut(&mut self.query)
            .expect("original scalar slot") = expression;
        if let Some(owner) = &mut self.memory {
            *slot.memory_mut(owner) = memory.map(RewriteMemory::Scalar);
        }
        Ok(self)
    }

    /// Observe an enclosing query's namespace while rewriting one directly owned child. Its original box or arena entry stays allocated in the parent and holds an empty query until the callback returns.
    pub fn map_query_child_with_parent(
        mut self,
        slot: QueryChildSlot,
        control: &ProductionControl<'_>,
        map: impl FnOnce(&QueryPlan, Self) -> Result<Self, SQLError>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let input = Self {
            query: std::mem::replace(
                slot.query_mut(&mut self.query),
                super::queries::empty_query(),
            ),
            memory: self
                .memory
                .as_mut()
                .and_then(|memory| slot.memory_mut(memory).take()),
        };
        let output = map(&self.query, input)?;
        output.assert_owner(control);
        control.check()?;
        *slot.query_mut(&mut self.query) = output.query;
        if let Some(memory) = &mut self.memory {
            *slot.memory_mut(memory) = output.memory;
        }
        Ok(self)
    }

    /// Observe the query's scalar and analysis inputs while rewriting its source. FROM is temporarily absent; its complete original source is moved into the callback.
    pub fn map_source_with_parent(
        mut self,
        control: &ProductionControl<'_>,
        map: impl FnOnce(&QueryPlan, MutableSourcePlan) -> Result<MutableSourcePlan, SQLError>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let RelationalPlan::QueryBlock(block) = &mut self.query.root else {
            return Err(SQLError::Internal(
                "source rewrite requires a query block".into(),
            ));
        };
        let Some(source) = block.from.take() else {
            return Ok(self);
        };
        let memory = self
            .memory
            .as_mut()
            .and_then(|memory| query_source_memory(memory).take());
        let output = map(&self.query, MutableSourcePlan { source, memory })?;
        output.assert_owner(control);
        control.check()?;
        let RelationalPlan::QueryBlock(block) = &mut self.query.root else {
            unreachable!("read-only parent cannot change topology");
        };
        block.from = Some(output.source);
        if let Some(memory) = &mut self.memory {
            *query_source_memory(memory) = output.memory;
        }
        Ok(self)
    }
}
