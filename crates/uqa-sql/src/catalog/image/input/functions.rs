//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native binding reconstruction retains dispatch and deferred errors without reevaluation.

use super::super::function_dispatch::OperationTag;
use super::{invalid, Fields, Input};
use crate::{
    ast::{
        ArraySliceBounds, FunctionBinding, FunctionDispatch, FunctionResolutionError,
        OperatorResolutionError,
    },
    SQLError,
};
use std::{io::Read, num::NonZeroU32};
use uqa_core::{
    image_io::{decode_image, ImageDecoder},
    memory::{Budgeted, MemoryBudget},
    CancellationToken,
};

impl Fields<'_> {
    async fn operation<T: OperationTag>(&mut self) -> Result<T, SQLError> {
        T::from_image_tag(self.binary.array::<1>().await?[0])
    }

    async fn dispatch_payload(&mut self, tag: u8) -> Result<FunctionDispatch, SQLError> {
        use FunctionDispatch as D;
        Ok(match tag {
            0 => D::NumericOperator(self.operation().await?),
            1 => D::Pattern {
                operator: self.operation().await?,
                oid: self
                    .optional_number()
                    .await?
                    .map(|oid| {
                        NonZeroU32::new(oid)
                            .ok_or_else(|| invalid("zero pattern operator identifier"))
                    })
                    .transpose()?,
            },
            2 => D::PatternEscape {
                similar: self.flag().await?,
            },
            3 => D::BooleanTest(self.operation().await?),
            4 => D::Collate {
                oid: self.optional_number().await?,
            },
            5 => D::CollationFor {
                collation_oid: u32::from_le_bytes(self.binary.array().await?),
                noncollatable_type_oid: self.optional_number().await?,
            },
            6 => D::TypeOf {
                type_oid: u32::from_le_bytes(self.binary.array().await?),
            },
            7 => D::NamedArgument,
            8 => D::VariadicArgument,
            9 => D::ArraySubscripts,
            10 => D::ArraySlices,
            11 => D::ArraySlicesWithBounds(ArraySliceBounds::from_stored_masks(
                self.binary.array().await?,
            )),
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
                negated: self.flag().await?,
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
                oid: self.optional_number().await?,
            },
            37 => D::ArrayConcat,
            38 => D::ArrayAppend,
            39 => D::ArrayPrepend,
            40 => D::JsonPath {
                as_text: self.flag().await?,
                jsonb: self.flag().await?,
            },
            41 => D::JsonExtract {
                as_text: self.flag().await?,
                path: self.flag().await?,
            },
            42 => D::Range {
                operation: self.operation().await?,
                subtype: self.operation().await?,
                multirange: self.flag().await?,
            },
            43 => D::Enum {
                operation: self.operation().await?,
                type_oid: u32::from_le_bytes(self.binary.array().await?),
            },
            44 => D::MergeAction,
            _ => return Err(invalid("function dispatch tag")),
        })
    }

    async fn function_resolution_error(
        &mut self,
    ) -> Result<Budgeted<Option<Box<FunctionResolutionError>>>, SQLError> {
        let mut memory = self.binary.memory.empty_reservation();
        let tag = self.binary.array::<1>().await?[0];
        if tag == 0 {
            return Ok(Budgeted::new(None, memory));
        }
        if tag > 2 {
            return Err(invalid("function resolution error tag"));
        }
        memory.grow(size_of::<FunctionResolutionError>())?;
        let error = if tag == 1 {
            let (signature, reservation) = self.binary.text().await?.into_parts();
            memory.absorb(reservation);
            FunctionResolutionError::UndefinedFunction { signature }
        } else {
            memory.grow(size_of::<OperatorResolutionError>())?;
            let (sqlstate, reservation) = self.binary.text().await?.into_parts();
            memory.absorb(reservation);
            let (message, reservation) = self.binary.text().await?.into_parts();
            memory.absorb(reservation);
            FunctionResolutionError::Operator(Box::new(OperatorResolutionError {
                sqlstate,
                message,
            }))
        };
        Ok(Budgeted::new(Some(Box::new(error)), memory))
    }

    pub(super) async fn function_binding(&mut self) -> Result<Budgeted<FunctionBinding>, SQLError> {
        let (mut value, mut memory) = self.function_binding_fields().await?.into_parts();
        value.dispatch = if self.flag().await? {
            let tag = self.binary.array::<1>().await?[0];
            Some(self.dispatch_payload(tag).await?)
        } else {
            None
        };
        let (error, reservation) = self.function_resolution_error().await?.into_parts();
        value.resolution_error = error;
        memory.absorb(reservation);
        Ok(Budgeted::new(value, memory))
    }

    async fn function_binding_image(&mut self) -> Result<Budgeted<FunctionBinding>, SQLError> {
        if self.header(super::super::FUNCTION_BINDING).await? != 1 {
            return Err(invalid("function binding count"));
        }
        let result = self.function_binding().await?;
        self.binary.end().await?;
        Ok(result)
    }
}

impl Input<'_> {
    pub(super) fn function_binding(&mut self) -> Result<Budgeted<FunctionBinding>, SQLError> {
        self.fields(async |mut input| input.function_binding().await)
    }
}

pub(in super::super) fn function_binding_image(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<Budgeted<FunctionBinding>, SQLError> {
    decode_image(reader, memory, cancellation, async |binary| {
        Fields {
            binary,
            retained_children: &mut 0,
        }
        .function_binding_image()
        .await
    })
}

impl FunctionBinding {
    /// Retain original selected dispatch and deferred diagnostics across byte/poll windows without resolving or invoking a function.
    pub fn image_input(
        memory: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<ImageDecoder<Budgeted<Self>, SQLError>, SQLError> {
        Ok(ImageDecoder::new(
            memory,
            cancellation,
            |mut reader| async move {
                Fields {
                    binary: reader.input(),
                    retained_children: &mut 0,
                }
                .function_binding_image()
                .await
            },
        )?)
    }
}
