//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rewrite-rule images preserve bound OLD/NEW identities and exact dependency sets.

use crate::{
    ast::{CreateRule, EventEnableMode},
    catalog::events::{
        RuleCatalog, RuleColumnDependency, RuleConditionBinding, RuleDependencies,
        RuleRoutineDependency, StoredRule,
    },
    plan::{copy::SyntaxProduction, ExpressionPlan},
    SQLError,
};
use uqa_core::{
    memory::{Produced, ProductionControl},
    RelationIdentity,
};

#[derive(Debug)]
pub struct CapturedRuleDependencies {
    pub relations: Vec<RelationIdentity>,
    pub columns: Vec<RuleColumnDependency>,
    pub routines: Vec<RuleRoutineDependency>,
}

#[derive(Debug)]
pub struct CapturedRule {
    pub definition: CreateRule,
    pub enabled: EventEnableMode,
    pub condition_plan: Option<ExpressionPlan>,
    pub condition_binding: Option<RuleConditionBinding>,
    pub dependencies: Option<CapturedRuleDependencies>,
    pub catalog_oid: Option<i64>,
}

pub type CapturedRules = Vec<(RelationIdentity, Vec<(String, CapturedRule)>)>;

impl RuleDependencies {
    pub fn capture_dependencies(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<CapturedRuleDependencies>, SQLError> {
        let output = SyntaxProduction::new(*control);
        let Self {
            relations,
            columns,
            routines,
        } = self;
        let mut copied_relations = output.vector(relations.len())?;
        for relation in relations {
            copied_relations.push(output.copy(relation)?)?;
        }
        let mut copied_columns = output.vector(columns.len())?;
        for column in columns {
            copied_columns.push(output.copy(column)?)?;
        }
        let mut copied_routines = output.vector(routines.len())?;
        for routine in routines {
            copied_routines.push(output.copy(routine)?)?;
        }
        let captured = CapturedRuleDependencies {
            relations: copied_relations.finish()?,
            columns: copied_columns.finish()?,
            routines: copied_routines.finish()?,
        };
        output.finish(captured)
    }
}

impl StoredRule {
    pub fn capture_definition(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<CapturedRule>, SQLError> {
        let output = SyntaxProduction::new(*control);
        let Self {
            definition,
            enabled,
            condition_plan,
            condition_binding,
            dependencies,
            catalog_oid,
        } = self;
        let captured = CapturedRule {
            definition: output.capture_stored(definition)?,
            enabled: *enabled,
            condition_plan: output.capture_stored(condition_plan)?,
            condition_binding: condition_binding
                .as_ref()
                .map(|binding| {
                    binding
                        .capture_binding(control)
                        .map(|value| output.take(value))
                })
                .transpose()?,
            dependencies: dependencies
                .as_ref()
                .map(|dependencies| {
                    dependencies
                        .capture_dependencies(control)
                        .map(|value| output.take(value))
                })
                .transpose()?,
            catalog_oid: *catalog_oid,
        };
        output.finish(captured)
    }
}

pub fn rules(
    values: &RuleCatalog,
    control: &ProductionControl<'_>,
) -> Result<Produced<CapturedRules>, SQLError> {
    selected_rules(values, control, |_| true)
}

/// Preserve original durable relation bindings without retaining temporary rule bodies.
pub fn durable_rules(
    values: &RuleCatalog,
    control: &ProductionControl<'_>,
) -> Result<Produced<CapturedRules>, SQLError> {
    selected_rules(values, control, |relation| {
        !crate::catalog::temporary_namespace::is_temporary_schema_name(&relation.schema)
    })
}

fn selected_rules(
    values: &RuleCatalog,
    control: &ProductionControl<'_>,
    include: impl Fn(&RelationIdentity) -> bool,
) -> Result<Produced<CapturedRules>, SQLError> {
    let output = SyntaxProduction::new(*control);
    let count = values.keys().try_fold(0, |count, relation| {
        output.check()?;
        Ok::<_, SQLError>(count + usize::from(include(relation)))
    })?;
    let mut relations = output.vector(count)?;
    for (relation, values) in values {
        output.check()?;
        if !include(relation) {
            continue;
        }
        let mut entries = output.vector(values.len())?;
        for (name, value) in values.iter() {
            entries.push((
                output.text(name)?,
                output.take(value.capture_definition(control)?),
            ))?;
        }
        relations.push((output.copy(relation)?, entries.finish()?))?;
    }
    let relations = relations.finish()?;
    output.finish(relations)
}

#[cfg(test)]
mod tests;
