//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Procedural preparations live with a routine version and its concrete invocation type.
use super::bodies::{routine_identity, CompiledRoutine, SessionRoutineBodies};
use crate::routines::preparation::PLpgSQLPreparations;
use std::sync::Arc;
use uqa_sql::{
    ast::CreateFunction,
    plpgsql::{PLpgSQLDatum, PLpgSQLFunction},
    routines::{CompiledFunctionBody, SQLUserFunction},
    type_resolution::canonical_routine_type_name,
    SQLError,
};

pub(super) struct PreparedBody {
    arguments: Vec<String>,
    relation: Option<i64>,
    body: Arc<CompiledFunctionBody>,
    preparations: Arc<PLpgSQLPreparations>,
}
impl PreparedBody {
    fn new(
        body: Arc<CompiledFunctionBody>,
        arguments: Vec<String>,
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
            plpgsql: Vec::new(),
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
        compile: impl FnOnce(&CreateFunction) -> Result<CompiledFunctionBody, SQLError>,
    ) -> Result<Arc<CompiledFunctionBody>, SQLError> {
        let identity = routine_identity(function)?;
        let version = function.definition_version()?;
        let arguments: Vec<_> = definition
            .params
            .iter()
            .map(|p| canonical_routine_type_name(&p.type_name))
            .collect();
        {
            let mut bodies = self.compiled.lock();
            if let Some(compiled) = bodies.get_mut(&identity).filter(|c| c.version == version) {
                if let Some(cached) = compiled
                    .plpgsql
                    .iter()
                    .find(|p| p.arguments == arguments && p.relation == relation)
                {
                    return Ok(Arc::clone(&cached.body));
                }
                if relation.is_none()
                    && compatible_validator(&compiled.body, &arguments, &function.def)
                {
                    let body = Arc::clone(&compiled.body);
                    // A validator body has one preparation owner even when an
                    // equivalent signature spelling is used at invocation.
                    if !compiled.plpgsql.iter().any(|p| Arc::ptr_eq(&p.body, &body)) {
                        compiled.plpgsql.push(PreparedBody::new(
                            Arc::clone(&body),
                            arguments,
                            relation,
                            self.procedural.register(),
                        ));
                        return Ok(body);
                    }
                }
            }
        }
        let body = Arc::new(compile(definition)?);
        let prepared = PreparedBody::new(
            Arc::clone(&body),
            arguments.clone(),
            relation,
            self.procedural.register(),
        );
        let mut bodies = self.compiled.lock();
        let compiled = bodies
            .entry(identity)
            .or_insert_with(|| CompiledRoutine::new(version, Arc::clone(&body)));
        if compiled.version != version {
            *compiled = CompiledRoutine::new(version, Arc::clone(&body));
        }
        if let Some(cached) = compiled
            .plpgsql
            .iter()
            .find(|p| p.arguments == arguments && p.relation == relation)
        {
            return Ok(Arc::clone(&cached.body));
        }
        compiled.plpgsql.push(prepared);
        Ok(body)
    }

    /// Interpreter clones share preparation only with their owning compilation.
    /// Anonymous blocks and detached old activations receive activation-local state.
    pub fn plpgsql_preparations(
        &self,
        definition: &CreateFunction,
        parsed: &PLpgSQLFunction,
    ) -> Arc<PLpgSQLPreparations> {
        if let Some(identity) = definition.object_id {
            let bodies = self.compiled.lock();
            if let Some(compiled) = bodies.get(&identity) {
                for entry in &compiled.plpgsql {
                    if matches!(&*entry.body, CompiledFunctionBody::PLpgSQL(body) if body.compilation.same(&parsed.compilation))
                    {
                        return Arc::clone(&entry.preparations);
                    }
                }
            }
        }
        self.procedural.register()
    }
}
fn compatible_validator(
    body: &CompiledFunctionBody,
    arguments: &[String],
    declared: &CreateFunction,
) -> bool {
    let CompiledFunctionBody::PLpgSQL(parsed) = body else {
        return false;
    };
    arguments
        .iter()
        .enumerate()
        .all(|(i, expected)| match parsed.datums.get(i) {
            Some(PLpgSQLDatum::Var(var)) => {
                canonical_routine_type_name(&var.type_name) == *expected
            }
            Some(PLpgSQLDatum::Rec { .. }) => declared.params.get(i).is_some_and(|parameter| {
                canonical_routine_type_name(&parameter.type_name) == *expected
            }),
            _ => false,
        })
}

#[cfg(test)]
mod tests;
