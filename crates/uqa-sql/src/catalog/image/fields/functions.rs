//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected operations and deferred errors are native metadata, not names to resolve again.

use super::super::function_dispatch::{dispatch_tag, OperationTag};
use super::Fields;
use crate::ast::{FunctionDispatch, FunctionResolutionError, OperatorResolutionError};

impl<'a> Fields<'a> {
    pub fn function_dispatch(&mut self, value: Option<FunctionDispatch>) {
        self.flag(value.is_some());
        let Some(value) = value else { return };
        self.scalar([dispatch_tag(value)]);
        self.dispatch_payload(value);
    }

    fn dispatch_payload(&mut self, value: FunctionDispatch) {
        use FunctionDispatch as D;
        match value {
            D::NumericOperator(operator) => self.scalar([operator.image_tag()]),
            D::Pattern { operator, oid } => {
                self.scalar([operator.image_tag()]);
                self.optional_u32(oid.map(std::num::NonZeroU32::get));
            }
            D::PatternEscape { similar } => self.flag(similar),
            D::BooleanTest(test) => self.scalar([test.image_tag()]),
            D::Collate { oid } | D::Concat { oid } => self.optional_u32(oid),
            D::CollationFor {
                collation_oid,
                noncollatable_type_oid,
            } => {
                self.scalar(collation_oid.to_le_bytes());
                self.optional_u32(noncollatable_type_oid);
            }
            D::TypeOf { type_oid } => self.scalar(type_oid.to_le_bytes()),
            D::ArraySlicesWithBounds(bounds) => self.scalar(bounds.stored_masks()),
            D::ValueNullTest { negated } => self.flag(negated),
            D::JsonPath { as_text, jsonb } => {
                self.flag(as_text);
                self.flag(jsonb);
            }
            D::JsonExtract { as_text, path } => {
                self.flag(as_text);
                self.flag(path);
            }
            D::Range {
                operation,
                subtype,
                multirange,
            } => {
                self.scalar([operation.image_tag(), subtype.image_tag()]);
                self.flag(multirange);
            }
            D::Enum {
                operation,
                type_oid,
            } => {
                self.scalar([operation.image_tag()]);
                self.scalar(type_oid.to_le_bytes());
            }
            D::NamedArgument
            | D::VariadicArgument
            | D::ArraySubscripts
            | D::ArraySlices
            | D::Subscript
            | D::Slice
            | D::FieldSelect
            | D::AssignmentStore
            | D::AnyOperator
            | D::AllOperator
            | D::IsDistinct
            | D::DistinctNot
            | D::RowDistinct
            | D::ValueIsDistinct
            | D::BetweenSymmetric
            | D::ToBinInt4
            | D::ToBinInt8
            | D::ToHexInt4
            | D::ToHexInt8
            | D::ToOctInt4
            | D::ToOctInt8
            | D::RandomInt4Range
            | D::RandomInt8Range
            | D::RandomNumericRange
            | D::RegexSubstring
            | D::SimilarSubstring
            | D::ArraySortJson
            | D::ArrayConcat
            | D::ArrayAppend
            | D::ArrayPrepend
            | D::MergeAction => {}
        }
    }

    pub fn function_resolution_error(&mut self, value: Option<&'a FunctionResolutionError>) {
        match value {
            None => self.scalar([0]),
            Some(FunctionResolutionError::UndefinedFunction { signature }) => {
                self.scalar([1]);
                self.text(signature);
            }
            Some(FunctionResolutionError::Operator(error)) => {
                let OperatorResolutionError { sqlstate, message } = &**error;
                self.scalar([2]);
                self.text(sqlstate);
                self.text(message);
            }
        }
    }
}
