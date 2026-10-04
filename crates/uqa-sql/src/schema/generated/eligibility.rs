//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Name-only generation restrictions shared by definition validation and retained value production. Argument-dependent typing and routine binding remain with the existing generated expression validator.

use crate::SQLError;

/// Input is the normalized local built-in name. This is the existing non-fixed generation rule, not a general function-volatility classifier.
pub(crate) fn validate_builtin_name(name: &str) -> Result<(), SQLError> {
    if matches!(
        name,
        "concat"
            | "concat_ws"
            | "format"
            | "array_sample"
            | "now"
            | "current_timestamp"
            | "current_date"
            | "clock_timestamp"
            | "statement_timestamp"
            | "transaction_timestamp"
            | "current_time"
            | "localtime"
            | "localtimestamp"
            | "timeofday"
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
            | "to_number"
    ) {
        return Err(non_immutable_function());
    }
    if matches!(
        name,
        "string_to_table" | "unnest" | "json_object_keys" | "jsonb_object_keys"
    ) {
        return Err(SQLError::TypeMismatch(format!(
            "set-returning function `{name}` is not allowed in a column generation expression"
        )));
    }
    Ok(())
}

pub(crate) fn fixed_builtin_is_non_immutable(name: &str) -> bool {
    matches!(
        name,
        "random"
            | "gen_random_uuid"
            | "uuidv4"
            | "uuidv7"
            | "pg_get_expr"
            | "pg_get_partkeydef"
            | "pg_backend_pid"
            | "pg_my_temp_schema"
            | "pg_is_other_temp_schema"
            | "pg_trigger_depth"
            | "current_setting"
            | "set_config"
            | "pg_sleep"
            | "pg_sleep_for"
            | "pg_sleep_until"
            | "version"
            | "pg_listening_channels"
            | "pg_notify"
            | "pg_notification_queue_usage"
            | "pg_get_serial_sequence"
            | "pg_get_sequence_data"
            | "pg_sequence_last_value"
            | "pg_sequence_parameters"
            | "pg_get_triggerdef"
            | "pg_get_ruledef"
            | "pg_get_viewdef"
            | "pg_get_indexdef"
            | "pg_get_constraintdef"
            | "pg_get_function_arguments"
            | "pg_get_function_identity_arguments"
            | "pg_get_function_result"
            | "pg_get_function_sqlbody"
            | "format_type"
            | "pg_describe_object"
            | "pg_has_role"
            | "pg_get_userbyid"
            | "has_table_privilege"
            | "has_column_privilege"
            | "has_database_privilege"
            | "has_schema_privilege"
            | "has_sequence_privilege"
            | "has_function_privilege"
            | "has_type_privilege"
            | "to_regproc"
            | "to_regprocedure"
            | "to_regclass"
            | "to_regnamespace"
            | "to_regrole"
            | "to_regtype"
    )
}

pub(crate) fn non_immutable_function() -> SQLError {
    SQLError::Routine {
        sqlstate: "42P17".into(),
        message: "generation expression is not immutable".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_name_restrictions_keep_type_dependent_functions_with_the_validator() {
        for name in ["to_char", "format", "jsonb_build_array", "now", "to_json"] {
            let error = validate_builtin_name(name).unwrap_err();
            assert_eq!(error.sqlstate(), Some("42P17"));
            assert_eq!(error.to_string(), "generation expression is not immutable");
        }
        for name in [
            "json_object_keys",
            "jsonb_object_keys",
            "unnest",
            "string_to_table",
        ] {
            assert!(matches!(
                validate_builtin_name(name),
                Err(SQLError::TypeMismatch(_))
            ));
        }
        for name in [
            "extract",
            "date_part",
            "date_trunc",
            "quote_literal",
            "quote_nullable",
            "upper",
            "jsonb_set",
        ] {
            assert!(validate_builtin_name(name).is_ok());
            assert!(!fixed_builtin_is_non_immutable(name));
        }
        for name in ["random", "uuidv7", "to_regclass", "current_setting"] {
            assert!(fixed_builtin_is_non_immutable(name));
        }
    }
}
