//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordinary migration and retained observation share marker spelling and range-operation selection.

use super::{FunctionBinding, FunctionDispatch, RangeFunctionOperation, RangeSubtype};
use uqa_core::{
    image_io::{AsyncImageInput, ImageIoError},
    CancellationToken,
};

impl FunctionDispatch {
    /// Decode function spellings in older durable expressions. This is a catalog migration primitive, never a SQL routine lookup path.
    #[doc(hidden)]
    #[must_use]
    pub fn from_legacy_serialized_name(name: &str) -> Option<Self> {
        Self::fixed_legacy_name(name).or_else(|| {
            range_candidates(name).find_map(|(operation, subtype, multirange)| {
                range_dispatch(operation.trim_end_matches('_'), subtype, multirange)
            })
        })
    }

    async fn legacy_name_from_input(
        name: &str,
        original: &CancellationToken,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<Option<Self>, ImageIoError> {
        original.check()?;
        input.checkpoint().await?;
        original.check()?;
        if let Some(dispatch) = Self::fixed_legacy_name(name) {
            return Ok(Some(dispatch));
        }
        for (operation, subtype, multirange) in range_candidates(name) {
            let mut end = operation.len();
            while end > 0 {
                original.check()?;
                input.checkpoint().await?;
                original.check()?;
                let begin = end.saturating_sub(4096);
                while end > begin && operation.as_bytes()[end - 1] == b'_' {
                    end -= 1;
                }
                if end > begin {
                    break;
                }
            }
            // Only ASCII underscores were removed, so the selected boundary is valid UTF-8.
            if let Some(dispatch) = range_dispatch(&operation[..end], subtype, multirange) {
                return Ok(Some(dispatch));
            }
        }
        Ok(None)
    }
}

fn range_candidates(name: &str) -> impl Iterator<Item = (&str, RangeSubtype, bool)> {
    let encoded = name.strip_prefix("__range_");
    [
        RangeSubtype::Integer,
        RangeSubtype::BigInteger,
        RangeSubtype::Numeric,
        RangeSubtype::Date,
        RangeSubtype::Timestamp,
        RangeSubtype::TimestampTz,
    ]
    .into_iter()
    .flat_map(|subtype| {
        [
            (subtype.multirange_name(), subtype, true),
            (subtype.range_name(), subtype, false),
        ]
    })
    .filter_map(move |(name, subtype, multirange)| {
        encoded?
            .strip_suffix(name)
            .map(|operation| (operation, subtype, multirange))
    })
}

fn range_dispatch(
    operation: &str,
    subtype: RangeSubtype,
    multirange: bool,
) -> Option<FunctionDispatch> {
    let operation = match operation {
        "lower" => RangeFunctionOperation::Lower,
        "upper" => RangeFunctionOperation::Upper,
        "isempty" => RangeFunctionOperation::IsEmpty,
        "lower_inc" => RangeFunctionOperation::LowerInclusive,
        "upper_inc" => RangeFunctionOperation::UpperInclusive,
        "lower_inf" => RangeFunctionOperation::LowerInfinite,
        "upper_inf" => RangeFunctionOperation::UpperInfinite,
        "merge" => RangeFunctionOperation::Merge,
        "multirange" => RangeFunctionOperation::Multirange,
        "overlap" => RangeFunctionOperation::Overlap,
        "contains" => RangeFunctionOperation::Contains,
        "contained_by" => RangeFunctionOperation::ContainedBy,
        "adjacent" => RangeFunctionOperation::Adjacent,
        _ => return None,
    };
    Some(FunctionDispatch::Range {
        operation,
        subtype,
        multirange,
    })
}

impl FunctionBinding {
    pub(super) fn accepts_legacy_dispatch(binding: Option<&Self>) -> bool {
        !binding.is_some_and(|binding| binding.dispatch.is_some() || !binding.builtin)
    }

    pub(crate) async fn legacy_serialized_dispatch_from_input(
        name: &str,
        binding: Option<&Self>,
        original: &CancellationToken,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<Option<FunctionDispatch>, ImageIoError> {
        if Self::accepts_legacy_dispatch(binding) {
            FunctionDispatch::legacy_name_from_input(name, original, input).await
        } else {
            Ok(None)
        }
    }
}

impl FunctionDispatch {
    fn fixed_legacy_name(name: &str) -> Option<Self> {
        let fixed = match name {
            "merge_action" => Self::MergeAction,
            "__named_arg" => Self::NamedArgument,
            "__variadic_arg" => Self::VariadicArgument,
            "__array_subscripts" => Self::ArraySubscripts,
            "__array_slices" => Self::ArraySlices,
            "__subscript" => Self::Subscript,
            "__slice" => Self::Slice,
            "__any_op" => Self::AnyOperator,
            "__all_op" => Self::AllOperator,
            "__is_distinct" => Self::IsDistinct,
            "__between_symmetric" => Self::BetweenSymmetric,
            "__to_bin_int4" => Self::ToBinInt4,
            "__to_bin_int8" => Self::ToBinInt8,
            "__to_hex_int4" => Self::ToHexInt4,
            "__to_hex_int8" => Self::ToHexInt8,
            "__to_oct_int4" => Self::ToOctInt4,
            "__to_oct_int8" => Self::ToOctInt8,
            "__random_int4_range" => Self::RandomInt4Range,
            "__random_int8_range" => Self::RandomInt8Range,
            "__random_numeric_range" => Self::RandomNumericRange,
            "__array_sort_json" => Self::ArraySortJson,
            _ => return None,
        };
        Some(fixed)
    }
}
