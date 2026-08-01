use super::*;

#[test]
fn plpgsql_row_count_overflow_is_an_error_not_a_saturated_success() {
    let result = SQLResult::from_affected(u64::MAX);
    assert!(matches!(
        result_row_count(&result),
        Err(SQLError::Internal(message))
            if message.contains("exceeds PL/pgSQL's signed 64-bit ROW_COUNT range")
    ));
}

#[test]
fn plpgsql_row_count_preserves_representable_affected_rows() {
    let result = SQLResult::from_affected(42);
    assert_eq!(result_row_count(&result).unwrap(), 42);
}
