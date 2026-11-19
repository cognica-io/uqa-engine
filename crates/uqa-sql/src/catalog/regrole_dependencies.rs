//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` dependency restrictions for stored `regrole` constants.

use crate::{
    ast::{ColumnType, Expr},
    catalog::work::{inline, ReferenceWork, Work},
    expr::EngineHook,
    ir::observation::ScalarObserver,
    plan::{visit_stored_definition_scalars, PlanRef, QueryPlan, UnifiedPlan},
    type_resolution::base_type_with_work,
    SQLError, ScalarExpr,
};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    Value,
};

#[cfg(test)]
mod tests;

pub trait StoredRegroleResolver {
    fn resolve_stored_regrole(&self, name: &str) -> Result<Option<i64>, SQLError>;
}
impl<T: EngineHook + ?Sized> StoredRegroleResolver for T {
    fn resolve_stored_regrole(&self, name: &str) -> Result<Option<i64>, SQLError> {
        EngineHook::resolve_regrole(self, name)
    }
}

pub(crate) fn regrole_constant_error() -> SQLError {
    SQLError::Routine {
        sqlstate: "0A000".into(),
        message: "constant of the type regrole cannot be used here".into(),
    }
}

enum Constant<'a> {
    Input(&'a str),
    Bound,
}

enum Node<'a> {
    Syntax(&'a Expr),
    Scalar(&'a ScalarExpr),
}

impl<'a> Node<'a> {
    fn constant(self) -> Option<(Constant<'a>, &'a str, Option<&'a ColumnType>)> {
        match self {
            Self::Syntax(Expr::Cast { expr, ty, .. }) => match expr.as_ref() {
                Expr::Literal(Value::Str(input)) => Some((Constant::Input(input), ty, None)),
                _ => None,
            },
            Self::Scalar(ScalarExpr::Cast { expr, ty, .. }) => match expr.as_ref() {
                ScalarExpr::Literal(Value::Str(input)) => Some((Constant::Input(input), ty, None)),
                _ => None,
            },
            Self::Syntax(Expr::TypedLiteral { value, ty, .. }) if !matches!(value, Value::Null) => {
                Some((Constant::Bound, ty, None))
            }
            Self::Scalar(ScalarExpr::TypedLiteral {
                value,
                ty,
                bound_type,
                ..
            }) if !matches!(value, Value::Null) => Some((Constant::Bound, ty, bound_type.as_ref())),
            _ => None,
        }
    }

    async fn dependency<W: Work + Send>(
        self,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<Option<Constant<'a>>, SQLError> {
        let Some((constant, name, binding)) = self.constant() else {
            return Ok(None);
        };
        let mut work = ReferenceWork { control, work };
        if let Some(bound) = binding {
            if matches!(
                base_type_with_work(bound, control, &mut work).await?,
                ColumnType::Regrole
            ) {
                return Ok(Some(constant));
            }
        }
        match ColumnType::from_sql_name_with_input_work(name, control, &mut work).await {
            Ok(ty)
                if matches!(
                    base_type_with_work(&ty, control, &mut work).await?,
                    ColumnType::Regrole
                ) =>
            {
                Ok(Some(constant))
            }
            Err(error) if error.native_resource_error().is_some() => Err(error),
            _ => Ok(None),
        }
    }
}

#[derive(Debug, Default)]
pub struct StoredRegroleConstants {
    inputs: Vec<String>,
    has_bound_constant: bool,
}

struct Collector<'a> {
    inputs: ProductionVec<'a, String>,
    has_bound_constant: bool,
    control: ProductionControl<'a>,
}

impl<'a> Collector<'a> {
    fn new(control: &ProductionControl<'a>) -> Self {
        Self {
            inputs: ProductionVec::new(*control),
            has_bound_constant: false,
            control: *control,
        }
    }

    fn finish(self) -> Result<Produced<StoredRegroleConstants>, SQLError> {
        let (inputs, memory) = self.inputs.finish()?.into_parts();
        Ok(self.control.finish(
            StoredRegroleConstants {
                inputs,
                has_bound_constant: self.has_bound_constant,
            },
            memory,
        )?)
    }

    async fn observe(
        &mut self,
        node: Node<'_>,
        work: &mut (impl Work + Send),
    ) -> Result<(), SQLError> {
        match node.dependency(&self.control, work).await? {
            Some(Constant::Input(name)) => {
                self.inputs
                    .push_produced(work.text(name, &self.control).await?)?;
            }
            Some(Constant::Bound) => self.has_bound_constant = true,
            None => (),
        }
        Ok(())
    }
}

impl ScalarObserver for Collector<'_> {
    async fn scalar<W: Work + Send>(
        &mut self,
        value: &ScalarExpr,
        work: &mut W,
    ) -> Result<(), SQLError> {
        self.observe(Node::Scalar(value), work).await
    }
}

impl StoredRegroleConstants {
    pub(crate) fn has_constants(&self) -> bool {
        !self.inputs.is_empty() || self.has_bound_constant
    }

    pub(crate) fn retain(
        self,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_core::memory::Produced<Self>, SQLError> {
        if control.budget().is_none() {
            return Ok(control.finish(self, None)?);
        }
        let mut inputs = uqa_core::memory::ProductionVec::new(*control);
        inputs.reserve(self.inputs.len())?;
        for input in self.inputs {
            inputs.push_produced(control.copy_text(&input)?)?;
        }
        let (inputs, memory) = inputs.finish()?.into_parts();
        Ok(control.finish(
            Self {
                inputs,
                has_bound_constant: self.has_bound_constant,
            },
            memory,
        )?)
    }

    pub fn collect_expression(
        &mut self,
        expression: &Expr,
        assignment_target: Option<&ColumnType>,
    ) {
        let output = inline(Self::from_expression(
            expression,
            assignment_target,
            &ProductionControl::uncontrolled(),
            &mut (),
        ))
        .expect("ordinary stored expression collection has no resource limit");
        self.append(output);
    }

    pub(crate) async fn from_expression(
        expression: &Expr,
        assignment_target: Option<&ColumnType>,
        control: &ProductionControl<'_>,
        work: &mut (impl Work + Send),
    ) -> Result<Produced<Self>, SQLError> {
        let mut collector = Collector::new(control);
        if let (Expr::Literal(Value::Str(input)), Some(ty)) = (expression, assignment_target) {
            if matches!(
                base_type_with_work(ty, control, &mut ReferenceWork { control, work }).await?,
                ColumnType::Regrole
            ) {
                collector
                    .inputs
                    .push_produced(work.text(input, control).await?)?;
            }
        }
        let mut nodes = expression.stored_nodes_with_budget(control.budget())?;
        while let Some(node) = nodes.next_scalar(control, work).await? {
            collector.observe(Node::Syntax(node), work).await?;
        }
        collector.finish()
    }

    /// Keep dependency identity before a caller replaces an expression by its optimized form.
    pub fn collect_scalar_expression(&mut self, expression: &ScalarExpr) {
        expression.visit(&mut |node| self.collect_scalar(node));
    }

    fn collect_scalar(&mut self, expression: &ScalarExpr) {
        match inline(
            Node::Scalar(expression).dependency(&ProductionControl::uncontrolled(), &mut ()),
        )
        .expect("ordinary scalar classification has no resource limit")
        {
            Some(Constant::Input(name)) => self.inputs.push(name.to_owned()),
            Some(Constant::Bound) => self.has_bound_constant = true,
            None => (),
        }
    }

    pub fn collect_query_plan(&mut self, plan: &mut QueryPlan) {
        self.append_plan(PlanRef::Query(plan));
    }

    pub fn collect_plan(&mut self, plan: &mut UnifiedPlan) {
        self.append_plan((&*plan).into());
    }

    fn append_plan(&mut self, plan: PlanRef<'_>) {
        let output = inline(Self::from_plan(
            plan,
            &ProductionControl::uncontrolled(),
            &mut (),
        ))
        .expect("ordinary plan collection has no resource limit");
        self.append(output);
    }

    fn append(&mut self, output: Produced<Self>) {
        let mut constants = output
            .into_uncontrolled()
            .expect("ordinary role names have no reservation");
        self.has_bound_constant |= constants.has_bound_constant;
        if self.inputs.is_empty() {
            self.inputs = constants.inputs;
        } else {
            self.inputs.append(&mut constants.inputs);
        }
    }

    pub(crate) async fn from_plan(
        plan: PlanRef<'_>,
        control: &ProductionControl<'_>,
        work: &mut (impl Work + Send),
    ) -> Result<Produced<Self>, SQLError> {
        let mut constants = Collector::new(control);
        // A dependency is either a leaf or a cast of a string literal. No such node contains another dependency, so their order is also the rewrite's post-order.
        visit_stored_definition_scalars(plan, control, work, &mut constants).await?;
        constants.finish()
    }

    pub fn validate_inputs(&self, context: &dyn EngineHook) -> Result<(), SQLError> {
        self.validate_inputs_with(context)
    }

    pub fn validate_inputs_with<C: StoredRegroleResolver + ?Sized>(
        &self,
        context: &C,
    ) -> Result<(), SQLError> {
        let mut next = 0;
        while self.validate_next_input(context, &mut next)? {}
        Ok(())
    }

    /// Resolve one deferred input in definition order. Only a successful lookup consumes that input.
    pub(crate) fn validate_next_input<C: StoredRegroleResolver + ?Sized>(
        &self,
        context: &C,
        next: &mut usize,
    ) -> Result<bool, SQLError> {
        let Some(name) = self.inputs.get(*next) else {
            return Ok(false);
        };
        context.resolve_stored_regrole(name)?;
        *next += 1;
        Ok(true)
    }

    pub fn reject(&self, context: &dyn EngineHook) -> Result<(), SQLError> {
        self.reject_with(context)
    }

    pub fn reject_with<C: StoredRegroleResolver + ?Sized>(
        &self,
        context: &C,
    ) -> Result<(), SQLError> {
        self.validate_inputs_with(context)?;
        if self.has_constants() {
            Err(regrole_constant_error())
        } else {
            Ok(())
        }
    }
}

pub fn reject_stored_regrole_constants(
    context: &dyn EngineHook,
    expression: &Expr,
    assignment_target: Option<&ColumnType>,
) -> Result<(), SQLError> {
    reject_stored_regrole_constants_with(context, expression, assignment_target)
}

pub fn reject_stored_regrole_constants_with<C: StoredRegroleResolver + ?Sized>(
    context: &C,
    expression: &Expr,
    assignment_target: Option<&ColumnType>,
) -> Result<(), SQLError> {
    let mut constants = StoredRegroleConstants::default();
    constants.collect_expression(expression, assignment_target);
    constants.reject_with(context)
}

pub fn reject_stored_query_regrole_constants(
    context: &dyn EngineHook,
    plan: &mut QueryPlan,
) -> Result<(), SQLError> {
    let mut constants = StoredRegroleConstants::default();
    constants.collect_query_plan(plan);
    constants.reject(context)
}

pub fn reject_stored_plan_regrole_constants(
    context: &dyn EngineHook,
    plan: &mut UnifiedPlan,
) -> Result<(), SQLError> {
    reject_stored_plan_regrole_constants_with(context, plan)
}

pub fn reject_stored_plan_regrole_constants_with<C: StoredRegroleResolver + ?Sized>(
    context: &C,
    plan: &mut UnifiedPlan,
) -> Result<(), SQLError> {
    let mut constants = StoredRegroleConstants::default();
    constants.collect_plan(plan);
    constants.reject_with(context)
}
