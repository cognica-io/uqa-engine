//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stable image tags preserve native operation identities independently of Rust enum layout.

use crate::{
    ast::{
        BooleanTest, EnumFunctionOperation, FunctionDispatch, NumericOperator, PatternOperator,
        RangeFunctionOperation, RangeSubtype,
    },
    SQLError,
};

pub(super) trait OperationTag: Sized {
    fn image_tag(self) -> u8;
    fn from_image_tag(tag: u8) -> Result<Self, SQLError>;
}

macro_rules! operation_tags {
    ($type:ty, $($variant:ident = $tag:literal),+ $(,)?) => {
        impl OperationTag for $type {
            fn image_tag(self) -> u8 {
                match self { $(Self::$variant => $tag,)+ }
            }
            fn from_image_tag(tag: u8) -> Result<Self, SQLError> {
                match tag {
                    $($tag => Ok(Self::$variant),)+
                    _ => Err(super::invalid("function operation tag")),
                }
            }
        }
    };
}

operation_tags!(
    NumericOperator,
    Modulo = 0,
    Power = 1,
    Plus = 2,
    SquareRoot = 3,
    CubeRoot = 4,
    Absolute = 5,
    BitAnd = 6,
    BitOr = 7,
    BitXor = 8,
    BitNot = 9,
    ShiftLeft = 10,
    ShiftRight = 11,
);
operation_tags!(
    PatternOperator,
    Like = 0,
    NotLike = 1,
    ILike = 2,
    NotILike = 3,
    Regex = 4,
    NotRegex = 5,
    RegexInsensitive = 6,
    NotRegexInsensitive = 7,
    StartsWith = 8,
);
operation_tags!(
    BooleanTest,
    IsTrue = 0,
    IsNotTrue = 1,
    IsFalse = 2,
    IsNotFalse = 3,
    IsUnknown = 4,
    IsNotUnknown = 5,
);
operation_tags!(
    RangeFunctionOperation,
    Lower = 0,
    Upper = 1,
    IsEmpty = 2,
    LowerInclusive = 3,
    UpperInclusive = 4,
    LowerInfinite = 5,
    UpperInfinite = 6,
    Merge = 7,
    Multirange = 8,
    Overlap = 9,
    Contains = 10,
    ContainedBy = 11,
    ContainsElement = 12,
    ElementContainedBy = 13,
    Adjacent = 14,
);
operation_tags!(
    RangeSubtype,
    Integer = 0,
    BigInteger = 1,
    Numeric = 2,
    Date = 3,
    Timestamp = 4,
    TimestampTz = 5,
);
operation_tags!(
    EnumFunctionOperation,
    First = 0,
    Last = 1,
    Range = 2,
    BoundedRange = 3,
    Compare = 4,
    Equal = 5,
    NotEqual = 6,
    Less = 7,
    Greater = 8,
    LessEqual = 9,
    GreaterEqual = 10,
    Smaller = 11,
    Larger = 12,
    Hash = 13,
    ExtendedHash = 14,
);

pub(super) fn dispatch_tag(value: FunctionDispatch) -> u8 {
    use FunctionDispatch as D;
    match value {
        D::NumericOperator(_) => 0,
        D::Pattern { .. } => 1,
        D::PatternEscape { .. } => 2,
        D::BooleanTest(_) => 3,
        D::Collate { .. } => 4,
        D::CollationFor { .. } => 5,
        D::TypeOf { .. } => 6,
        D::NamedArgument => 7,
        D::VariadicArgument => 8,
        D::ArraySubscripts => 9,
        D::ArraySlices => 10,
        D::ArraySlicesWithBounds(_) => 11,
        D::Subscript => 12,
        D::Slice => 13,
        D::FieldSelect => 14,
        D::AssignmentStore => 15,
        D::AnyOperator => 16,
        D::AllOperator => 17,
        D::IsDistinct => 18,
        D::DistinctNot => 19,
        D::RowDistinct => 20,
        D::ValueIsDistinct => 21,
        D::ValueNullTest { .. } => 22,
        D::BetweenSymmetric => 23,
        D::ToBinInt4 => 24,
        D::ToBinInt8 => 25,
        D::ToHexInt4 => 26,
        D::ToHexInt8 => 27,
        D::ToOctInt4 => 28,
        D::ToOctInt8 => 29,
        D::RandomInt4Range => 30,
        D::RandomInt8Range => 31,
        D::RandomNumericRange => 32,
        D::RegexSubstring => 33,
        D::SimilarSubstring => 34,
        D::ArraySortJson => 35,
        D::Concat { .. } => 36,
        D::ArrayConcat => 37,
        D::ArrayAppend => 38,
        D::ArrayPrepend => 39,
        D::JsonPath { .. } => 40,
        D::JsonExtract { .. } => 41,
        D::Range { .. } => 42,
        D::Enum { .. } => 43,
        D::MergeAction => 44,
    }
}
