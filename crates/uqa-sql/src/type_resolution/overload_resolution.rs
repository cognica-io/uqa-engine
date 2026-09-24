//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared `PostgreSQL` function-candidate matching and ranking rules.

use crate::ast::{ColumnType, FunctionBinding};
use crate::SQLError;

use super::BuiltinFunctionOverload;

/// Candidate information used by `PostgreSQL`'s exact-match, preferred-type, and unknown-category ranking passes.
pub trait RankedFunctionMatch {
    fn argument_types(&self) -> &[String];
    fn raw_exact_matches(&self) -> usize;
    fn exact_matches(&self) -> usize;
    fn preferred_matches(&self) -> usize;

    fn is_variadic_expansion(&self) -> bool {
        false
    }
}

/// One declared parameter in a function signature used for structural matching and type scoring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionParameterDescriptor {
    pub name: Option<String>,
    pub type_name: String,
    pub has_default: bool,
}

/// Structural and type-scoring result shared by built-in and catalog routine candidates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchedFunctionSignature {
    pub argument_types: Vec<String>,
    pub argument_positions: Vec<usize>,
    pub raw_exact_matches: usize,
    pub exact_matches: usize,
    pub preferred_matches: usize,
}

pub(super) mod candidates;
mod signature;
pub use signature::match_function_signature;
pub(super) use signature::{match_signature_with_control, SignatureParameters};

mod type_names;
pub use type_names::{canonical_column_type_name, canonical_routine_type_name};
pub(super) use type_names::{
    canonical_column_type_name_with_control, canonical_routine_type_name_with_control,
};

#[must_use]
pub fn routine_type_accepts_implicit_cast(actual: &str, declared: &str) -> bool {
    if actual == declared {
        return true;
    }
    if declared == "anyarray"
        && (actual.ends_with("[]") || matches!(actual, "int2vector" | "oidvector"))
    {
        return true;
    }
    if let (Some(actual), Some(declared)) = (actual.strip_suffix("[]"), declared.strip_suffix("[]"))
    {
        return routine_type_accepts_implicit_cast(actual, declared);
    }
    matches!(
        (actual, declared),
        (
            "int2",
            "int4"
                | "int8"
                | "float4"
                | "float8"
                | "numeric"
                | "oid"
                | "regclass"
                | "regnamespace"
                | "regproc"
                | "regrole"
                | "regtype",
        ) | (
            "int4",
            "int8"
                | "float4"
                | "float8"
                | "numeric"
                | "oid"
                | "regclass"
                | "regnamespace"
                | "regproc"
                | "regrole"
                | "regtype",
        ) | (
            "int8",
            "float4"
                | "float8"
                | "numeric"
                | "oid"
                | "regclass"
                | "regnamespace"
                | "regproc"
                | "regrole"
                | "regtype",
        ) | (
            "oid",
            "regclass" | "regnamespace" | "regproc" | "regrole" | "regtype",
        ) | (
            "regclass" | "regnamespace" | "regproc" | "regrole" | "regtype",
            "oid",
        ) | ("numeric", "float4" | "float8")
            | ("float4", "float8")
            | ("bpchar", "varchar" | "name" | "text")
            | ("varchar", "bpchar" | "name" | "text" | "regclass")
            | ("text", "bpchar" | "varchar" | "name" | "regclass")
            | ("name" | "\"char\"", "text")
            | ("date", "timestamp" | "timestamptz")
            | ("timestamp", "timestamptz")
            | ("time", "timetz" | "interval")
    )
}

#[must_use]
pub fn routine_type_category(type_name: &str) -> char {
    canonical_type_category(&canonical_routine_type_name(type_name))
}

fn canonical_type_category(canonical: &str) -> char {
    if canonical.ends_with("[]") {
        return 'A';
    }
    match canonical {
        "bool" => 'B',
        "date" | "time" | "timetz" | "timestamp" | "timestamptz" => 'D',
        "int2" | "int4" | "int8" | "float4" | "float8" | "numeric" | "oid" | "regclass"
        | "regnamespace" | "regproc" | "regrole" | "regtype" => 'N',
        "int2vector" | "oidvector" => 'A',
        "anyarray" | "record" => 'P',
        "bpchar" | "name" | "text" | "varchar" => 'S',
        "interval" => 'T',
        "\"char\"" | "pg_node_tree" => 'Z',
        _ => 'U',
    }
}

#[must_use]
pub fn routine_type_is_preferred(type_name: &str) -> bool {
    canonical_type_is_preferred(&canonical_routine_type_name(type_name))
}

fn canonical_type_is_preferred(type_name: &str) -> bool {
    matches!(
        type_name,
        "bool" | "float8" | "oid" | "text" | "timestamptz" | "interval"
    )
}

mod ranking;
pub use ranking::rank_function_matches;
pub(super) use ranking::rank_function_matches_with_control;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchedBuiltinFunction {
    pub overload: BuiltinFunctionOverload,
    pub argument_types: Vec<String>,
    pub argument_positions: Vec<usize>,
    pub raw_exact_matches: usize,
    pub exact_matches: usize,
    pub preferred_matches: usize,
}

impl RankedFunctionMatch for MatchedBuiltinFunction {
    fn argument_types(&self) -> &[String] {
        &self.argument_types
    }

    fn raw_exact_matches(&self) -> usize {
        self.raw_exact_matches
    }

    fn exact_matches(&self) -> usize {
        self.exact_matches
    }

    fn preferred_matches(&self) -> usize {
        self.preferred_matches
    }
}

mod builtin;
pub use builtin::{
    builtin_binding_matches, builtin_name_matches, match_builtin_function_overload,
    resolve_local_builtin_overload,
};
pub(super) use builtin::{
    resolve_local_builtin_overload_with_control, select_local_builtin_with_control,
};

pub(super) fn bound_function_resolution_error(binding: &FunctionBinding) -> SQLError {
    SQLError::Routine {
        sqlstate: "42883".into(),
        message: format!(
            "bound function {}({}) does not exist",
            binding.name,
            binding.argument_types.join(", ")
        ),
    }
}

pub fn function_resolution_error(
    sqlstate: &str,
    name: &str,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    suffix: &str,
) -> SQLError {
    let arguments = argument_names
        .iter()
        .zip(argument_types)
        .map(|(argument_name, argument_type)| {
            let argument_type = argument_type
                .as_ref()
                .map_or_else(|| "unknown".into(), ColumnType::regtype_name);
            argument_name
                .as_ref()
                .map_or(argument_type.clone(), |name| {
                    format!("{name} => {argument_type}")
                })
        })
        .collect::<Vec<_>>()
        .join(", ");
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message: format!("function {name}({arguments}) {suffix}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Candidate {
        argument_types: Vec<String>,
        raw_exact_matches: usize,
        exact_matches: usize,
        preferred_matches: usize,
    }

    impl RankedFunctionMatch for Candidate {
        fn argument_types(&self) -> &[String] {
            &self.argument_types
        }

        fn raw_exact_matches(&self) -> usize {
            self.raw_exact_matches
        }

        fn exact_matches(&self) -> usize {
            self.exact_matches
        }

        fn preferred_matches(&self) -> usize {
            self.preferred_matches
        }
    }

    fn candidate(
        argument_types: &[&str],
        raw_exact_matches: usize,
        exact_matches: usize,
        preferred_matches: usize,
    ) -> Candidate {
        Candidate {
            argument_types: argument_types
                .iter()
                .map(|argument| (*argument).into())
                .collect(),
            raw_exact_matches,
            exact_matches,
            preferred_matches,
        }
    }

    fn parameter(name: &str, type_name: &str, has_default: bool) -> FunctionParameterDescriptor {
        FunctionParameterDescriptor {
            name: Some(name.into()),
            type_name: type_name.into(),
            has_default,
        }
    }

    #[test]
    fn signature_matcher_maps_named_arguments_and_individual_defaults() {
        let parameters = [
            parameter("first", "integer", false),
            parameter("middle", "text", true),
            parameter("last", "double precision", true),
        ];
        let matched = match_function_signature(
            &parameters,
            &[None, Some("last".into())],
            &[Some(ColumnType::SmallInteger), Some(ColumnType::Integer)],
        )
        .expect("signature should accept the implicit casts and omitted default");
        assert_eq!(matched.argument_types, ["int4", "float8"]);
        assert_eq!(matched.raw_exact_matches, 0);
        assert_eq!(matched.exact_matches, 0);
        assert_eq!(matched.preferred_matches, 1);

        assert!(match_function_signature(
            &parameters,
            &[Some("last".into()), None],
            &[Some(ColumnType::DoublePrecision), Some(ColumnType::Integer)],
        )
        .is_none());
        assert!(match_function_signature(
            &parameters,
            &[None, Some("first".into())],
            &[Some(ColumnType::Integer), Some(ColumnType::Integer)],
        )
        .is_none());
        assert!(match_function_signature(&parameters, &[], &[]).is_none());
    }

    #[test]
    fn signature_matcher_reserves_required_outputs_after_defaulted_inputs() {
        let parameters = [
            parameter("input", "integer", false),
            parameter("optional", "integer", true),
            parameter("output", "integer", false),
        ];
        let matched = match_function_signature(
            &parameters,
            &[None, None],
            &[Some(ColumnType::Integer), None],
        )
        .expect("the second positional argument is the required output placeholder");
        assert_eq!(matched.argument_positions, [0, 2]);
        assert_eq!(matched.argument_types, ["int4", "int4"]);

        let full = match_function_signature(
            &parameters,
            &[None, None, None],
            &[Some(ColumnType::Integer), Some(ColumnType::Integer), None],
        )
        .expect("all declared slots remain callable positionally");
        assert_eq!(full.argument_positions, [0, 1, 2]);

        assert!(
            match_function_signature(&parameters, &[None], &[Some(ColumnType::Integer)],).is_none()
        );
    }

    #[test]
    fn signature_matcher_distinguishes_domain_and_base_exactness() {
        let domain = ColumnType::Domain {
            schema: "public".into(),
            name: "integer_domain".into(),
            oid: 99_999,
            base: Box::new(ColumnType::Integer),
        };
        let domain_match = match_function_signature(
            &[parameter("value", "public.integer_domain", false)],
            &[None],
            &[Some(domain.clone())],
        )
        .expect("domain signature should match exactly");
        assert_eq!(domain_match.raw_exact_matches, 1);
        assert_eq!(domain_match.exact_matches, 1);

        let base_match = match_function_signature(
            &[parameter("value", "int4", false)],
            &[None],
            &[Some(domain)],
        )
        .expect("domain should match its base type");
        assert_eq!(base_match.raw_exact_matches, 0);
        assert_eq!(base_match.exact_matches, 1);
    }

    #[test]
    fn routine_type_aliases_categories_and_implicit_casts_share_one_contract() {
        assert_eq!(canonical_routine_type_name("PG_CATALOG.INTEGER"), "int4");
        assert_eq!(canonical_routine_type_name("varchar(12)[]"), "varchar[]");
        assert_eq!(
            canonical_routine_type_name("timestamp(3) with time zone"),
            "timestamptz"
        );
        assert_eq!(
            canonical_routine_type_name("time(6) without time zone[]"),
            "time[]"
        );
        assert_eq!(routine_type_category("character varying"), 'S');
        assert_eq!(routine_type_category("regclass"), 'N');
        assert_eq!(routine_type_category("regnamespace"), 'N');
        assert_eq!(routine_type_category("int2vector"), 'A');
        assert_eq!(routine_type_category("pg_node_tree"), 'Z');
        assert!(routine_type_is_preferred("double precision"));
        assert!(routine_type_accepts_implicit_cast("int4", "numeric"));
        assert!(routine_type_accepts_implicit_cast("int4", "regclass"));
        assert!(routine_type_accepts_implicit_cast("int4[]", "anyarray"));
        assert!(routine_type_accepts_implicit_cast("regclass", "oid"));
        assert!(routine_type_accepts_implicit_cast("text", "regclass"));
        assert!(!routine_type_accepts_implicit_cast("text", "bytea"));
    }

    #[test]
    fn raw_domain_exact_match_precedes_base_type_ranking() {
        let domain = ColumnType::Domain {
            schema: "public".into(),
            name: "integer_domain".into(),
            oid: 99_999,
            base: Box::new(ColumnType::Integer),
        };
        let mut candidates = vec![
            candidate(&["public.integer_domain"], 1, 1, 0),
            candidate(&["int4"], 0, 1, 0),
        ];
        assert!(rank_function_matches(
            &mut candidates,
            &[Some(domain.clone())]
        ));
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].argument_types, ["public.integer_domain"]);

        let mut candidates = vec![candidate(&["int4"], 0, 1, 0), candidate(&["int8"], 0, 0, 0)];
        assert!(rank_function_matches(&mut candidates, &[Some(domain)]));
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].argument_types, ["int4"]);
    }

    #[test]
    fn conflicting_unknown_category_fails_before_later_positions_rank() {
        let mut candidates = vec![
            candidate(&["int4", "float8"], 0, 0, 0),
            candidate(&["bool", "int4"], 0, 0, 0),
        ];
        assert!(!rank_function_matches(&mut candidates, &[None, None]));
        assert_eq!(candidates.len(), 2);
    }

    #[test]
    fn regclass_participates_in_numeric_unknown_category_ranking() {
        let mut candidates = vec![
            candidate(&["regclass"], 0, 0, 0),
            candidate(&["oid"], 0, 0, 0),
        ];
        assert!(rank_function_matches(&mut candidates, &[None]));
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].argument_types, ["oid"]);
    }
}
