//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use serde::{Deserialize, Serialize};

use super::RangeSubtype;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FunctionBinding {
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
    /// A typed overload-resolution failure retained until the expression reaches a fallible planning or execution boundary. This never reuses the SQL function-name namespace as an error channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution_error: Option<FunctionResolutionError>,
}

/// Static function-call failure discovered while binding declared argument types.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FunctionResolutionError {
    UndefinedFunction { signature: String },
}

/// Structural identity for parser-owned expressions and overload-specific built-in implementations. These variants occupy no SQL function-name namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FunctionDispatch {
    NamedArgument,
    VariadicArgument,
    ArraySubscripts,
    ArraySlices,
    Subscript,
    Slice,
    AnyOperator,
    AllOperator,
    IsDistinct,
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
    Range {
        operation: RangeFunctionOperation,
        subtype: RangeSubtype,
        multirange: bool,
    },
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
    Adjacent,
}

impl FunctionDispatch {
    /// Human-readable expression label used only in diagnostics and serialized plans; dispatch is always selected by the enum variant.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::NamedArgument => "named argument",
            Self::VariadicArgument => "VARIADIC argument",
            Self::ArraySubscripts | Self::Subscript => "subscript",
            Self::ArraySlices | Self::Slice => "slice",
            Self::AnyOperator => "ANY operator",
            Self::AllOperator => "ALL operator",
            Self::IsDistinct => "IS DISTINCT FROM",
            Self::BetweenSymmetric => "BETWEEN SYMMETRIC",
            Self::ToBinInt4 | Self::ToBinInt8 => "pg_catalog.to_bin",
            Self::ToHexInt4 | Self::ToHexInt8 => "pg_catalog.to_hex",
            Self::ToOctInt4 | Self::ToOctInt8 => "pg_catalog.to_oct",
            Self::RandomInt4Range | Self::RandomInt8Range | Self::RandomNumericRange => {
                "pg_catalog.random"
            }
            Self::ArraySortJson => "pg_catalog.array_sort",
            Self::Range { operation, .. } => operation.label(),
        }
    }

    #[must_use]
    pub const fn is_call_argument_marker(self) -> bool {
        matches!(self, Self::NamedArgument | Self::VariadicArgument)
    }

    /// Decode the compiler-private function spellings written into durable expressions by releases through 0.1.6. This is a catalog migration primitive, never a SQL routine lookup path.
    #[doc(hidden)]
    #[must_use]
    pub fn from_legacy_serialized_name(name: &str) -> Option<Self> {
        let fixed = match name {
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
    /// Construct the identity marker used when `PostgreSQL` parses a polymorphic syntax expression instead of an ordinary function call.
    #[must_use]
    pub fn polymorphic_builtin_syntax(name: &str) -> Self {
        assert!(Self::is_polymorphic_builtin_syntax_name(name));
        Self {
            name: name.into(),
            argument_types: Vec::new(),
            builtin: true,
            dispatch: None,
            invocation: None,
            resolution_error: None,
        }
    }

    /// Construct a parser- or binder-owned expression with an identity that cannot collide with a SQL routine name.
    #[must_use]
    pub fn dispatched(dispatch: FunctionDispatch) -> Self {
        Self {
            name: dispatch.label().into(),
            argument_types: Vec::new(),
            builtin: true,
            dispatch: Some(dispatch),
            invocation: None,
            resolution_error: None,
        }
    }

    /// Preserve an undefined-overload error structurally without fabricating a dispatch name.
    #[must_use]
    pub fn undefined_function(name: impl Into<String>, signature: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            argument_types: Vec::new(),
            builtin: false,
            dispatch: None,
            invocation: None,
            resolution_error: Some(FunctionResolutionError::UndefinedFunction {
                signature: signature.into(),
            }),
        }
    }

    /// Upgrade one function node deserialized from the catalog format written by releases through 0.1.6. Bound user routines are deliberately left untouched even when their SQL names resemble an old compiler marker.
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
