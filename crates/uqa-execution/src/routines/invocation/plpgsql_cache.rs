//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Procedural preparations live with a routine version and its concrete invocation type.
use super::bodies::{routine_identity, CompiledRoutine, SessionRoutineBodies};
use crate::routines::preparation::PLpgSQLPreparations;
use std::sync::Arc;
use uqa_core::memory::{OwnedMap, Produced, ProductionControl, ProductionVec};
use uqa_sql::{
    ast::CreateFunction,
    plpgsql::{PLpgSQLDatum, PLpgSQLFunction},
    routines::{CompiledFunctionBody, SQLUserFunction},
    type_resolution::canonical_routine_type_name_with_control,
    SQLError,
};

pub(super) struct PreparedBody {
    arguments: Produced<Vec<String>>,
    relation: Option<i64>,
    body: Arc<CompiledFunctionBody>,
    preparations: Arc<PLpgSQLPreparations>,
}
impl PreparedBody {
    fn new(
        body: Arc<CompiledFunctionBody>,
        arguments: Produced<Vec<String>>,
        relation: Option<i64>,
        preparations: Arc<PLpgSQLPreparations>,
    ) -> Self {
        Self {
            arguments,
            relation,
            body,
            preparations,
        }
    }
}
impl CompiledRoutine {
    pub(super) fn new(version: u64, body: Arc<CompiledFunctionBody>) -> Self {
        Self {
            version,
            body,
            plpgsql: OwnedMap::new(),
        }
    }
}
impl SessionRoutineBodies {
    /// Reuse a validator compilation only for matching concrete arguments and
    /// ordinary invocation. Trigger relation descriptors have separate plans.
    pub fn plpgsql_body(
        &self,
        function: &SQLUserFunction,
        definition: &CreateFunction,
        relation: Option<i64>,
        control: &ProductionControl<'_>,
        compile: impl FnOnce(&CreateFunction) -> Result<CompiledFunctionBody, SQLError>,
    ) -> Result<Arc<CompiledFunctionBody>, SQLError> {
        let identity = routine_identity(function)?;
        let version = function.definition_version()?;
        let mut arguments = ProductionVec::new(*control);
        arguments.reserve(definition.params.len())?;
        for parameter in &definition.params {
            arguments.push_produced(canonical_routine_type_name_with_control(
                &parameter.type_name,
                control,
            )?)?;
        }
        let arguments = arguments.finish()?;
        {
            let mut bodies = self.compiled.lock();
            if let Some(compiled) = bodies.get_mut(&identity).filter(|c| c.version == version) {
                if let Some(cached) = compiled.plpgsql.values().find(|p| {
                    p.arguments.as_slice() == arguments.as_slice() && p.relation == relation
                }) {
                    return Ok(Arc::clone(&cached.body));
                }
                if relation.is_none()
                    && compatible_validator(&compiled.body, &arguments, &function.def, control)?
                {
                    let body = Arc::clone(&compiled.body);
                    // A validator body has one preparation owner even when an
                    // equivalent signature spelling is used at invocation.
                    if !compiled
                        .plpgsql
                        .values()
                        .any(|p| Arc::ptr_eq(&p.body, &body))
                    {
                        compiled.retain_preparation(
                            Arc::clone(&body),
                            arguments,
                            relation,
                            &self.procedural,
                            control,
                        )?;
                        return Ok(body);
                    }
                }
            }
        }
        let body = Arc::new(compile(definition)?);
        let mut bodies = self.compiled.lock();
        let compiled = bodies
            .entry(identity)
            .or_insert_with(|| CompiledRoutine::new(version, Arc::clone(&body)));
        if compiled.version != version {
            *compiled = CompiledRoutine::new(version, Arc::clone(&body));
        }
        if let Some(cached) = compiled
            .plpgsql
            .values()
            .find(|p| p.arguments.as_slice() == arguments.as_slice() && p.relation == relation)
        {
            return Ok(Arc::clone(&cached.body));
        }
        compiled.retain_preparation(
            Arc::clone(&body),
            arguments,
            relation,
            &self.procedural,
            control,
        )?;
        Ok(body)
    }

    /// Interpreter clones share preparation only with their owning compilation.
    /// Anonymous blocks and detached old activations receive activation-local state.
    pub fn plpgsql_preparations(
        &self,
        definition: &CreateFunction,
        parsed: &PLpgSQLFunction,
        control: &ProductionControl<'_>,
    ) -> Result<Arc<PLpgSQLPreparations>, SQLError> {
        control.check()?;
        if let Some(identity) = definition.object_id {
            let bodies = self.compiled.lock();
            if let Some(compiled) = bodies.get(&identity) {
                for entry in compiled.plpgsql.values() {
                    if matches!(&*entry.body, CompiledFunctionBody::PLpgSQL(body) if body.compilation.same(&parsed.compilation))
                    {
                        return Ok(Arc::clone(&entry.preparations));
                    }
                }
            }
        }
        self.procedural.register(control)
    }
}
fn compatible_validator(
    body: &CompiledFunctionBody,
    arguments: &[String],
    declared: &CreateFunction,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    let CompiledFunctionBody::PLpgSQL(parsed) = body else {
        return Ok(false);
    };
    for (index, expected) in arguments.iter().enumerate() {
        control.check()?;
        let actual = match parsed.datums.get(index) {
            Some(PLpgSQLDatum::Var(var)) => Some(var.type_name.as_str()),
            Some(PLpgSQLDatum::Rec { .. }) => declared
                .params
                .get(index)
                .map(|parameter| parameter.type_name.as_str()),
            _ => None,
        };
        let Some(actual) = actual else {
            return Ok(false);
        };
        if canonical_routine_type_name_with_control(actual, control)?.as_str() != expected {
            return Ok(false);
        }
    }
    Ok(true)
}

impl CompiledRoutine {
    fn retain_preparation(
        &mut self,
        body: Arc<CompiledFunctionBody>,
        arguments: Produced<Vec<String>>,
        relation: Option<i64>,
        registry: &crate::routines::preparation::PLpgSQLPreparationRegistry,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        control.check()?;
        let key = Arc::as_ptr(&body) as usize;
        let produce = || -> Result<PreparedBody, SQLError> {
            Ok(PreparedBody::new(
                body,
                arguments,
                relation,
                registry.register(control)?,
            ))
        };
        if let Some(budget) = control.budget() {
            self.plpgsql.insert_budgeted_with(key, budget, produce)?;
        } else {
            self.plpgsql.insert(key, produce()?);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
