//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Closed field and variant diagnostics retain admitted input names until redaction.

use super::{Budgeted, Cause, JsonDiagnostic, JsonPosition};

#[derive(Debug)]
pub(super) enum FieldFailure {
    Missing(&'static str),
    Duplicate(&'static str),
    Unknown {
        kind: &'static str,
        name: Option<Budgeted<String>>,
        expected: &'static [&'static str],
    },
    Length {
        actual: usize,
        expected: &'static str,
    },
}

impl FieldFailure {
    pub(super) fn redact(&mut self) {
        if let Self::Unknown { name, .. } = self {
            *name = None;
        }
    }
}

impl JsonDiagnostic {
    pub fn missing_field(name: &'static str, position: JsonPosition) -> Self {
        Self {
            position,
            cause: Cause::Field(FieldFailure::Missing(name)),
        }
    }

    pub fn duplicate_field(name: &'static str, position: JsonPosition) -> Self {
        Self {
            position,
            cause: Cause::Field(FieldFailure::Duplicate(name)),
        }
    }

    pub fn unknown_field(
        name: Budgeted<String>,
        expected: &'static [&'static str],
        position: JsonPosition,
    ) -> Self {
        Self::unknown_name("field", name, expected, position)
    }

    pub fn unknown_variant(
        name: Budgeted<String>,
        expected: &'static [&'static str],
        position: JsonPosition,
    ) -> Self {
        Self::unknown_name("variant", name, expected, position)
    }

    fn unknown_name(
        kind: &'static str,
        name: Budgeted<String>,
        expected: &'static [&'static str],
        position: JsonPosition,
    ) -> Self {
        Self {
            position,
            cause: Cause::Field(FieldFailure::Unknown {
                kind,
                name: Some(name),
                expected,
            }),
        }
    }

    pub fn invalid_length(actual: usize, expected: &'static str, position: JsonPosition) -> Self {
        Self {
            position,
            cause: Cause::Field(FieldFailure::Length { actual, expected }),
        }
    }
}

impl std::fmt::Display for FieldFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing(name) => write!(f, "missing field `{name}`"),
            Self::Duplicate(name) => write!(f, "duplicate field `{name}`"),
            Self::Length { actual, expected } => {
                write!(f, "invalid length {actual}, expected {expected}")
            }
            Self::Unknown {
                kind,
                name,
                expected,
            } => {
                write!(f, "unknown {kind}")?;
                if let Some(name) = name {
                    write!(f, " `{}`", name.as_str())?;
                }
                match *expected {
                    [] => write!(f, ", there are no {kind}s"),
                    [one] => write!(f, ", expected `{one}`"),
                    [one, two] => write!(f, ", expected `{one}` or `{two}`"),
                    names => {
                        f.write_str(", expected one of ")?;
                        for (index, name) in names.iter().enumerate() {
                            if index != 0 {
                                f.write_str(", ")?;
                            }
                            write!(f, "`{name}`")?;
                        }
                        Ok(())
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
