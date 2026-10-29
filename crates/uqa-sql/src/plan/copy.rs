//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Destination admission for complete SQL syntax and executable query/mutation copies.

use super::{CommandPlan, ExpressionPlan, PlanRef, QueryPlan, UnifiedPlan};
use crate::{catalog::roles::RoleReference, SQLError, ScalarExpr};
use uqa_core::memory::{BudgetedVec, MemoryReservation, OwnedSet, Produced, ProductionControl};

mod command;
mod query;
mod source;
mod syntax;
#[cfg(test)]
mod tests;

type Result<T> = std::result::Result<T, SQLError>;

impl super::CtePlan {
    /// Copy a complete CTE declaration under the selected allowance and cancellation scopes.
    pub fn clone_with_control(&self, control: &ProductionControl<'_>) -> Result<Produced<Self>> {
        control.check()?;
        let Some(mut copying) = Copying::new(*control) else {
            return Ok(control.finish(self.clone(), None)?);
        };
        let plan = copying.cte(self)?;
        copying.finish(plan)
    }
}

impl QueryPlan {
    /// Copy a relational executable and its query/mutation CTEs, admitting destination capacities and owned payloads before allocation. Immutable analysis descriptors and retained row/literal identities remain shared.
    pub fn clone_with_control(&self, control: &ProductionControl<'_>) -> Result<Produced<Self>> {
        control.check()?;
        let Some(mut copying) = Copying::new(*control) else {
            return Ok(control.finish(self.clone(), None)?);
        };
        let plan = copying.query(self)?;
        copying.finish(plan)
    }
}

impl ExpressionPlan {
    /// Copy a scalar expression and its owned query arena under the same production allowance.
    pub fn clone_with_control(&self, control: &ProductionControl<'_>) -> Result<Produced<Self>> {
        control.check()?;
        let Some(mut copying) = Copying::new(*control) else {
            return Ok(control.finish(self.clone(), None)?);
        };
        let expression = copying.expression(self)?;
        copying.finish(expression)
    }
}

impl UnifiedPlan {
    /// Copy complete analyzed syntax, including utility commands, admitting each owned destination before construction. Transient bindings and shared immutable input owners preserve their original identities.
    pub fn clone_with_control(&self, control: &ProductionControl<'_>) -> Result<Produced<Self>> {
        control.check()?;
        let Some(mut copying) = Copying::new(*control) else {
            return Ok(control.finish(self.clone(), None)?);
        };
        let plan = syntax::SyntaxCopy::copy_syntax(self, &mut copying)?;
        copying.finish(plan)
    }

    /// Copy a query or INSERT/UPDATE/DELETE/MERGE executable. Utility commands have separate preparation and lifetime owners and are rejected by this executable-only operation.
    pub fn clone_executable_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>> {
        PlanRef::from(self).clone_executable_with_control(control)
    }
}

impl PlanRef<'_> {
    /// Copy a borrowed query or mutation directly into its admitted executable owner, without first constructing a temporary statement wrapper.
    pub fn clone_executable_with_control(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<UnifiedPlan>> {
        control.check()?;
        let Some(mut copying) = Copying::new(*control) else {
            if let Self::Command(command) = self {
                require_mutation(command)?;
            }
            return Ok(control.finish(self.to_owned(), None)?);
        };
        let plan = match self {
            Self::Query(query) => {
                UnifiedPlan::Query(copying.boxed(|copying| copying.query(query))?)
            }
            Self::Command(command) => {
                UnifiedPlan::Command(copying.boxed(|copying| copying.command(command))?)
            }
        };
        copying.finish(plan)
    }
}

fn require_mutation(command: &CommandPlan) -> Result<()> {
    if matches!(
        command,
        CommandPlan::Insert(_)
            | CommandPlan::Update(_)
            | CommandPlan::Delete(_)
            | CommandPlan::Merge(_)
    ) {
        Ok(())
    } else {
        Err(SQLError::Internal(
            "utility command has no query execution copy".into(),
        ))
    }
}

pub(crate) struct Copying<'a> {
    control: ProductionControl<'a>,
    memory: MemoryReservation,
}

impl<'a> Copying<'a> {
    fn new(control: ProductionControl<'a>) -> Option<Self> {
        control
            .empty_reservation()
            .map(|memory| Self { control, memory })
    }

    fn check(&self) -> Result<()> {
        Ok(self.control.check()?)
    }

    fn finish<T>(self, value: T) -> Result<Produced<T>> {
        Ok(self.control.finish(value, Some(self.memory))?)
    }

    fn take<T>(&mut self, output: Produced<T>) -> T {
        let (value, memory) = output.into_parts();
        self.memory.absorb(memory.expect("controlled plan copy"));
        value
    }

    pub(crate) fn map<I: ExactSizeIterator, T>(
        &mut self,
        items: I,
        mut copy: impl FnMut(&mut Self, I::Item) -> Result<T>,
    ) -> Result<Vec<T>> {
        self.check()?;
        let mut output = BudgetedVec::new(self.control.budget().expect("controlled plan copy"));
        output.reserve(items.len())?;
        for item in items {
            self.check()?;
            output.push(copy(self, item)?)?;
        }
        let (output, memory) = output.into_parts();
        self.memory.absorb(memory);
        Ok(output)
    }

    pub(crate) fn set<K: Ord>(
        &mut self,
        keys: &OwnedSet<K>,
        mut copy: impl FnMut(&mut Self, &K) -> Result<K>,
    ) -> Result<OwnedSet<K>> {
        let mut output = OwnedSet::new();
        for key in keys {
            self.check()?;
            self.memory.grow(OwnedSet::<K>::entry_bytes())?;
            output.insert(copy(self, key)?);
        }
        Ok(output)
    }

    fn boxed<T>(&mut self, copy: impl FnOnce(&mut Self) -> Result<T>) -> Result<Box<T>> {
        self.check()?;
        self.memory.grow(size_of::<T>())?;
        let value = copy(self)?;
        self.check()?;
        Ok(Box::new(value))
    }

    pub(crate) fn text(&mut self, value: &str) -> Result<String> {
        let output = self.control.copy_text(value)?;
        Ok(self.take(output))
    }

    fn optional_text(&mut self, value: Option<&str>) -> Result<Option<String>> {
        value.map(|value| self.text(value)).transpose()
    }

    fn texts(&mut self, values: &[String]) -> Result<Vec<String>> {
        self.map(values.iter(), |copying, value| copying.text(value))
    }

    fn scalar(&mut self, expression: &ScalarExpr) -> Result<ScalarExpr> {
        let output = expression.clone_with_control(&self.control)?;
        Ok(self.take(output))
    }

    fn optional_scalar(&mut self, expression: Option<&ScalarExpr>) -> Result<Option<ScalarExpr>> {
        expression
            .map(|expression| self.scalar(expression))
            .transpose()
    }

    fn scalar_box(&mut self, expression: &ScalarExpr) -> Result<Box<ScalarExpr>> {
        self.boxed(|copying| copying.scalar(expression))
    }

    fn optional_scalar_box(
        &mut self,
        expression: Option<&ScalarExpr>,
    ) -> Result<Option<Box<ScalarExpr>>> {
        expression
            .map(|expression| self.scalar_box(expression))
            .transpose()
    }

    fn expression(&mut self, expression: &ExpressionPlan) -> Result<ExpressionPlan> {
        Ok(ExpressionPlan {
            scalar: self.scalar(&expression.scalar)?,
            subqueries: self.map(expression.subqueries.iter(), Self::query)?,
        })
    }

    fn ty(&mut self, ty: &crate::ColumnType) -> Result<crate::ColumnType> {
        let output = ty.clone_with_control(&self.control)?;
        Ok(self.take(output))
    }

    fn role(&mut self, role: Option<&RoleReference>) -> Result<Option<RoleReference>> {
        role.map(|role| match role {
            RoleReference::Named(name) => Ok(RoleReference::Named(self.text(name)?)),
            RoleReference::Bound(binding) => {
                Ok(RoleReference::Bound(std::sync::Arc::clone(binding)))
            }
        })
        .transpose()
    }

    fn identity(&mut self, identity: &crate::ColumnIdentity) -> Result<crate::ColumnIdentity> {
        let output = identity.clone_with_control(&self.control)?;
        Ok(self.take(output))
    }
}
