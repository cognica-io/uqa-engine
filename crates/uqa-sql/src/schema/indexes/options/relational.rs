//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` relation-option declarations, independent of physical index construction.

use crate::SQLError;

mod values;

pub(super) fn validate(options: &[(String, String)], gin: bool) -> Result<(), SQLError> {
    let mut seen = std::collections::BTreeSet::new();
    for (name, value) in options {
        let kind = match (gin, name.as_str()) {
            (false, "fillfactor") => Kind::Integer(10, 100),
            (false, "deduplicate_items") | (true, "fastupdate") => Kind::Boolean,
            (false, "vacuum_cleanup_index_scale_factor") => Kind::Real,
            (true, "gin_pending_list_limit") => Kind::Integer(64, i32::MAX),
            (true, name) if name.eq_ignore_ascii_case("analyzer") => Kind::Analyzer,
            _ => return Err(invalid(format!("unrecognized parameter \"{name}\""))),
        };
        let identity = if matches!(kind, Kind::Analyzer) {
            "analyzer"
        } else {
            name.as_str()
        };
        if !seen.insert(identity) {
            return Err(invalid(format!(
                "parameter \"{name}\" specified more than once"
            )));
        }
        match kind {
            Kind::Boolean => {
                if !values::boolean(value) {
                    return Err(invalid(format!(
                        "invalid value for boolean option \"{name}\": {value}"
                    )));
                }
            }
            Kind::Integer(min, max) => {
                let parsed = values::integer(value).ok_or_else(|| {
                    invalid(format!(
                        "invalid value for integer option \"{name}\": {value}"
                    ))
                })?;
                if !(min..=max).contains(&parsed) {
                    return Err(bounds(
                        name,
                        value,
                        format!("Valid values are between \"{min}\" and \"{max}\"."),
                    ));
                }
            }
            Kind::Real => {
                let parsed = values::real(value).ok_or_else(|| {
                    invalid(format!(
                        "invalid value for floating point option \"{name}\": {value}"
                    ))
                })?;
                if !(0.0..=1e10).contains(&parsed) {
                    return Err(bounds(
                        name,
                        value,
                        "Valid values are between \"0.000000\" and \"10000000000.000000\".".into(),
                    ));
                }
            }
            Kind::Analyzer => {}
        }
    }
    Ok(())
}

enum Kind {
    Boolean,
    Integer(i32, i32),
    Real,
    Analyzer,
}

fn invalid(message: String) -> SQLError {
    SQLError::Routine {
        sqlstate: "22023".into(),
        message,
    }
}

fn bounds(name: &str, value: &str, detail: String) -> SQLError {
    SQLError::Diagnostic {
        sqlstate: "22023".into(),
        message: format!("value {value} out of bounds for option \"{name}\""),
        detail: Some(detail),
        hint: None,
    }
}

#[cfg(test)]
mod tests;
