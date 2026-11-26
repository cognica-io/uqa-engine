//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` varlena allocation and `StringInfo` append sizes for SQL text and bytea results.

use super::{Result, SQLError};
use uqa_core::memory::{Produced, ProductionControl, ProductionString};
use uqa_pg_regex::MAX_ALLOC_SIZE;

const VARHDRSZ: usize = 4;
pub(super) const MAX_TEXT_BYTES: usize = MAX_ALLOC_SIZE - VARHDRSZ;

/// Producers such as repeat, pad and translate validate their predicted allocation before building the text.
pub(crate) fn check_text_size(payload: Option<usize>) -> Result<()> {
    if payload.is_none_or(|bytes| bytes > MAX_ALLOC_SIZE - VARHDRSZ) {
        return Err(SQLError::Routine {
            sqlstate: "54000".into(),
            message: "requested length too large".into(),
        });
    }
    Ok(())
}

/// A direct varlena allocation includes its header, including the final result copied from a `StringInfo`.
pub fn check_varlena_size(payload: usize) -> Result<()> {
    check_allocation_size(payload as u128 + VARHDRSZ as u128)
}

/// `PostgreSQL` first allocates the mapped C string, then converts it to text with a varlena header.
pub(super) fn check_case_mapping_size(bytes: u64) -> Result<()> {
    check_allocation_size(u128::from(bytes) + 1)?;
    check_allocation_size(u128::from(bytes) + VARHDRSZ as u128)
}

pub(super) fn check_allocation_size(requested: u128) -> Result<()> {
    if requested > MAX_ALLOC_SIZE as u128 {
        return Err(SQLError::Routine {
            sqlstate: "XX000".into(),
            message: format!("invalid memory alloc request size {requested}"),
        });
    }
    Ok(())
}

/// The encoding functions use the source codec's predicted size before allocating their varlena.
pub(super) fn check_conversion_size(payload: u64, operation: &str) -> Result<()> {
    if payload > (MAX_ALLOC_SIZE - VARHDRSZ) as u64 {
        return Err(SQLError::Routine {
            sqlstate: "54000".into(),
            message: format!("result of {operation} conversion is too large"),
        });
    }
    Ok(())
}

/// `StringInfo` reserves its terminating byte and reports the size of the individual append that crossed the boundary.
pub fn check_buffer_append(current: usize, additional: usize) -> Result<()> {
    if additional >= MAX_ALLOC_SIZE.saturating_sub(current) {
        return Err(SQLError::Diagnostic {
            sqlstate: "54000".into(),
            message: format!("string buffer exceeds maximum allowed length ({MAX_ALLOC_SIZE} bytes)"),
            detail: Some(format!("Cannot enlarge string buffer containing {current} bytes by {additional} more bytes.")),
            hint: None,
        });
    }
    Ok(())
}

pub(super) struct TextBuffer<'a>(ProductionString<'a>);

impl<'a> TextBuffer<'a> {
    pub(super) fn new(control: &ProductionControl<'a>) -> Self {
        Self(ProductionString::new(*control))
    }

    pub(super) fn push_str(&mut self, value: &str) -> Result<()> {
        check_buffer_append(self.0.len(), value.len())?;
        self.0.push_str(value)?;
        Ok(())
    }

    pub(super) fn push(&mut self, character: char) -> Result<()> {
        // format() copies literal UTF-8 bytes with appendStringInfoCharMacro.
        if character.len_utf8() >= MAX_ALLOC_SIZE - self.0.len() {
            check_buffer_append(MAX_ALLOC_SIZE - 1, 1)?;
        }
        self.0.push(character)?;
        Ok(())
    }

    pub(super) fn spaces(&mut self, count: usize) -> Result<()> {
        const SPACES: &str = "                                                                ";
        check_buffer_append(self.0.len(), count)?;
        self.0.reserve(count)?;
        for _ in 0..count / SPACES.len() {
            self.0.push_str(SPACES)?;
        }
        self.0.push_str(&SPACES[..count % SPACES.len()])?;
        Ok(())
    }

    pub(super) fn finish(self) -> Result<Produced<String>> {
        check_varlena_size(self.0.len())?;
        self.0.finish().map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_mapping_reports_the_exact_c_string_or_final_varlena_allocation() {
        let limit = MAX_ALLOC_SIZE as u64;
        assert!(check_case_mapping_size(limit - 4).is_ok());
        for (payload, requested) in [
            (limit - 3, limit + 1),
            (limit - 1, limit + 3),
            (limit, limit + 1),
            (2 * limit, 2 * limit + 1),
        ] {
            let error = check_case_mapping_size(payload).unwrap_err();
            assert_eq!(error.sqlstate(), Some("XX000"));
            assert_eq!(
                error.to_string(),
                format!("invalid memory alloc request size {requested}")
            );
        }
    }

    #[test]
    fn allocation_limits_distinguish_headers_terminators_and_requested_chunks() {
        let limit = MAX_ALLOC_SIZE;
        assert!(check_text_size(Some(limit - VARHDRSZ)).is_ok());
        assert_eq!(
            check_text_size(Some(limit - VARHDRSZ + 1))
                .unwrap_err()
                .sqlstate(),
            Some("54000")
        );
        assert_eq!(check_text_size(None).unwrap_err().sqlstate(), Some("54000"));
        assert!(check_varlena_size(limit - VARHDRSZ).is_ok());
        for payload in limit - 3..=limit {
            let error = check_varlena_size(payload).unwrap_err();
            assert_eq!(error.sqlstate(), Some("XX000"));
            assert_eq!(
                error.to_string(),
                format!("invalid memory alloc request size {}", payload + VARHDRSZ)
            );
        }
        for operation in ["encoding", "decoding"] {
            assert!(check_conversion_size((limit - VARHDRSZ) as u64, operation).is_ok());
            let err = check_conversion_size((limit - VARHDRSZ + 1) as u64, operation).unwrap_err();
            assert_eq!(err.sqlstate(), Some("54000"));
            assert_eq!(
                err.to_string(),
                format!("result of {operation} conversion is too large")
            );
        }
        assert!(check_buffer_append(limit - 7, 6).is_ok());
        let error = check_buffer_append(limit - 7, 7).unwrap_err();
        assert_eq!(error.sqlstate(), Some("54000"));
        assert_eq!(
            error.detail(),
            Some("Cannot enlarge string buffer containing 1073741816 bytes by 7 more bytes.")
        );
        assert!(check_buffer_append(1, usize::MAX).is_err());
    }
}
