//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine images preserve original declarations and separately bound SQL bodies.

use super::{
    fields, invalid, variant, DefinitionImageReader, Frame, ImageFrame, Node, Part, SQLError,
};
use crate::{
    ast::{
        CreateFunction, FunctionBody, FunctionParallel, FunctionParam, FunctionParamMode,
        FunctionReturns, FunctionVolatility, ObjectAclEntry, RoutineColumnTypeReference,
        RoutineDefaultType, RoutineSecurityAttributes, SQLBodyForm,
    },
    routines::{
        registry::{CapturedRoutines, RoutineRegistry},
        CompiledFunctionBody, RoutineBody, SQLRoutine, SQLUserFunction,
    },
};
use uqa_core::{memory::MemoryBudget, CancellationToken};

impl CapturedRoutines {
    /// Borrow the captured overload order, original definitions and bound SQL without parsing or invoking bodies.
    pub fn image_reader<'a>(
        &'a self,
        memory: &'a MemoryBudget,
        cancellation: &'a CancellationToken,
    ) -> Result<DefinitionImageReader<'a>, SQLError> {
        let mut reader = DefinitionImageReader::start(
            ImageFrame::NextRoutine,
            *b"UQAROU01",
            self.len(),
            memory,
            cancellation,
        )?;
        reader.routines = Some(self.iter());
        reader.statement_expressions = true;
        Ok(reader)
    }
}

impl RoutineRegistry {
    /// Reconstruct an unpublished native catalog with independently retained definitions and bound bodies. Cross-catalog validation belongs to the enclosing restore.
    pub fn read_image(
        reader: &mut dyn std::io::Read,
        memory: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<Self, SQLError> {
        super::super::super::input::routines(reader, memory, cancellation)
    }
}

impl DefinitionImageReader<'_> {
    pub(in super::super) fn next_routine(&mut self) -> Result<(), SQLError> {
        if let Some((name, values)) = self.routines.as_mut().and_then(Iterator::next) {
            self.fields.text(name);
            self.push(ImageFrame::NextRoutine)?;
            self.push(ImageFrame::Plan(Frame::Node(values, 0)))?;
        }
        Ok(())
    }
}

impl Node for SQLRoutine {
    fn part(&self, position: usize) -> Result<Option<Part<'_>>, SQLError> {
        (***self).part(position)
    }
}
impl Node for SQLUserFunction {
    fn part(&self, position: usize) -> Result<Option<Part<'_>>, SQLError> {
        Ok(fields(position, &[&self.def, &self.body]))
    }
}
impl Node for RoutineBody {
    fn part(&self, position: usize) -> Result<Option<Part<'_>>, SQLError> {
        Ok(match self {
            Self::Source => variant(position, 0, &[]),
            Self::Bound(body) => match &***body {
                CompiledFunctionBody::SQL(plans) => variant(position, 1, &[plans]),
                CompiledFunctionBody::PLpgSQL(_) => {
                    return Err(invalid("session-compiled routine body"))
                }
            },
        })
    }
}
impl Node for CreateFunction {
    fn part(&self, position: usize) -> Result<Option<Part<'_>>, SQLError> {
        let Self {
            object_id,
            catalog_revision,
            catalog_oid,
            name,
            or_replace,
            is_procedure,
            params,
            returns,
            return_type_reference,
            return_written_type,
            language,
            body,
            body_origin: _,
            sql_body_form,
            creation_search_path,
            volatility,
            strict,
            owner,
            security,
            parallel,
            support,
            cost,
            rows,
            config,
            config_actions,
            attribute_clauses,
            execute_acl,
        } = self;
        if position == 0 && (!config_actions.is_empty() || !attribute_clauses.is_empty()) {
            return Err(invalid("unpublished routine declaration actions"));
        }
        Ok(fields(
            position,
            &[
                object_id,
                catalog_revision,
                catalog_oid,
                name,
                or_replace,
                is_procedure,
                params,
                returns,
                return_type_reference,
                return_written_type,
                language,
                body,
                sql_body_form,
                creation_search_path,
                volatility,
                strict,
                owner,
                security,
                parallel,
                support,
                cost,
                rows,
                config,
                execute_acl,
            ],
        ))
    }
}

image_struct!(FunctionParam {
    name,
    type_name,
    type_reference,
    written_type,
    mode,
    default,
    default_type
});
image_struct!(RoutineColumnTypeReference {
    schema,
    relation,
    column
});
image_struct!(RoutineSecurityAttributes {
    security_definer,
    leakproof
});
image_struct!(ObjectAclEntry {
    role,
    grantor,
    grant_option
});
image_enum!(FunctionBody { 0 => Source(value), 1 => Statements(value) });
image_enum!(FunctionParamMode { 0 => In, 1 => Out, 2 => InOut, 3 => Variadic, 4 => Table });
image_enum!(FunctionReturns { 0 => None, 1 => Scalar { type_name }, 2 => SetOf { type_name }, 3 => Table });
image_enum!(FunctionVolatility { 0 => Immutable, 1 => Stable, 2 => Volatile });
image_enum!(FunctionParallel { 0 => Unsafe, 1 => Restricted, 2 => Safe });
image_enum!(SQLBodyForm { 0 => Return, 1 => Atomic });
image_enum!(RoutineDefaultType { 0 => Concrete(value), 1 => Polymorphic(value) });

impl Node for f32 {
    fn part(&self, position: usize) -> Result<Option<Part<'_>>, SQLError> {
        Ok((position == 0).then_some(Part::U32(self.to_bits())))
    }
}
