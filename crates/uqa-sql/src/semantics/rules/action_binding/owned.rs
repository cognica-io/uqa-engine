//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bound action bodies and replaceable RETURNING lists retire their allocations independently.

use super::{
    bind_rule_statement_body, returning, RuleActionBodyResolver, RuleBindingContext,
    RuleBindingScope, RuleSourceCatalog, SyntaxProduction,
};
use crate::ast::Projection;
use crate::{
    ast::{RuleEvent, Statement},
    plpgsql::VariableResolver,
    SQLError,
};
use uqa_core::memory::{MemoryReservation, OwnedProductionControl, Produced, ProductionControl};

/// Only SQL-owned operations can replace a clause; callers borrow the complete statement without detaching its allocation owners.
pub struct BoundRuleStatement {
    statement: Statement,
    body_memory: Option<MemoryReservation>,
    returning_memory: Option<MemoryReservation>,
    control: OwnedProductionControl,
}

impl BoundRuleStatement {
    pub fn bind(
        catalog: &dyn RuleSourceCatalog,
        action: &Statement,
        action_columns: &dyn super::ActionColumns,
        resolver: &mut dyn VariableResolver,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        let context = RuleBindingContext::with_control(catalog, true, control);
        let body = SyntaxProduction::new(*control);
        let (returning, event, aliases) = match action {
            Statement::Insert(action) => (
                &action.returning,
                RuleEvent::Insert,
                &action.returning_aliases,
            ),
            Statement::Update(action) => (
                &action.returning,
                RuleEvent::Update,
                &action.returning_aliases,
            ),
            Statement::Delete(action) => (
                &action.returning,
                RuleEvent::Delete,
                &action.returning_aliases,
            ),
            _ => {
                let statement = bind_rule_statement_body(
                    action,
                    resolver,
                    &RuleBindingScope::default(),
                    &context,
                    &body,
                )?;
                let (statement, body_memory) = body.finish(statement)?.into_parts();
                return Ok(Self {
                    statement,
                    body_memory,
                    returning_memory: control.empty_reservation(),
                    control: control.capture(),
                });
            }
        };
        let statement = bind_rule_statement_body(
            action,
            &mut RuleActionBodyResolver {
                action_columns,
                action_event: event,
                event_resolver: resolver,
            },
            &RuleBindingScope::default(),
            &context,
            &body,
        )?;
        let body = body.finish(statement)?;
        let output = SyntaxProduction::new(*control);
        let projections = returning::bind_rule_action_returning(
            returning,
            action_columns,
            aliases,
            event,
            resolver,
            &context,
            &output,
        )?;
        let projections = output.finish(projections)?;
        let (mut statement, body_memory) = body.into_parts();
        let (projections, returning_memory) = projections.into_parts();
        *returning_mut(&mut statement).expect("bound DML action") = projections;
        Ok(Self {
            statement,
            body_memory,
            returning_memory,
            control: control.capture(),
        })
    }

    pub fn statement(&self) -> &Statement {
        &self.statement
    }

    pub fn reserved_bytes(&self) -> usize {
        self.body_memory
            .as_ref()
            .map_or(0, MemoryReservation::bytes)
            + self
                .returning_memory
                .as_ref()
                .map_or(0, MemoryReservation::bytes)
    }

    pub fn into_produced(self) -> Result<Produced<Statement>, SQLError> {
        let control = self.control.control();
        Ok(control.finish(
            self.statement,
            control.combine(self.body_memory, self.returning_memory),
        )?)
    }

    pub fn clear_returning(&mut self) -> Result<(), SQLError> {
        self.control.control().check()?;
        if let Some(returning) = returning_mut(&mut self.statement) {
            drop(std::mem::take(returning));
            self.returning_memory = self.control.control().empty_reservation();
        }
        Ok(())
    }

    /// Construct all requested images before replacing the old list. Rejection or cancellation leaves the original statement and its leases intact.
    pub fn augment_returning(
        &mut self,
        source_index: Option<&crate::ast::Expr>,
        event_width: usize,
        request: crate::semantics::rules::returning::RuleReturningRequest,
        target_columns: &dyn super::ActionColumns,
    ) -> Result<(), SQLError> {
        if let Some(output) = crate::semantics::rules::returning::produce_rule_returning(
            &self.statement,
            source_index,
            event_width,
            request,
            target_columns,
            &self.control.control(),
        )? {
            self.replace_returning(output);
        }
        Ok(())
    }

    pub fn append_returning_context(
        &mut self,
        expressions: &[crate::ast::Expr],
    ) -> Result<(), SQLError> {
        let control = self.control.control();
        control.check()?;
        if expressions.is_empty() {
            return Ok(());
        }
        let returning = match &self.statement {
            Statement::Update(action) => &action.returning,
            Statement::Delete(action) => &action.returning,
            _ => {
                return Err(SQLError::Internal(
                    "rule source context requires UPDATE or DELETE".into(),
                ))
            }
        };
        let production = SyntaxProduction::new(control);
        let mut output = production.vector(returning.len().saturating_add(expressions.len()))?;
        for projection in returning {
            output.push(production.copy(projection)?)?;
        }
        for expression in expressions {
            output.push(Projection {
                expr: production.copy(expression)?,
                alias: None,
            })?;
        }
        let output = output.finish()?;
        let output = production.finish(output)?;
        self.replace_returning(output);
        Ok(())
    }

    fn replace_returning(&mut self, output: Produced<Vec<Projection>>) {
        let (value, memory) = output.into_parts();
        let slot =
            returning_mut(&mut self.statement).expect("RETURNING producer validated DML action");
        drop(std::mem::replace(slot, value));
        self.returning_memory = memory;
    }
}

pub(crate) fn returning_mut(statement: &mut Statement) -> Option<&mut Vec<Projection>> {
    match statement {
        Statement::Insert(action) => Some(&mut action.returning),
        Statement::Update(action) => Some(&mut action.returning),
        Statement::Delete(action) => Some(&mut action.returning),
        _ => None,
    }
}
