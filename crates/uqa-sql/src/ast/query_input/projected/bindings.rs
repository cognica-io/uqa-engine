//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::ast::{
    AssignmentBinding, AssignmentPathStep, BooleanTest, CompositeFieldBinding, FunctionDispatch,
    FunctionResolutionError, FunctionValueBinding, NumericOperator, OperatorResolutionError,
    RangeFunctionOperation, RoutineInvocationBinding, RoutineVariadicMode,
};

projected_enum!(FunctionValueBinding {
    CompositeField as "composite_field"(value),
    Assignment as "assignment"(value)
});

projected_struct!(CompositeFieldBinding {
    type_oid: required,
    number: required,
    result_type: required,
    collation_oid: defaulted,
    dropped: required,
    changed_type: defaulted
});

projected_enum!(FunctionResolutionError {
    UndefinedFunction { signature: required },
    Operator(value)
});

projected_struct!(OperatorResolutionError {
    sqlstate: required,
    message: required
});

projected_enum!(FunctionDispatch {
    NumericOperator(value),
    Pattern { operator: required, oid: optional },
    PatternEscape { similar: required },
    BooleanTest(value),
    Collate { oid: optional },
    CollationFor { collation_oid: required, noncollatable_type_oid: optional },
    TypeOf { type_oid: required },
    NamedArgument,
    VariadicArgument,
    ArraySubscripts,
    ArraySlices,
    ArraySlicesWithBounds(value),
    Subscript,
    Slice,
    FieldSelect,
    AssignmentStore,
    AnyOperator,
    AllOperator,
    IsDistinct,
    DistinctNot,
    RowDistinct,
    ValueIsDistinct,
    ValueNullTest { negated: required },
    BetweenSymmetric,
    ToBinInt4,
    ToBinInt8,
    ToHexInt4,
    ToHexInt8,
    ToOctInt4,
    ToOctInt8,
    RandomInt4Range,
    RandomInt8Range,
    RandomNumericRange,
    RegexSubstring,
    SimilarSubstring,
    ArraySortJson,
    Concat { oid: optional },
    ArrayConcat,
    ArrayAppend,
    ArrayPrepend,
    JsonPath { as_text: required, jsonb: required },
    JsonExtract { as_text: required, path: required },
    Range { operation: required, subtype: required, multirange: required },
    Enum { operation: required, type_oid: required },
    MergeAction
});

projected_enum!(BooleanTest {
    IsTrue,
    IsNotTrue,
    IsFalse,
    IsNotFalse,
    IsUnknown,
    IsNotUnknown
});

projected_enum!(RangeFunctionOperation {
    Lower,
    Upper,
    IsEmpty,
    LowerInclusive,
    UpperInclusive,
    LowerInfinite,
    UpperInfinite,
    Merge,
    Multirange,
    Overlap,
    Contains,
    ContainedBy,
    ContainsElement,
    ElementContainedBy,
    Adjacent
});

projected_enum!(NumericOperator {
    Modulo,
    Power,
    Plus,
    SquareRoot,
    CubeRoot,
    Absolute,
    BitAnd,
    BitOr,
    BitXor,
    BitNot,
    ShiftLeft,
    ShiftRight
});

projected_struct!(RoutineInvocationBinding {
    argument_positions: required,
    argument_targets: required,
    argument_sources: defaulted,
    parameter_types: required,
    return_type: optional,
    variadic_mode: required
});

projected_enum!(RoutineVariadicMode {
    None,
    Expanded { parameter_index: required },
    Explicit { parameter_index: required }
});

projected_struct!(AssignmentBinding {
    path: required,
    value_argument: required
});

projected_enum!(AssignmentPathStep {
    Field { type_oid: required, number: required },
    Subscripts { first_argument: required, dimensions: required, slice: required, bounds: required }
});
