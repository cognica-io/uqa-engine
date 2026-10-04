//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{compile_options, CompileOptions, VariableConflict};

#[test]
fn options_before_the_first_block_are_read_between_whitespace_and_comments() {
    assert_eq!(
        compile_options("BEGIN RETURN 1; END"),
        CompileOptions::default()
    );
    assert_eq!(
        compile_options(
            "\n  -- choose the column\n  #VARIABLE_CONFLICT use_column /* nested /* comment */ */\n#print_strict_params on\nDECLARE x int; BEGIN END"
        ),
        CompileOptions {
            variable_conflict: Some(VariableConflict::UseColumn),
            print_strict_params: Some(true),
        }
    );
    assert_eq!(
        compile_options(
            "#option dump\n#variable_conflict use_variable\n#variable_conflict error\nBEGIN END"
        ),
        CompileOptions {
            variable_conflict: Some(VariableConflict::Error),
            print_strict_params: None,
        }
    );
}

#[test]
fn a_hash_after_the_first_block_starts_no_option() {
    assert_eq!(
        compile_options("BEGIN PERFORM 1; END\n#variable_conflict use_column"),
        CompileOptions::default()
    );
}
