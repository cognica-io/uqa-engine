//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, Result, SyntaxCopy};

syntax_enum!(crate::ast::BooleanTest {
    IsTrue,
    IsNotTrue,
    IsFalse,
    IsNotFalse,
    IsUnknown,
    IsNotUnknown
});

syntax_struct!(crate::ast::CompositeFieldBinding {
    type_oid,
    number,
    result_type,
    collation_oid,
    dropped,
    changed_type
});

syntax_struct!(crate::ast::FunctionBinding {
    object_id,
    name,
    argument_types,
    builtin,
    dispatch,
    invocation,
    value_binding,
    resolution_error,
    input_collation
});

syntax_enum!(crate::ast::FunctionDispatch {
    NumericOperator(value0),
    Pattern { operator, oid },
    PatternEscape { similar },
    BooleanTest(value0),
    Collate { oid },
    CollationFor { collation_oid, noncollatable_type_oid },
    TypeOf { type_oid },
    NamedArgument,
    VariadicArgument,
    ArraySubscripts,
    ArraySlices,
    ArraySlicesWithBounds(value0),
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
    ValueNullTest { negated },
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
    Concat { oid },
    ArrayConcat,
    ArrayAppend,
    ArrayPrepend,
    JsonPath { as_text, jsonb },
    JsonExtract { as_text, path },
    Range { operation, subtype, multirange },
    Enum { operation, type_oid },
    MergeAction
});

syntax_enum!(crate::ast::FunctionResolutionError {
    UndefinedFunction { signature },
    Operator(value0)
});

syntax_enum!(crate::ast::FunctionValueBinding {
    CompositeField(value0),
    Assignment(value0)
});

syntax_enum!(crate::ast::NumericOperator {
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

syntax_struct!(crate::ast::OperatorResolutionError { sqlstate, message });

syntax_enum!(crate::ast::RangeFunctionOperation {
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

syntax_struct!(crate::ast::RoutineInvocationBinding {
    argument_positions,
    argument_targets,
    argument_sources,
    parameter_types,
    return_type,
    variadic_mode
});

syntax_enum!(crate::ast::RoutineVariadicMode {
    None,
    Expanded { parameter_index },
    Explicit { parameter_index }
});
