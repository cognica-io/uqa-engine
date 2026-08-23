//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Built-in function typing for generated-column validation.

use super::{
    accepts_class, common_numeric_type, common_type, common_types, concat_result_type,
    finalize_common_type, function_type_error, generation_type_name, non_immutable_function,
    numeric_input_type, require_arity, require_class, require_one, require_signature,
    GenerationType, SQLError, TypeClass,
};

#[allow(clippy::too_many_lines)]
pub(super) fn infer_builtin_function(
    name: &str,
    argument_names: &[Option<String>],
    args: &[GenerationType],
) -> Result<Option<GenerationType>, SQLError> {
    let result = match name {
        "coalesce" | "greatest" | "least" => {
            require_arity(name, args, 1, usize::MAX)?;
            common_types(args)?
        }
        "nullif" => {
            require_arity(name, args, 2, 2)?;
            common_type(&args[0], &args[1])?;
            finalize_common_type(args[0].clone())
        }
        "upper" | "lower" | "casefold" | "initcap" => {
            require_signature(name, args, &[TypeClass::Text])?;
            GenerationType::Text
        }
        "length" | "char_length" | "character_length" | "octet_length" => {
            require_signature(name, args, &[TypeClass::Text])?;
            GenerationType::Integer
        }
        "trim" | "btrim" | "ltrim" | "rtrim" => {
            require_arity(name, args, 1, 2)?;
            require_class(name, args, TypeClass::Text)?;
            GenerationType::Text
        }
        "reverse" => {
            require_arity(name, args, 1, 1)?;
            if accepts_class(&args[0], TypeClass::Bytea) {
                GenerationType::Bytea
            } else {
                require_class(name, args, TypeClass::Text)?;
                GenerationType::Text
            }
        }
        "concat" | "concat_ws" | "format" => {
            return Err(non_immutable_function(name));
        }
        "concat_op" => {
            require_arity(name, args, 2, 2)?;
            concat_result_type(&args[0], &args[1])?
        }
        "replace" => {
            require_signature(
                name,
                args,
                &[TypeClass::Text, TypeClass::Text, TypeClass::Text],
            )?;
            GenerationType::Text
        }
        "substring" | "substr" => {
            require_arity(name, args, 2, 3)?;
            require_one(name, &args[0], TypeClass::Text)?;
            require_one(name, &args[1], TypeClass::Integer)?;
            if let Some(length) = args.get(2) {
                require_one(name, length, TypeClass::Integer)?;
            }
            GenerationType::Text
        }
        "left" | "right" => {
            require_signature(name, args, &[TypeClass::Text, TypeClass::Integer])?;
            GenerationType::Text
        }
        "abs" | "ceil" | "ceiling" | "floor" | "sign" => {
            require_signature(name, args, &[TypeClass::Numeric])?;
            numeric_input_type(&args[0])
        }
        "round" | "trunc" => {
            require_arity(name, args, 1, 2)?;
            require_one(name, &args[0], TypeClass::Numeric)?;
            if let Some(scale) = args.get(1) {
                require_one(name, scale, TypeClass::Integer)?;
            }
            numeric_input_type(&args[0])
        }
        "power" | "pow" => {
            require_signature(name, args, &[TypeClass::Numeric, TypeClass::Numeric])?;
            if args.iter().any(|ty| matches!(ty, GenerationType::Numeric))
                && !args.iter().any(|ty| matches!(ty, GenerationType::Real))
            {
                GenerationType::Numeric
            } else {
                GenerationType::Real
            }
        }
        "sqrt" | "sin" | "cos" | "tan" | "asin" | "acos" | "atan" | "sinh" | "cosh" | "tanh"
        | "exp" | "ln" | "log2" | "cbrt" | "gamma" | "lgamma" | "degrees" | "radians" => {
            require_signature(name, args, &[TypeClass::Numeric])?;
            GenerationType::Real
        }
        "atan2" => {
            require_signature(name, args, &[TypeClass::Numeric, TypeClass::Numeric])?;
            GenerationType::Real
        }
        "log" | "log10" => {
            require_arity(name, args, 1, 2)?;
            require_class(name, args, TypeClass::Numeric)?;
            if args.len() == 2 && !args.iter().any(|ty| matches!(ty, GenerationType::Real)) {
                GenerationType::Numeric
            } else {
                GenerationType::Real
            }
        }
        "mod" => {
            require_signature(name, args, &[TypeClass::Numeric, TypeClass::Numeric])?;
            common_numeric_type(args)?
        }
        "div" | "gcd" | "lcm" => {
            require_signature(name, args, &[TypeClass::Integer, TypeClass::Integer])?;
            GenerationType::Integer
        }
        "starts_with" => {
            require_signature(name, args, &[TypeClass::Text, TypeClass::Text])?;
            GenerationType::Boolean
        }
        "like" | "ilike" | "similar_to" => {
            require_arity(name, args, 2, 3)?;
            require_class(name, args, TypeClass::Text)?;
            GenerationType::Boolean
        }
        "position" | "strpos" => {
            require_signature(name, args, &[TypeClass::Text, TypeClass::Text])?;
            GenerationType::Integer
        }
        "ascii" => {
            require_signature(name, args, &[TypeClass::Text])?;
            GenerationType::Integer
        }
        "chr" => {
            require_signature(name, args, &[TypeClass::Integer])?;
            GenerationType::Text
        }
        "regexp_match" | "regexp_matches" => {
            require_arity(name, args, 2, 3)?;
            require_class(name, args, TypeClass::Text)?;
            GenerationType::Array(Box::new(GenerationType::Text))
        }
        "regexp_replace" => {
            require_arity(name, args, 3, 6)?;
            require_one(name, &args[0], TypeClass::Text)?;
            require_one(name, &args[1], TypeClass::Text)?;
            require_one(name, &args[2], TypeClass::Text)?;
            GenerationType::Text
        }
        "pi" => {
            require_arity(name, args, 0, 0)?;
            GenerationType::Real
        }
        "uuid_extract_version" => {
            require_uuid_extraction_signature(name, args)?;
            GenerationType::Integer
        }
        "uuid_extract_timestamp" => {
            require_uuid_extraction_signature(name, args)?;
            GenerationType::TimestampTz
        }
        "random" => {
            require_random_signature(name, argument_names, args)?;
            return Err(non_immutable_function(name));
        }
        "array_sample"
        | "now"
        | "current_timestamp"
        | "current_date"
        | "clock_timestamp"
        | "statement_timestamp"
        | "timeofday"
        | "gen_random_uuid"
        | "uuidv4"
        | "uuidv7"
        | "current_database"
        | "current_catalog"
        | "current_user"
        | "session_user"
        | "pg_typeof"
        | "typeof"
        | "row_to_json"
        | "to_json"
        | "to_jsonb"
        | "json_build_object"
        | "jsonb_build_object"
        | "json_build_array"
        | "jsonb_build_array"
        | "to_char"
        | "to_date"
        | "to_number" => {
            return Err(non_immutable_function(name));
        }
        "crc32" | "crc32c" => {
            require_signature(name, args, &[TypeClass::Bytea])?;
            GenerationType::Integer
        }
        "width_bucket" => {
            require_arity(name, args, 4, 4)?;
            require_class(name, &args[..3], TypeClass::Numeric)?;
            require_one(name, &args[3], TypeClass::Integer)?;
            GenerationType::Integer
        }
        "lpad" | "rpad" => {
            require_arity(name, args, 2, 3)?;
            require_one(name, &args[0], TypeClass::Text)?;
            require_one(name, &args[1], TypeClass::Integer)?;
            if let Some(fill) = args.get(2) {
                require_one(name, fill, TypeClass::Text)?;
            }
            GenerationType::Text
        }
        "repeat" => {
            require_signature(name, args, &[TypeClass::Text, TypeClass::Integer])?;
            GenerationType::Text
        }
        "translate" => {
            require_signature(
                name,
                args,
                &[TypeClass::Text, TypeClass::Text, TypeClass::Text],
            )?;
            GenerationType::Text
        }
        "overlay" => {
            require_arity(name, args, 3, 4)?;
            require_one(name, &args[0], TypeClass::Text)?;
            require_one(name, &args[1], TypeClass::Text)?;
            require_one(name, &args[2], TypeClass::Integer)?;
            if let Some(count) = args.get(3) {
                require_one(name, count, TypeClass::Integer)?;
            }
            GenerationType::Text
        }
        "md5" => {
            require_arity(name, args, 1, 1)?;
            if !accepts_class(&args[0], TypeClass::Text)
                && !accepts_class(&args[0], TypeClass::Bytea)
            {
                return Err(function_type_error(name, &args[0], "text or bytea"));
            }
            GenerationType::Text
        }
        "encode" => {
            require_signature(name, args, &[TypeClass::Bytea, TypeClass::Text])?;
            GenerationType::Text
        }
        "decode" => {
            require_signature(name, args, &[TypeClass::Text, TypeClass::Text])?;
            GenerationType::Bytea
        }
        "split_part" => {
            require_signature(
                name,
                args,
                &[TypeClass::Text, TypeClass::Text, TypeClass::Integer],
            )?;
            GenerationType::Text
        }
        "factorial" => {
            require_signature(name, args, &[TypeClass::Integer])?;
            GenerationType::Numeric
        }
        "bit_length" => {
            require_arity(name, args, 1, 1)?;
            GenerationType::Integer
        }
        "to_bin" | "to_hex" | "to_oct" => {
            return infer_integer_base_conversion(name, args).map(Some);
        }
        "string_to_array" => {
            require_arity(name, args, 2, 3)?;
            require_class(name, args, TypeClass::Text)?;
            GenerationType::Array(Box::new(GenerationType::Text))
        }
        "string_to_table" | "unnest" | "json_object_keys" | "jsonb_object_keys" => {
            return Err(SQLError::TypeMismatch(format!(
                "set-returning function `{name}` is not allowed in a column generation expression"
            )));
        }
        "quote_ident" => {
            require_signature(name, args, &[TypeClass::Text])?;
            GenerationType::Text
        }
        "quote_literal" | "quote_nullable" => {
            require_arity(name, args, 1, 1)?;
            if !accepts_class(&args[0], TypeClass::Text) {
                return Err(non_immutable_function(name));
            }
            GenerationType::Text
        }
        "regexp_count" | "regexp_instr" => {
            require_arity(name, args, 2, 6)?;
            require_one(name, &args[0], TypeClass::Text)?;
            require_one(name, &args[1], TypeClass::Text)?;
            GenerationType::Integer
        }
        "regexp_like" => {
            require_arity(name, args, 2, 3)?;
            require_class(name, args, TypeClass::Text)?;
            GenerationType::Boolean
        }
        "regexp_substr" => {
            require_arity(name, args, 2, 5)?;
            require_one(name, &args[0], TypeClass::Text)?;
            require_one(name, &args[1], TypeClass::Text)?;
            GenerationType::Text
        }
        "num_nulls" | "num_nonnulls" => GenerationType::Integer,
        "array_positions" => {
            require_arity(name, args, 2, 2)?;
            require_one(name, &args[0], TypeClass::Array)?;
            GenerationType::Array(Box::new(GenerationType::Integer))
        }
        "array_replace" => {
            require_arity(name, args, 3, 3)?;
            require_one(name, &args[0], TypeClass::Array)?;
            args[0].clone()
        }
        "array_remove" | "array_append" => {
            require_arity(name, args, 2, 2)?;
            require_one(name, &args[0], TypeClass::Array)?;
            args[0].clone()
        }
        "array_prepend" => {
            require_arity(name, args, 2, 2)?;
            require_one(name, &args[1], TypeClass::Array)?;
            args[1].clone()
        }
        "array_to_string" => {
            require_arity(name, args, 2, 3)?;
            require_one(name, &args[0], TypeClass::Array)?;
            require_one(name, &args[1], TypeClass::Text)?;
            GenerationType::Text
        }
        "array_fill" => {
            require_arity(name, args, 2, 3)?;
            require_one(name, &args[1], TypeClass::Array)?;
            GenerationType::Array(Box::new(args[0].clone()))
        }
        "trim_array" => {
            require_signature(name, args, &[TypeClass::Array, TypeClass::Integer])?;
            args[0].clone()
        }
        "array_overlap" | "__any_op" | "__all_op" | "__is_distinct" | "__between_symmetric" => {
            GenerationType::Boolean
        }
        "__subscript" | "__array_subscripts" => {
            require_arity(name, args, 2, usize::MAX)?;
            require_one(name, &args[0], TypeClass::Array)?;
            for argument in &args[1..] {
                require_one(name, argument, TypeClass::Integer)?;
            }
            match &args[0] {
                GenerationType::Array(element) => (**element).clone(),
                GenerationType::Vector | GenerationType::Tensor => GenerationType::Real,
                GenerationType::Null | GenerationType::UnknownLiteral(_) => GenerationType::Null,
                _ => unreachable!(),
            }
        }
        "__slice" | "__array_slices" => {
            require_arity(name, args, 3, usize::MAX)?;
            require_one(name, &args[0], TypeClass::Array)?;
            for argument in &args[1..] {
                require_one(name, argument, TypeClass::Integer)?;
            }
            args[0].clone()
        }
        "array_length" | "array_upper" | "array_lower" => {
            require_signature(name, args, &[TypeClass::Array, TypeClass::Integer])?;
            GenerationType::Integer
        }
        "cardinality" => {
            require_arity(name, args, 1, 1)?;
            require_one(name, &args[0], TypeClass::Array)?;
            GenerationType::Integer
        }
        "array_ndims" => {
            require_arity(name, args, 1, 1)?;
            require_one(name, &args[0], TypeClass::Array)?;
            GenerationType::Integer
        }
        "array_dims" => {
            require_arity(name, args, 1, 1)?;
            require_one(name, &args[0], TypeClass::Array)?;
            GenerationType::Text
        }
        "array_position" => {
            require_arity(name, args, 2, 3)?;
            require_one(name, &args[0], TypeClass::Array)?;
            GenerationType::Integer
        }
        "array_cat" => {
            require_signature(name, args, &[TypeClass::Array, TypeClass::Array])?;
            common_type(&args[0], &args[1])?
        }
        "array_reverse" => {
            require_arity(name, args, 1, 1)?;
            require_one(name, &args[0], TypeClass::Array)?;
            args[0].clone()
        }
        "array_sort" => {
            require_arity(name, args, 1, 3)?;
            require_one(name, &args[0], TypeClass::Array)?;
            for flag in &args[1..] {
                require_one(name, flag, TypeClass::Boolean)?;
            }
            args[0].clone()
        }
        "json_typeof" | "jsonb_typeof" | "jsonb_pretty" => {
            require_arity(name, args, 1, 1)?;
            GenerationType::Text
        }
        "json_array_length" | "jsonb_array_length" => {
            require_arity(name, args, 1, 1)?;
            GenerationType::Integer
        }
        "json_extract_path" => {
            require_arity(name, args, 2, usize::MAX)?;
            require_one(name, &args[0], TypeClass::Json)?;
            GenerationType::Json
        }
        "jsonb_extract_path" => {
            require_arity(name, args, 2, usize::MAX)?;
            require_one(name, &args[0], TypeClass::JsonB)?;
            GenerationType::JsonB
        }
        "json_extract_path_text" | "jsonb_extract_path_text" => {
            require_arity(name, args, 2, usize::MAX)?;
            GenerationType::Text
        }
        "contains_op" | "contained_by_op" => {
            require_containment_operands(name, args)?;
            GenerationType::Boolean
        }
        "json_contains" | "json_contained_by" | "json_has_key" | "json_has_any_key"
        | "json_has_all_keys" | "jsonb_path_exists" | "jsonpath_exists" | "jsonb_path_match"
        | "jsonpath_match" => GenerationType::Boolean,
        "json_delete_path" => GenerationType::JsonB,
        "jsonb_set" | "jsonb_insert" => {
            require_arity(name, args, 3, 4)?;
            require_one(name, &args[0], TypeClass::JsonB)?;
            GenerationType::JsonB
        }
        "json_strip_nulls" => {
            require_arity(name, args, 1, 2)?;
            require_one(name, &args[0], TypeClass::Json)?;
            GenerationType::Json
        }
        "jsonb_strip_nulls" => {
            require_arity(name, args, 1, 2)?;
            require_one(name, &args[0], TypeClass::JsonB)?;
            GenerationType::JsonB
        }
        "to_timestamp" => {
            require_signature(name, args, &[TypeClass::Numeric])?;
            GenerationType::TimestampTz
        }
        "extract" | "date_part" => {
            require_signature(name, args, &[TypeClass::Text, TypeClass::Temporal])?;
            if matches!(args[1], GenerationType::TimestampTz) {
                return Err(non_immutable_function(name));
            }
            if name == "extract" {
                GenerationType::Numeric
            } else {
                GenerationType::Real
            }
        }
        "age" => {
            require_arity(name, args, 2, 2)?;
            require_class(name, args, TypeClass::Temporal)?;
            GenerationType::Interval
        }
        "date_trunc" => {
            require_signature(name, args, &[TypeClass::Text, TypeClass::Temporal])?;
            if matches!(args[1], GenerationType::TimestampTz) {
                return Err(non_immutable_function(name));
            }
            args[1].clone()
        }
        "make_timestamp" => {
            require_arity(name, args, 6, 6)?;
            require_class(name, args, TypeClass::Numeric)?;
            GenerationType::Timestamp
        }
        "make_date" => {
            require_signature(
                name,
                args,
                &[TypeClass::Integer, TypeClass::Integer, TypeClass::Integer],
            )?;
            GenerationType::Date
        }
        "make_interval" => {
            require_arity(name, args, 0, 7)?;
            require_class(name, args, TypeClass::Numeric)?;
            GenerationType::Interval
        }
        "justify_hours" => {
            require_arity(name, args, 1, 1)?;
            if !matches!(args[0], GenerationType::Interval) {
                return Err(function_type_error(name, &args[0], "interval"));
            }
            GenerationType::Interval
        }
        "isfinite" => {
            require_signature(name, args, &[TypeClass::Temporal])?;
            GenerationType::Boolean
        }
        "point" => {
            require_signature(name, args, &[TypeClass::Numeric, TypeClass::Numeric])?;
            GenerationType::Array(Box::new(GenerationType::Real))
        }
        "st_distance" => {
            require_signature(name, args, &[TypeClass::Array, TypeClass::Array])?;
            GenerationType::Real
        }
        "st_within" | "st_dwithin" | "overlaps" => GenerationType::Boolean,
        _ => return Ok(None),
    };
    Ok(Some(result))
}

fn infer_integer_base_conversion(
    name: &str,
    args: &[GenerationType],
) -> Result<GenerationType, SQLError> {
    if let [GenerationType::Integer | GenerationType::BigInteger] = args {
        return Ok(GenerationType::Text);
    }
    let signature = args
        .iter()
        .map(generation_type_name)
        .collect::<Vec<_>>()
        .join(", ");
    let ambiguous = matches!(
        args,
        [GenerationType::Null | GenerationType::UnknownLiteral(_) | GenerationType::SmallInteger]
    );
    Err(SQLError::Routine {
        sqlstate: if ambiguous { "42725" } else { "42883" }.into(),
        message: if ambiguous {
            format!("function {name}({signature}) is not unique")
        } else {
            format!("function {name}({signature}) does not exist")
        },
    })
}

fn require_random_signature(
    name: &str,
    argument_names: &[Option<String>],
    args: &[GenerationType],
) -> Result<(), SQLError> {
    if args.is_empty() && argument_names.is_empty() {
        return Ok(());
    }
    let valid_names = if args.len() == 2 && argument_names.len() == 2 {
        let mut positions = [false; 2];
        let mut positional = 0;
        argument_names.iter().all(|argument_name| {
            let position = match argument_name.as_deref() {
                Some("min") => 0,
                Some("max") => 1,
                Some(_) => return false,
                None => {
                    let position = positional;
                    positional += 1;
                    position
                }
            };
            positions.get_mut(position).is_some_and(|occupied| {
                let available = !*occupied;
                *occupied = true;
                available
            })
        }) && positions.into_iter().all(|occupied| occupied)
    } else {
        false
    };
    let strongest = args
        .iter()
        .try_fold(None, |strongest, argument| {
            let rank = match argument {
                GenerationType::Null | GenerationType::UnknownLiteral(_) => return Some(strongest),
                GenerationType::SmallInteger => 0,
                GenerationType::Integer => 1,
                GenerationType::BigInteger => 2,
                GenerationType::Numeric => 3,
                _ => return None,
            };
            Some(Some(
                strongest.map_or(rank, |current: u8| current.max(rank)),
            ))
        })
        .flatten();
    if valid_names && matches!(strongest, Some(1..=3)) {
        return Ok(());
    }
    let signature = args
        .iter()
        .zip(argument_names)
        .map(|(argument, argument_name)| {
            let argument = generation_type_name(argument);
            argument_name
                .as_ref()
                .map_or(argument.clone(), |name| format!("{name} => {argument}"))
        })
        .collect::<Vec<_>>()
        .join(", ");
    let ambiguous = valid_names
        && args.iter().all(|argument| {
            matches!(
                argument,
                GenerationType::Null
                    | GenerationType::UnknownLiteral(_)
                    | GenerationType::SmallInteger
            )
        });
    Err(SQLError::Routine {
        sqlstate: if ambiguous { "42725" } else { "42883" }.into(),
        message: if ambiguous {
            format!("function {name}({signature}) is not unique")
        } else {
            format!("function {name}({signature}) does not exist")
        },
    })
}

fn require_uuid_extraction_signature(name: &str, args: &[GenerationType]) -> Result<(), SQLError> {
    if matches!(args, [argument] if accepts_class(argument, TypeClass::Uuid)) {
        return Ok(());
    }
    let signature = args
        .iter()
        .map(generation_type_name)
        .collect::<Vec<_>>()
        .join(", ");
    Err(SQLError::Routine {
        sqlstate: "42883".into(),
        message: format!("function {name}({signature}) does not exist"),
    })
}

fn require_containment_operands(name: &str, args: &[GenerationType]) -> Result<(), SQLError> {
    require_arity(name, args, 2, 2)?;
    let unknown = |ty: &GenerationType| {
        matches!(ty, GenerationType::Null | GenerationType::UnknownLiteral(_))
    };
    let supported =
        |ty: &GenerationType| matches!(ty, GenerationType::Array(_) | GenerationType::JsonB);
    let compatible = match (&args[0], &args[1]) {
        (GenerationType::JsonB, GenerationType::JsonB) => true,
        (GenerationType::Array(left), GenerationType::Array(right)) => left == right,
        (left, right) if unknown(left) && supported(right) => true,
        (left, right) if supported(left) && unknown(right) => true,
        _ => false,
    };
    if compatible {
        Ok(())
    } else {
        let symbol = if name == "contains_op" { "@>" } else { "<@" };
        Err(SQLError::TypeMismatch(format!(
            "operator does not exist: {} {symbol} {}",
            generation_type_name(&args[0]),
            generation_type_name(&args[1])
        )))
    }
}
