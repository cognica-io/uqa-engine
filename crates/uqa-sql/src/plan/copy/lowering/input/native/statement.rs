//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete retained statements keep their construction owner through normalization and observation.

use super::{
    build, construct, construction, mutations, AggregateClassifier, Copying, Prefix,
    ProductionControl, Result, SQLError,
};
use crate::{
    ast::Statement,
    plan::{
        copy::ParameterLookup, retirement::construction::Context, MutableCommandPlan,
        MutableExecutablePlan, PlanRef, UnifiedPlan,
    },
};
use uqa_core::image_io::AsyncImageInput;

/// A completed private executable with its original mutable field leases and admitted retirement workspace. Observation borrows the native plan; dropping the owner retires its complete tree before returning those leases.
#[derive(Debug)]
pub struct RetainedExecutablePlan(Prefix<MutableExecutablePlan>);

impl RetainedExecutablePlan {
    pub(crate) async fn bind_relation_names_from_input(
        self,
        resolve: &crate::binding::view_dependencies::owned::fields::StoredRelationResolver<'_>,
        control: &ProductionControl<'_>,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<(Self, bool)> {
        let (plan, temporary) = self
            .0
            .bind_relation_names_from_input(resolve, control, input)
            .await?;
        Ok((Self(plan), temporary))
    }

    /// Bind sequence references in a freshly lowered statement after its complete relation pass. The retained constructor owns every nested query and mutation; catalog lookup uses the supplied admitted-name producer.
    pub async fn bind_sequence_references_from_input(
        self,
        resolve: &(dyn Fn(&str, &ProductionControl<'_>) -> Result<uqa_core::memory::Produced<String>>
              + Send
              + Sync),
        control: &ProductionControl<'_>,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<Self> {
        Ok(Self(
            self.0
                .bind_sequence_names_from_input(resolve, control, input)
                .await?,
        ))
    }

    pub fn as_plan_ref(&self) -> PlanRef<'_> {
        self.0.as_plan_ref()
    }

    pub fn reserved_bytes(&self) -> usize {
        self.0.reserved_bytes()
    }
}

impl UnifiedPlan {
    /// Construct a retained SELECT, VALUES or INSERT/UPDATE/DELETE/MERGE executable through the existing native work channel. Query arenas, catalog aggregate classification and canonical window normalization share ordinary lowering semantics. Source parsing and its immutable syntax owner remain with the caller; utility commands have separate preparation owners.
    pub async fn lower_executable_from_input(
        statement: &Statement,
        aggregates: &(dyn AggregateClassifier + Sync),
        control: &ProductionControl<'_>,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<RetainedExecutablePlan> {
        Self::lower_executable_with_parameters_from_input(
            statement, aggregates, control, None, input,
        )
        .await
    }

    pub(in crate::plan) async fn lower_executable_with_parameters_from_input(
        statement: &Statement,
        aggregates: &(dyn AggregateClassifier + Sync),
        control: &ProductionControl<'_>,
        parameters: Option<&ParameterLookup<'_>>,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<RetainedExecutablePlan> {
        control.check()?;
        if control.budget().is_none() {
            return Err(SQLError::Internal(
                "native statement construction requires a destination allowance".into(),
            ));
        }
        input.checkpoint().await?;
        let mut driver = construction::Driver::new(*control)?;
        let output = driver.work().submit(move |mut work| async move {
            let mut copying =
                Copying::for_rewrite(*control).expect("admitted statement constructor");
            copying.plan_retirement = Some(Context::new(control)?);
            copying.parameters = parameters;
            let plan = match statement {
                Statement::Select(query) => {
                    let query = build(
                        construct::Source::Select(query),
                        copying,
                        work.clone(),
                        aggregates,
                    )?
                    .await?;
                    query.map(MutableExecutablePlan::Query)
                }
                Statement::Values { rows } => {
                    let query = build(
                        construct::Source::Values(rows),
                        copying,
                        work.clone(),
                        aggregates,
                    )?
                    .await?;
                    query.map(MutableExecutablePlan::Query)
                }
                statement => {
                    let source =
                        crate::plan::copy::lowering::mutations::Source::statement(statement)
                            .ok_or_else(|| {
                                SQLError::Unsupported(
                                    "native executable construction requires a query or mutation"
                                        .into(),
                                )
                            })?;
                    let command =
                        mutations::child(source, &mut copying, &mut work, aggregates).await?;
                    command.map(|command| {
                        MutableExecutablePlan::Command(MutableCommandPlan::from_rewrite_parts(
                            command,
                            Some(copying.into_rewrite_memory()),
                        ))
                    })
                }
            };
            control.check()?;
            Ok::<_, SQLError>(RetainedExecutablePlan(plan))
        })?;
        while driver.advance().await? {
            input.checkpoint().await?;
        }
        control.check()?;
        construction::receive(output).await
    }
}
