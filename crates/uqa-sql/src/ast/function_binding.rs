//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use serde::{Deserialize, Serialize};

use super::{EnumFunctionOperation, RangeSubtype};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FunctionBinding {
    /// Stable identity of a bound user routine. Built-ins and unbound calls leave this unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object_id: Option<[u8; 16]>,
    pub name: String,
    pub argument_types: Vec<String>,
    #[serde(default)]
    pub builtin: bool,
    /// Executor operation selected structurally during parsing or overload binding. SQL-visible routine lookup never consults display-name conventions for these operations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch: Option<FunctionDispatch>,
    /// Concrete invocation contract selected during routine overload resolution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation: Option<Box<RoutineInvocationBinding>>,
    /// Prepared field identity and its original result type; executable copies refresh its live descriptor without changing the result contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub composite_field: Option<Box<CompositeFieldBinding>>,
    /// A typed overload-resolution failure retained until the expression reaches a fallible planning or execution boundary. This never reuses the SQL function-name namespace as an error channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution_error: Option<Box<FunctionResolutionError>>,
}

/// A field selected during preparation. Attribute numbers are never reused, even when a later attribute has the same name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompositeFieldBinding {
    pub type_oid: u32,
    pub number: i16,
    pub result_type: super::ColumnType,
    /// The field's declaration at preparation, independently of the argument's collation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collation_oid: Option<u32>,
    pub dropped: bool,
    /// The live attribute type in an executable copy, when it differs from the original type OID. Type modifiers do not change `FieldSelect`'s result contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed_type: Option<super::ColumnType>,
}

/// Static function-call failure discovered while binding declared argument types.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FunctionResolutionError {
    UndefinedFunction { signature: String },
    Operator(Box<OperatorResolutionError>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorResolutionError {
    pub sqlstate: String,
    pub message: String,
}

impl FunctionResolutionError {
    #[must_use]
    pub fn sql_error(&self) -> crate::SQLError {
        match self {
            Self::UndefinedFunction { signature } => {
                crate::SQLError::undefined_function_call(signature)
            }
            Self::Operator(error) => crate::SQLError::Routine {
                sqlstate: error.sqlstate.clone(),
                message: error.message.clone(),
            },
        }
    }
}

/// Structural identity for parser-owned expressions and overload-specific built-in implementations. These variants occupy no SQL function-name namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FunctionDispatch {
    NumericOperator(NumericOperator),
    /// SQL pattern syntax, retaining its selected binary operator after escape processing.
    Pattern {
        operator: super::PatternOperator,
        oid: Option<std::num::NonZeroU32>,
    },
    /// A parser-owned escape helper, evaluated and folded independently of the strict match operator.
    PatternEscape {
        similar: bool,
    },
    /// A non-strict SQL Boolean test, distinct from routine lookup and equality.
    BooleanTest(BooleanTest),
    /// A collation clause, retaining its selected identity independently of its written name.
    Collate {
        oid: Option<u32>,
    },
    /// The selected argument metadata of `pg_collation_for`; the argument still executes before the observation.
    CollationFor {
        collation_oid: u32,
        noncollatable_type_oid: Option<u32>,
    },
    NamedArgument,
    VariadicArgument,
    ArraySubscripts,
    ArraySlices,
    /// Slice syntax with independently retained omitted endpoints.
    ArraySlicesWithBounds(super::ArraySliceBounds),
    Subscript,
    Slice,
    /// `(expression).field`: the field of a composite value, or the key of a document map.
    FieldSelect,
    AnyOperator,
    AllOperator,
    IsDistinct,
    /// One selected DISTINCT comparison of complete field values; nested ROW constructors remain one record-valued operand.
    ValueIsDistinct,
    /// The outer NULL test selected by DISTINCT against a bare NULL, without testing individual record fields or requiring equality.
    ValueNullTest {
        negated: bool,
    },
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
    ArraySortJson,
    /// SQL concatenation syntax, with its selected catalog overload after operand analysis.
    Concat {
        oid: Option<u32>,
    },
    /// The selected array `||` overload, retained even when its arguments evaluate to NULL.
    ArrayConcat,
    ArrayAppend,
    ArrayPrepend,
    /// A resolved JSON path function whose final argument is the declared text array.
    JsonPath {
        as_text: bool,
        jsonb: bool,
    },
    JsonExtract {
        as_text: bool,
        path: bool,
    },
    Range {
        operation: RangeFunctionOperation,
        subtype: RangeSubtype,
        multirange: bool,
    },
    /// An `anyenum` support function bound to one concrete enum type.
    Enum {
        operation: EnumFunctionOperation,
        type_oid: u32,
    },
    /// The current MERGE action, read from the executor-owned row attribute.
    MergeAction,
}

/// The six predicates over SQL's true, false and NULL Boolean inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BooleanTest {
    IsTrue,
    IsNotTrue,
    IsFalse,
    IsNotFalse,
    IsUnknown,
    IsNotUnknown,
}

impl BooleanTest {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::IsTrue => "IS TRUE",
            Self::IsNotTrue => "IS NOT TRUE",
            Self::IsFalse => "IS FALSE",
            Self::IsNotFalse => "IS NOT FALSE",
            Self::IsUnknown => "IS UNKNOWN",
            Self::IsNotUnknown => "IS NOT UNKNOWN",
        }
    }

    /// Stable `PostgreSQL` catalog node identity, independent of parser protobuf tags.
    #[must_use]
    pub const fn catalog_code(self) -> i64 {
        match self {
            Self::IsTrue => 0,
            Self::IsNotTrue => 1,
            Self::IsFalse => 2,
            Self::IsNotFalse => 3,
            Self::IsUnknown => 4,
            Self::IsNotUnknown => 5,
        }
    }

    #[must_use]
    pub const fn from_catalog_code(code: i64) -> Option<Self> {
        match code {
            0 => Some(Self::IsTrue),
            1 => Some(Self::IsNotTrue),
            2 => Some(Self::IsFalse),
            3 => Some(Self::IsNotFalse),
            4 => Some(Self::IsUnknown),
            5 => Some(Self::IsNotUnknown),
            _ => None,
        }
    }
}

/// Operation selected for one typed range or multirange call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RangeFunctionOperation {
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
    /// The range operand is first; the second operand has its declared subtype.
    ContainsElement,
    /// The range operand is second; the first operand has its declared subtype.
    ElementContainedBy,
    Adjacent,
}

impl FunctionDispatch {
    /// Whether this expression retains SQL concatenation syntax and operand roles.
    #[must_use]
    pub const fn is_concatenation(self) -> bool {
        matches!(
            self,
            Self::Concat { .. } | Self::ArrayConcat | Self::ArrayAppend | Self::ArrayPrepend
        )
    }

    /// Human-readable expression label used only in diagnostics and serialized plans; dispatch is always selected by the enum variant.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::MergeAction => "merge_action",
            Self::NumericOperator(operator) => operator.symbol(),
            Self::Pattern { operator, .. } => operator.syntax(),
            Self::PatternEscape { similar: false } => "pg_catalog.like_escape",
            Self::PatternEscape { similar: true } => "pg_catalog.similar_to_escape",
            Self::BooleanTest(test) => test.label(),
            Self::Collate { .. } => "COLLATE",
            Self::CollationFor { .. } => "pg_catalog.pg_collation_for",
            Self::NamedArgument => "named argument",
            Self::VariadicArgument => "VARIADIC argument",
            Self::ArraySubscripts | Self::Subscript => "subscript",
            Self::ArraySlices | Self::ArraySlicesWithBounds(_) | Self::Slice => "slice",
            Self::FieldSelect => "field selection",
            Self::AnyOperator => "ANY operator",
            Self::AllOperator => "ALL operator",
            Self::IsDistinct | Self::ValueIsDistinct => "IS DISTINCT FROM",
            Self::ValueNullTest { negated: false } => "IS NULL",
            Self::ValueNullTest { negated: true } => "IS NOT NULL",
            Self::BetweenSymmetric => "BETWEEN SYMMETRIC",
            Self::ToBinInt4 | Self::ToBinInt8 => "pg_catalog.to_bin",
            Self::ToHexInt4 | Self::ToHexInt8 => "pg_catalog.to_hex",
            Self::ToOctInt4 | Self::ToOctInt8 => "pg_catalog.to_oct",
            Self::RandomInt4Range | Self::RandomInt8Range | Self::RandomNumericRange => {
                "pg_catalog.random"
            }
            Self::ArraySortJson => "pg_catalog.array_sort",
            Self::Concat { .. } => "||",
            Self::ArrayConcat => "pg_catalog.array_cat",
            Self::ArrayAppend => "pg_catalog.array_append",
            Self::ArrayPrepend => "pg_catalog.array_prepend",
            Self::JsonPath {
                as_text: false,
                jsonb: false,
            } => "pg_catalog.json_extract_path",
            Self::JsonPath {
                as_text: true,
                jsonb: false,
            } => "pg_catalog.json_extract_path_text",
            Self::JsonPath {
                as_text: false,
                jsonb: true,
            } => "pg_catalog.jsonb_extract_path",
            Self::JsonPath {
                as_text: true,
                jsonb: true,
            } => "pg_catalog.jsonb_extract_path_text",
            Self::JsonExtract { as_text: false, .. } => "JSON extraction operator",
            Self::JsonExtract { as_text: true, .. } => "JSON text extraction operator",
            Self::Range { operation, .. } => operation.label(),
            Self::Enum { operation, .. } => operation.label(),
        }
    }

    #[must_use]
    pub const fn is_call_argument_marker(self) -> bool {
        matches!(self, Self::NamedArgument | Self::VariadicArgument)
    }

    /// The SQL spelling of a retained JSON extraction operator, independent of its diagnostic label.
    #[must_use]
    pub const fn json_extraction_symbol(self) -> Option<&'static str> {
        match self {
            Self::JsonExtract {
                as_text: false,
                path: false,
            } => Some("->"),
            Self::JsonExtract {
                as_text: true,
                path: false,
            } => Some("->>"),
            Self::JsonExtract {
                as_text: false,
                path: true,
            } => Some("#>"),
            Self::JsonExtract {
                as_text: true,
                path: true,
            } => Some("#>>"),
            _ => None,
        }
    }

    /// Decode function spellings in older durable expressions. This is a catalog migration primitive, never a SQL routine lookup path.
    #[doc(hidden)]
    #[must_use]
    pub fn from_legacy_serialized_name(name: &str) -> Option<Self> {
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
            _ => return Self::legacy_range_dispatch(name),
        };
        Some(fixed)
    }

    fn legacy_range_dispatch(name: &str) -> Option<Self> {
        let encoded = name.strip_prefix("__range_")?;
        let subtypes = [
            RangeSubtype::Integer,
            RangeSubtype::BigInteger,
            RangeSubtype::Numeric,
            RangeSubtype::Date,
            RangeSubtype::Timestamp,
            RangeSubtype::TimestampTz,
        ];
        for subtype in subtypes {
            for (type_name, multirange) in [
                (subtype.multirange_name(), true),
                (subtype.range_name(), false),
            ] {
                let Some(operation) = encoded.strip_suffix(type_name) else {
                    continue;
                };
                let operation = match operation.trim_end_matches('_') {
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
                    _ => continue,
                };
                return Some(Self::Range {
                    operation,
                    subtype,
                    multirange,
                });
            }
        }
        None
    }
}

/// Numeric operator syntax, kept separate from ordinary calls such as `mod` or `abs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NumericOperator {
    Modulo,
    Power,
    Plus,
    SquareRoot,
    CubeRoot,
    Absolute,
}

impl NumericOperator {
    #[must_use]
    pub const fn symbol(self) -> &'static str {
        match self {
            Self::Modulo => "%",
            Self::Power => "^",
            Self::Plus => "+",
            Self::SquareRoot => "|/",
            Self::CubeRoot => "||/",
            Self::Absolute => "@",
        }
    }

    #[must_use]
    pub const fn arity(self) -> usize {
        match self {
            Self::Modulo | Self::Power => 2,
            Self::Plus | Self::SquareRoot | Self::CubeRoot | Self::Absolute => 1,
        }
    }
}

impl RangeFunctionOperation {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Lower => "pg_catalog.lower",
            Self::Upper => "pg_catalog.upper",
            Self::IsEmpty => "pg_catalog.isempty",
            Self::LowerInclusive => "pg_catalog.lower_inc",
            Self::UpperInclusive => "pg_catalog.upper_inc",
            Self::LowerInfinite => "pg_catalog.lower_inf",
            Self::UpperInfinite => "pg_catalog.upper_inf",
            Self::Merge => "pg_catalog.range_merge",
            Self::Multirange => "pg_catalog.multirange",
            Self::Overlap => "range overlap operator",
            Self::Contains => "range contains operator",
            Self::ContainedBy => "range contained-by operator",
            Self::ContainsElement => "range contains element operator",
            Self::ElementContainedBy => "element contained-by range operator",
            Self::Adjacent => "range adjacent operator",
        }
    }
}

/// Concrete parameter and result types selected for one routine invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutineInvocationBinding {
    /// Zero-based declared parameter index for each call argument, aligned with the call argument list.
    pub argument_positions: Vec<usize>,
    /// Concrete coercion target for each call argument, aligned with the call argument list.
    pub argument_targets: Vec<String>,
    /// Declared source types before argument coercion; absent only in legacy bindings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub argument_sources: Vec<Option<String>>,
    /// Concrete type for each declared parameter, aligned with [`crate::ast::CreateFunction::params`].
    pub parameter_types: Vec<String>,
    /// Concrete invocation result type after polymorphic substitution.
    pub return_type: Option<String>,
    /// Whether and where the declared variadic parameter participates in this invocation.
    pub variadic_mode: RoutineVariadicMode,
}

/// Call syntax selected for a routine's variadic parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum RoutineVariadicMode {
    /// The invocation does not use a variadic parameter.
    #[default]
    None,
    /// Trailing call arguments are expanded into the declared variadic array parameter.
    Expanded {
        /// Zero-based index in [`crate::ast::CreateFunction::params`].
        parameter_index: usize,
    },
    /// An explicit `VARIADIC` array argument supplies the declared variadic parameter.
    Explicit {
        /// Zero-based index in [`crate::ast::CreateFunction::params`].
        parameter_index: usize,
    },
}

impl FunctionBinding {
    /// Preserve the predecessor format's encoded omission before an optimizer can fold supplied expressions to NULL.
    #[doc(hidden)]
    pub fn upgrade_legacy_array_slices(
        binding: &mut Option<Self>,
        arguments: usize,
        is_bare_null: impl Fn(usize) -> bool,
    ) -> bool {
        let Some(binding) = binding
            .as_mut()
            .filter(|binding| binding.dispatch == Some(FunctionDispatch::ArraySlices))
        else {
            return false;
        };
        let Some(Ok(access)) = super::ArrayAccess::from_dispatch(
            FunctionDispatch::ArraySlices,
            arguments,
            is_bare_null,
        ) else {
            return false;
        };
        binding.dispatch = Some(FunctionDispatch::ArraySlicesWithBounds(
            access.slice_bounds().expect("slice bounds"),
        ));
        true
    }

    /// Construct the identity marker used when `PostgreSQL` parses a polymorphic syntax expression instead of an ordinary function call.
    #[must_use]
    pub fn polymorphic_builtin_syntax(name: &str) -> Self {
        assert!(Self::is_polymorphic_builtin_syntax_name(name));
        Self {
            object_id: None,
            name: name.into(),
            argument_types: Vec::new(),
            builtin: true,
            dispatch: None,
            invocation: None,
            composite_field: None,
            resolution_error: None,
        }
    }

    /// Construct a parser- or binder-owned expression with an identity that cannot collide with a SQL routine name.
    #[must_use]
    pub fn dispatched(dispatch: FunctionDispatch) -> Self {
        Self::dispatched_with_control(
            dispatch,
            &uqa_core::memory::ProductionControl::uncontrolled(),
        )
        .expect("ordinary dispatch constructor cannot be cancelled or limited")
        .into_uncontrolled()
        .expect("ordinary dispatch owner")
    }

    pub fn dispatched_with_control(
        dispatch: FunctionDispatch,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_core::memory::Produced<Self>, uqa_core::ValueRetentionError> {
        let (name, memory) = control.copy_text(dispatch.label())?.into_parts();
        control.finish(
            Self {
                object_id: None,
                name,
                argument_types: Vec::new(),
                builtin: true,
                dispatch: Some(dispatch),
                invocation: None,
                composite_field: None,
                resolution_error: None,
            },
            memory,
        )
    }

    /// Preserve an undefined-overload error structurally without fabricating a dispatch name.
    #[must_use]
    pub fn undefined_function(name: impl Into<String>, signature: impl Into<String>) -> Self {
        Self {
            object_id: None,
            name: name.into(),
            argument_types: Vec::new(),
            builtin: false,
            dispatch: None,
            invocation: None,
            composite_field: None,
            resolution_error: Some(Box::new(FunctionResolutionError::UndefinedFunction {
                signature: signature.into(),
            })),
        }
    }

    /// Upgrade one function node from older serialized catalog syntax. Bound user routines are left untouched even when their SQL names resemble an old compiler marker.
    #[doc(hidden)]
    pub fn upgrade_legacy_serialized_dispatch(
        display_name: &mut String,
        binding: &mut Option<Self>,
    ) -> bool {
        if binding
            .as_ref()
            .is_some_and(|binding| binding.dispatch.is_some() || !binding.builtin)
        {
            return false;
        }
        let Some(dispatch) = FunctionDispatch::from_legacy_serialized_name(display_name) else {
            return false;
        };
        if let Some(binding) = binding {
            binding.dispatch = Some(dispatch);
            display_name.clone_from(&binding.name);
        } else {
            let upgraded = Self::dispatched(dispatch);
            display_name.clone_from(&upgraded.name);
            *binding = Some(upgraded);
        }
        true
    }

    /// Return whether this binding marks a polymorphic syntax expression whose argument types must be inferred from its operands.
    #[must_use]
    pub fn is_polymorphic_builtin_syntax(&self) -> bool {
        self.builtin
            && self.argument_types.is_empty()
            && Self::is_polymorphic_builtin_syntax_name(&self.name)
    }

    /// Return whether an unqualified local name belongs to `PostgreSQL`'s polymorphic function-like syntax expressions.
    #[must_use]
    pub fn is_polymorphic_builtin_syntax_name(name: &str) -> bool {
        matches!(name, "coalesce" | "greatest" | "least" | "nullif")
    }
}

pub type GeneratedFunctionDependency = FunctionBinding;
