//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native numeric input shared by regobject execution and stored-reference validation.

#[derive(Debug, PartialEq, Eq)]
pub enum NumericRegobjectOid {
    NotNumeric,
    Valid(i64),
    InvalidSyntax,
    OutOfRange,
}

/// `parseDashOrOid` recognizes `-` and digit strings before name lookup. Digit strings retain `oidin`'s radix and uint32 range; signs and whitespace belong to the name path.
pub fn numeric_regobject_oid(input: &str) -> NumericRegobjectOid {
    if input == "-" {
        return NumericRegobjectOid::Valid(0);
    }
    if input.is_empty() || !input.bytes().all(|byte| byte.is_ascii_digit()) {
        return NumericRegobjectOid::NotNumeric;
    }
    let radix = if input.len() > 1 && input.starts_with('0') {
        8
    } else {
        10
    };
    match u32::from_str_radix(input, radix) {
        Ok(oid) => NumericRegobjectOid::Valid(i64::from(oid)),
        Err(error) if matches!(error.kind(), std::num::IntErrorKind::InvalidDigit) => {
            NumericRegobjectOid::InvalidSyntax
        }
        Err(_) => NumericRegobjectOid::OutOfRange,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_regobject_numeric_inputs_preserve_postgresql_radix_and_name_boundaries() {
        // PostgreSQL 18.4 outcomes: backup_sequence_arguments_oracle.expected.txt.
        let observed = allocation_counter::measure(|| {
            for (input, expected) in [
                ("-", 0),
                ("0", 0),
                ("010", 8),
                ("4294967295", 4_294_967_295),
            ] {
                assert_eq!(
                    numeric_regobject_oid(input),
                    NumericRegobjectOid::Valid(expected)
                );
            }
            assert_eq!(
                numeric_regobject_oid("08"),
                NumericRegobjectOid::InvalidSyntax
            );
            assert_eq!(
                numeric_regobject_oid("4294967296"),
                NumericRegobjectOid::OutOfRange
            );
            for input in ["+5", " 5", "", "public.\"123\"", "public.\"-\""] {
                assert_eq!(
                    numeric_regobject_oid(input),
                    NumericRegobjectOid::NotNumeric
                );
            }
        });
        assert_eq!(observed.count_total, 0);
    }
}
