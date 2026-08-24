//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared `PostgreSQL` function-candidate matching and ranking rules.

use uqa_sql::ast::{ColumnType, FunctionBinding};
use uqa_sql::SQLError;

use super::common::base_type;
use super::{BuiltinFunctionOverload, ResolvedFunctionOverload};

/// Candidate information used by `PostgreSQL`'s exact-match, preferred-type, and unknown-category ranking passes.
pub trait RankedFunctionMatch {
    fn argument_types(&self) -> &[String];
    fn raw_exact_matches(&self) -> usize;
    fn exact_matches(&self) -> usize;
    fn preferred_matches(&self) -> usize;
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

/// Match a call against one declared signature, including named arguments, omitted defaults, domain exactness, implicit casts, and preferred types.
#[must_use]
pub fn match_function_signature(
    parameters: &[FunctionParameterDescriptor],
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
) -> Option<MatchedFunctionSignature> {
    if argument_types.len() > parameters.len() || argument_names.len() != argument_types.len() {
        return None;
    }
    let mut slots = vec![None; parameters.len()];
    let mut argument_positions = vec![usize::MAX; argument_types.len()];
    let mut reserved = vec![false; parameters.len()];
    let mut saw_named = false;
    for (argument_index, argument_name) in argument_names.iter().enumerate() {
        if let Some(argument_name) = argument_name {
            saw_named = true;
            let parameter_index = parameters
                .iter()
                .position(|parameter| parameter.name.as_deref() == Some(argument_name.as_str()))?;
            if reserved[parameter_index] {
                return None;
            }
            reserved[parameter_index] = true;
            argument_positions[argument_index] = parameter_index;
        } else if saw_named {
            return None;
        }
    }

    let positional_count = argument_names
        .iter()
        .take_while(|name| name.is_none())
        .count();
    let mut parameter_index = 0usize;
    for (argument_index, argument_position) in argument_positions
        .iter_mut()
        .take(positional_count)
        .enumerate()
    {
        let remaining_arguments = positional_count - argument_index;
        loop {
            if parameter_index >= parameters.len() || reserved[parameter_index] {
                return None;
            }
            let required_remaining = parameters[parameter_index..]
                .iter()
                .enumerate()
                .filter(|(offset, parameter)| {
                    !reserved[parameter_index + offset] && !parameter.has_default
                })
                .count();
            // PostgreSQL reserves required OUT/TABLE slots after defaulted inputs, so a positional placeholder binds the output slot when exactly the required arguments remain.
            if parameters[parameter_index].has_default && remaining_arguments == required_remaining
            {
                parameter_index += 1;
                continue;
            }
            *argument_position = parameter_index;
            parameter_index += 1;
            break;
        }
    }

    for (argument_index, argument_type) in argument_types.iter().enumerate() {
        let index = argument_positions[argument_index];
        if index == usize::MAX {
            return None;
        }
        if slots[index].replace(argument_type.as_ref()).is_some() {
            return None;
        }
    }
    let matched_argument_types = argument_positions
        .iter()
        .map(|index| canonical_routine_type_name(&parameters[*index].type_name))
        .collect::<Vec<_>>();

    let mut raw_exact_matches = 0usize;
    let mut exact_matches = 0usize;
    let mut preferred_matches = 0usize;
    for (slot, parameter) in slots.into_iter().zip(parameters) {
        let Some(actual) = slot else {
            if !parameter.has_default {
                return None;
            }
            continue;
        };
        let Some(actual_type) = actual else {
            continue;
        };
        let declared = canonical_routine_type_name(&parameter.type_name);
        let raw_actual = canonical_column_type_name(actual_type);
        let actual = canonical_column_type_name(base_type(actual_type));
        if raw_actual == declared {
            raw_exact_matches += 1;
            exact_matches += 1;
        } else if actual == declared {
            exact_matches += 1;
        } else if routine_type_accepts_implicit_cast(&actual, &declared) {
            preferred_matches += usize::from(routine_type_is_preferred(&declared));
        } else {
            return None;
        }
    }
    Some(MatchedFunctionSignature {
        argument_types: matched_argument_types,
        argument_positions,
        raw_exact_matches,
        exact_matches,
        preferred_matches,
    })
}

/// Canonical type spelling used by routine identity and overload resolution.
#[must_use]
pub fn canonical_routine_type_name(type_name: &str) -> String {
    let compact = type_name
        .trim()
        .to_ascii_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if let Some(element) = compact.strip_suffix("[]") {
        return format!("{}[]", canonical_routine_type_name(element));
    }
    let without_catalog = compact.strip_prefix("pg_catalog.").unwrap_or(&compact);
    let base = strip_type_modifiers(without_catalog);
    match base.as_str() {
        "smallint" | "int2" => "int2",
        "integer" | "int" | "int4" | "serial" | "serial4" => "int4",
        "bigint" | "int8" | "bigserial" | "serial8" => "int8",
        "real" | "float4" => "float4",
        "double" | "double precision" | "float8" => "float8",
        "decimal" | "numeric" => "numeric",
        "character varying" | "varchar" => "varchar",
        "character" | "char" | "bpchar" => "bpchar",
        "bool" | "boolean" => "bool",
        "timestamp without time zone" | "timestamp" => "timestamp",
        "timestamp with time zone" | "timestamptz" => "timestamptz",
        "time without time zone" | "time" => "time",
        "time with time zone" | "timetz" => "timetz",
        other => other,
    }
    .to_string()
}

fn strip_type_modifiers(type_name: &str) -> String {
    let mut stripped = String::with_capacity(type_name.len());
    let mut modifier_depth = 0usize;
    let mut quoted = false;
    let mut characters = type_name.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '"' && modifier_depth == 0 {
            stripped.push(character);
            if quoted && characters.peek() == Some(&'"') {
                stripped.push(characters.next().expect("peeked quoted identifier escape"));
            } else {
                quoted = !quoted;
            }
            continue;
        }
        if !quoted {
            if character == '(' {
                modifier_depth += 1;
                continue;
            }
            if character == ')' && modifier_depth > 0 {
                modifier_depth -= 1;
                continue;
            }
        }
        if modifier_depth == 0 {
            stripped.push(character);
        }
    }
    stripped.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[must_use]
pub fn canonical_column_type_name(ty: &ColumnType) -> String {
    canonical_routine_type_name(&ty.sql_name())
}

#[must_use]
pub fn routine_type_accepts_implicit_cast(actual: &str, declared: &str) -> bool {
    if actual == declared {
        return true;
    }
    if declared == "anyarray" && actual.ends_with("[]") {
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
                | "regtype",
        ) | ("oid", "regclass" | "regnamespace" | "regproc" | "regtype")
            | ("regclass" | "regnamespace" | "regproc" | "regtype", "oid")
            | ("numeric", "float4" | "float8")
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
    let canonical = canonical_routine_type_name(type_name);
    if canonical.ends_with("[]") {
        return 'A';
    }
    match canonical.as_str() {
        "bool" => 'B',
        "date" | "time" | "timetz" | "timestamp" | "timestamptz" => 'D',
        "int2" | "int4" | "int8" | "float4" | "float8" | "numeric" | "oid" | "regclass"
        | "regnamespace" | "regproc" | "regtype" => 'N',
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
    matches!(
        canonical_routine_type_name(type_name).as_str(),
        "bool" | "float8" | "oid" | "text" | "timestamptz" | "interval"
    )
}

/// Apply `PostgreSQL`'s candidate-ranking passes to candidates that already accept the call. Returns `false` when conflicting unknown categories make the call ambiguous before any later unknown position may narrow it.
#[must_use]
pub fn rank_function_matches<T: RankedFunctionMatch>(
    candidates: &mut Vec<T>,
    argument_types: &[Option<ColumnType>],
) -> bool {
    if argument_types.iter().all(Option::is_some) {
        let raw_exact = candidates
            .iter()
            .filter(|candidate| candidate.raw_exact_matches() == argument_types.len())
            .count();
        if raw_exact > 0 {
            candidates.retain(|candidate| candidate.raw_exact_matches() == argument_types.len());
            return true;
        }
    }
    let most_exact = candidates
        .iter()
        .map(RankedFunctionMatch::exact_matches)
        .max()
        .unwrap_or(0);
    candidates.retain(|candidate| candidate.exact_matches() == most_exact);
    let most_preferred = candidates
        .iter()
        .map(RankedFunctionMatch::preferred_matches)
        .max()
        .unwrap_or(0);
    candidates.retain(|candidate| candidate.preferred_matches() == most_preferred);

    for (index, actual) in argument_types.iter().enumerate() {
        if actual.is_some() || candidates.len() <= 1 {
            continue;
        }
        let mut categories = candidates
            .iter()
            .map(|candidate| routine_type_category(&candidate.argument_types()[index]))
            .collect::<Vec<_>>();
        categories.sort_unstable();
        categories.dedup();
        let selected = if categories.contains(&'S') {
            'S'
        } else if categories.len() == 1 {
            categories[0]
        } else {
            return false;
        };
        candidates.retain(|candidate| {
            routine_type_category(&candidate.argument_types()[index]) == selected
        });
        if candidates
            .iter()
            .any(|candidate| routine_type_is_preferred(&candidate.argument_types()[index]))
        {
            candidates
                .retain(|candidate| routine_type_is_preferred(&candidate.argument_types()[index]));
        }
    }

    if candidates.len() <= 1 {
        return true;
    }
    let mut known = argument_types.iter().flatten();
    let Some(first) = known.next() else {
        return true;
    };
    let identity = canonical_column_type_name(base_type(first));
    if !known.all(|ty| canonical_column_type_name(base_type(ty)) == identity) {
        return true;
    }
    candidates.retain(|candidate| {
        argument_types.iter().enumerate().all(|(index, actual)| {
            actual.is_some()
                || routine_type_accepts_implicit_cast(&identity, &candidate.argument_types()[index])
        })
    });
    true
}

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

#[must_use]
pub fn builtin_name_matches(name: &str, builtin_name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let builtin_name = builtin_name.to_ascii_lowercase();
    if name.contains('.') {
        name == builtin_name
    } else {
        builtin_name.rsplit('.').next() == Some(name.as_str())
    }
}

#[must_use]
pub fn builtin_binding_matches(
    builtin: &BuiltinFunctionOverload,
    binding: &FunctionBinding,
) -> bool {
    builtin.name.eq_ignore_ascii_case(&binding.name)
        && builtin
            .argument_types
            .iter()
            .map(|ty| canonical_routine_type_name(&ty.sql_name()))
            .eq(binding
                .argument_types
                .iter()
                .map(|ty| canonical_routine_type_name(ty)))
}

#[must_use]
pub fn match_builtin_function_overload(
    builtin: BuiltinFunctionOverload,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
) -> Option<MatchedBuiltinFunction> {
    let required_arguments = builtin
        .argument_types
        .len()
        .checked_sub(builtin.default_arguments)?;
    if builtin.argument_names.len() != builtin.argument_types.len() {
        return None;
    }
    let parameters = builtin
        .argument_names
        .iter()
        .cloned()
        .zip(&builtin.argument_types)
        .enumerate()
        .map(|(index, (name, ty))| FunctionParameterDescriptor {
            name,
            type_name: canonical_routine_type_name(&ty.sql_name()),
            has_default: index >= required_arguments,
        })
        .collect::<Vec<_>>();
    let matched = match_function_signature(&parameters, argument_names, argument_types)?;
    Some(MatchedBuiltinFunction {
        overload: builtin,
        argument_types: matched.argument_types,
        argument_positions: matched.argument_positions,
        raw_exact_matches: matched.raw_exact_matches,
        exact_matches: matched.exact_matches,
        preferred_matches: matched.preferred_matches,
    })
}

pub fn resolve_local_builtin_overload(
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    builtins: &[BuiltinFunctionOverload],
) -> Result<ResolvedFunctionOverload, SQLError> {
    if let Some(binding) = binding {
        if !binding.builtin {
            return Err(bound_function_resolution_error(binding));
        }
        let builtin = builtins
            .iter()
            .find(|builtin| builtin_binding_matches(builtin, binding))
            .cloned()
            .ok_or_else(|| bound_function_resolution_error(binding))?;
        let matched = match_builtin_function_overload(builtin, argument_names, argument_types)
            .ok_or_else(|| bound_function_resolution_error(binding))?;
        return Ok(resolved_builtin_overload(matched, argument_types));
    }
    let mut candidates = builtins
        .iter()
        .filter(|builtin| builtin_name_matches(name, &builtin.name))
        .cloned()
        .filter_map(|builtin| {
            match_builtin_function_overload(builtin, argument_names, argument_types)
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Err(function_resolution_error(
            "42883",
            name,
            argument_names,
            argument_types,
            "does not exist",
        ));
    }
    if !rank_function_matches(&mut candidates, argument_types) || candidates.len() != 1 {
        return Err(function_resolution_error(
            "42725",
            name,
            argument_names,
            argument_types,
            "is not unique",
        ));
    }
    let matched = candidates
        .pop()
        .ok_or_else(|| SQLError::Internal("resolved built-in candidate disappeared".into()))?;
    Ok(resolved_builtin_overload(matched, argument_types))
}

fn resolved_builtin_overload(
    matched: MatchedBuiltinFunction,
    argument_types: &[Option<ColumnType>],
) -> ResolvedFunctionOverload {
    ResolvedFunctionOverload {
        binding: FunctionBinding {
            name: matched.overload.name,
            argument_types: matched
                .overload
                .argument_types
                .iter()
                .map(ColumnType::sql_name)
                .collect(),
            builtin: true,
        },
        return_type: matched.overload.return_type,
        exact_matches: matched.exact_matches,
        known_arguments: argument_types.iter().flatten().count(),
        preferred_matches: matched.preferred_matches,
        precedes_pg_catalog: false,
    }
}

fn bound_function_resolution_error(binding: &FunctionBinding) -> SQLError {
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
