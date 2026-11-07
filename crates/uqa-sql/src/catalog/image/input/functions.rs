//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native binding reconstruction retains dispatch and deferred errors without reevaluation.

use super::super::function_dispatch::OperationTag;
use super::{invalid, Input};
use crate::{
    ast::{
        ArraySliceBounds, FunctionBinding, FunctionDispatch, FunctionResolutionError,
        OperatorResolutionError,
    },
    SQLError,
};
use std::{io::Read, num::NonZeroU32};
use uqa_core::{
    memory::{Budgeted, MemoryBudget},
    CancellationToken,
};

impl Input<'_> {
    fn operation<T: OperationTag>(&mut self) -> Result<T, SQLError> {
        T::from_image_tag(self.binary.array::<1>()?[0])
    }

    fn dispatch_payload(&mut self, tag: u8) -> Result<FunctionDispatch, SQLError> {
        use FunctionDispatch as D;
        Ok(match tag {
            0 => D::NumericOperator(self.operation()?),
            1 => D::Pattern {
                operator: self.operation()?,
                oid: self
                    .optional_number()?
                    .map(|oid| {
                        NonZeroU32::new(oid)
                            .ok_or_else(|| invalid("zero pattern operator identifier"))
                    })
                    .transpose()?,
            },
            2 => D::PatternEscape {
                similar: self.flag()?,
            },
            3 => D::BooleanTest(self.operation()?),
            4 => D::Collate {
                oid: self.optional_number()?,
            },
            5 => D::CollationFor {
                collation_oid: u32::from_le_bytes(self.binary.array()?),
                noncollatable_type_oid: self.optional_number()?,
            },
            6 => D::TypeOf {
                type_oid: u32::from_le_bytes(self.binary.array()?),
            },
            7 => D::NamedArgument,
            8 => D::VariadicArgument,
            9 => D::ArraySubscripts,
            10 => D::ArraySlices,
            11 => {
                D::ArraySlicesWithBounds(ArraySliceBounds::from_stored_masks(self.binary.array()?))
            }
            12 => D::Subscript,
            13 => D::Slice,
            14 => D::FieldSelect,
            15 => D::AssignmentStore,
            16 => D::AnyOperator,
            17 => D::AllOperator,
            18 => D::IsDistinct,
            19 => D::DistinctNot,
            20 => D::RowDistinct,
            21 => D::ValueIsDistinct,
            22 => D::ValueNullTest {
                negated: self.flag()?,
            },
            23 => D::BetweenSymmetric,
            24 => D::ToBinInt4,
            25 => D::ToBinInt8,
            26 => D::ToHexInt4,
            27 => D::ToHexInt8,
            28 => D::ToOctInt4,
            29 => D::ToOctInt8,
            30 => D::RandomInt4Range,
            31 => D::RandomInt8Range,
            32 => D::RandomNumericRange,
            33 => D::RegexSubstring,
            34 => D::SimilarSubstring,
            35 => D::ArraySortJson,
            36 => D::Concat {
                oid: self.optional_number()?,
            },
            37 => D::ArrayConcat,
            38 => D::ArrayAppend,
            39 => D::ArrayPrepend,
            40 => D::JsonPath {
                as_text: self.flag()?,
                jsonb: self.flag()?,
            },
            41 => D::JsonExtract {
                as_text: self.flag()?,
                path: self.flag()?,
            },
            42 => D::Range {
                operation: self.operation()?,
                subtype: self.operation()?,
                multirange: self.flag()?,
            },
            43 => D::Enum {
                operation: self.operation()?,
                type_oid: u32::from_le_bytes(self.binary.array()?),
            },
            44 => D::MergeAction,
            _ => return Err(invalid("function dispatch tag")),
        })
    }

    fn function_resolution_error(
        &mut self,
    ) -> Result<Budgeted<Option<Box<FunctionResolutionError>>>, SQLError> {
        let mut memory = self.binary.memory.empty_reservation();
        let tag = self.binary.array::<1>()?[0];
        if tag == 0 {
            return Ok(Budgeted::new(None, memory));
        }
        if tag > 2 {
            return Err(invalid("function resolution error tag"));
        }
        memory.grow(size_of::<FunctionResolutionError>())?;
        let error = if tag == 1 {
            let (signature, reservation) = self.binary.text()?.into_parts();
            memory.absorb(reservation);
            FunctionResolutionError::UndefinedFunction { signature }
        } else {
            memory.grow(size_of::<OperatorResolutionError>())?;
            let (sqlstate, reservation) = self.binary.text()?.into_parts();
            memory.absorb(reservation);
            let (message, reservation) = self.binary.text()?.into_parts();
            memory.absorb(reservation);
            FunctionResolutionError::Operator(Box::new(OperatorResolutionError {
                sqlstate,
                message,
            }))
        };
        Ok(Budgeted::new(Some(Box::new(error)), memory))
    }

    pub(super) fn function_binding(&mut self) -> Result<Budgeted<FunctionBinding>, SQLError> {
        let (mut value, mut memory) = self.function_binding_fields()?.into_parts();
        value.dispatch = if self.flag()? {
            let tag = self.binary.array::<1>()?[0];
            Some(self.dispatch_payload(tag)?)
        } else {
            None
        };
        let (error, reservation) = self.function_resolution_error()?.into_parts();
        value.resolution_error = error;
        memory.absorb(reservation);
        Ok(Budgeted::new(value, memory))
    }
}

pub(in super::super) fn function_binding_image(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<Budgeted<FunctionBinding>, SQLError> {
    let mut input = Input::new(reader, memory, cancellation);
    if input.header(super::super::FUNCTION_BINDING)? != 1 {
        return Err(invalid("function binding count"));
    }
    let result = input.function_binding()?;
    input.binary.end()?;
    Ok(result)
}
