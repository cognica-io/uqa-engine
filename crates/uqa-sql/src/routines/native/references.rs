//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Inspect the selected expression and table-function bindings in their original validation order.

use super::NativeRoutineReferences;
use crate::{
    ast::{Expr, FunctionBinding, Statement},
    binding::view_dependencies::{
        visit_table_function_calls_with_work, StoredTableFunctionObserver,
    },
    catalog::work::{inline, Work},
    construction::Read,
    ir::observation::{visit_all, ScalarObserver},
    plan::{PlanRef, StoredStatementObserver, UnifiedPlan},
    schema::dependencies::{observe_schema_expression, SchemaObserver},
    SQLError, ScalarExpr,
};
use std::future::Future;
use uqa_core::memory::ProductionControl;

/// One stored catalog field that can carry a concrete routine reference.
pub enum StoredRoutineReference<'a> {
    Expression(&'a Expr),
    Scalar(&'a ScalarExpr),
    Plan(PlanRef<'a>),
    Statement(&'a Statement),
    Binding(&'a FunctionBinding),
    Routine([u8; 16], &'a [String]),
    Trigger(&'a str, Option<[u8; 16]>),
}

type FieldRead<'a> = Read<'a, Result<(), SQLError>>;

fn select<'a, W: Work>(
    control: ProductionControl<'_>,
    future: impl Future<Output = Result<(), SQLError>> + Send + 'a,
) -> Result<Option<FieldRead<'a>>, SQLError> {
    if W::MAY_SUSPEND {
        Ok(Some(Read::new(control, future)?))
    } else {
        inline(future)?;
        Ok(None)
    }
}

impl NativeRoutineReferences<'_> {
    /// Only a suspending caller retains the workspace of its selected field.
    pub async fn inspect_with_work<W: Work + Send>(
        &self,
        field: StoredRoutineReference<'_>,
        work: &mut W,
    ) -> Result<(), SQLError> {
        use StoredRoutineReference as Field;
        let control = self.control;
        let selected = match field {
            Field::Expression(expr) => select::<W>(control, self.expression_with_work(expr, work)),
            Field::Scalar(expr) => select::<W>(control, self.scalar_with_work(expr, work)),
            Field::Plan(plan) => select::<W>(control, self.plan_with_work(plan, work)),
            Field::Statement(statement) => {
                select::<W>(control, self.statement_with_work(statement, work))
            }
            Field::Binding(binding) => select::<W>(control, self.binding_with_work(binding, work)),
            Field::Routine(id, args) => {
                select::<W>(control, self.identity_with_work(id, args, false, work))
            }
            Field::Trigger(name, id) => {
                select::<W>(control, self.trigger_with_work(name, id, work))
            }
        };
        if let Some(read) = selected? {
            read.await?;
        }
        Ok(())
    }

    pub fn expression(&self, expression: &Expr) -> Result<(), SQLError> {
        inline(self.expression_with_work(expression, &mut ()))
    }

    pub async fn expression_with_work(
        &self,
        expression: &Expr,
        work: &mut (impl Work + Send),
    ) -> Result<(), SQLError> {
        let mut observer = self;
        observe_schema_expression(expression, &self.control, work, &mut observer).await
    }

    pub fn scalar(&self, expression: &ScalarExpr) -> Result<(), SQLError> {
        inline(self.scalar_with_work(expression, &mut ()))
    }

    pub async fn scalar_with_work(
        &self,
        expression: &ScalarExpr,
        work: &mut (impl Work + Send),
    ) -> Result<(), SQLError> {
        let mut observer = self;
        visit_all(expression, &self.control, work, &mut observer).await
    }

    pub fn plan(&self, plan: PlanRef<'_>) -> Result<(), SQLError> {
        inline(self.plan_with_work(plan, &mut ()))
    }

    pub async fn plan_with_work(
        &self,
        plan: PlanRef<'_>,
        work: &mut (impl Work + Send),
    ) -> Result<(), SQLError> {
        let mut observer = self;
        crate::plan::visit_scalar_nodes_with_work(plan, &self.control, work, &mut observer).await?;
        visit_table_function_calls_with_work(plan, &self.control, work, &mut observer).await
    }

    /// Inspect the currently stored action and its concrete identities. Source routines retain their original lazy body.
    pub fn statement(&self, statement: &Statement) -> Result<(), SQLError> {
        inline(self.statement_with_work(statement, &mut ()))
    }

    pub async fn statement_with_work(
        &self,
        statement: &Statement,
        work: &mut (impl Work + Send),
    ) -> Result<(), SQLError> {
        let mut observer = self;
        UnifiedPlan::visit_stored_statement_with_work(statement, &self.control, work, &mut observer)
            .await
    }
}

impl SchemaObserver for &NativeRoutineReferences<'_> {
    async fn expression<W: Work + Send>(
        &mut self,
        value: &Expr,
        work: &mut W,
    ) -> Result<(), SQLError> {
        if let Expr::Func {
            binding: Some(binding),
            ..
        } = value
        {
            self.binding_with_work(binding, work).await?;
        }
        Ok(())
    }
}

impl ScalarObserver for &NativeRoutineReferences<'_> {
    async fn scalar<W: Work + Send>(
        &mut self,
        value: &ScalarExpr,
        work: &mut W,
    ) -> Result<(), SQLError> {
        if let ScalarExpr::Func {
            binding: Some(binding),
            ..
        } = value
        {
            self.binding_with_work(binding, work).await?;
        }
        Ok(())
    }
}

impl StoredTableFunctionObserver for &NativeRoutineReferences<'_> {
    async fn function<W: Work + Send>(
        &mut self,
        _: &str,
        binding: Option<&FunctionBinding>,
        work: &mut W,
    ) -> Result<(), SQLError> {
        if let Some(binding) = binding {
            self.binding_with_work(binding, work).await?;
        }
        Ok(())
    }
}

impl StoredStatementObserver for &NativeRoutineReferences<'_> {
    async fn plan<W: Work + Send>(
        &mut self,
        plan: PlanRef<'_>,
        _: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<(), SQLError> {
        self.plan_with_work(plan, work).await
    }
}
