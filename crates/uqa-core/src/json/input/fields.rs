//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Closed field and variant diagnostics retain admitted input names until redaction.

use super::{Cause, JsonDiagnostic, JsonPosition, Produced, Unexpected};

#[derive(Debug)]
pub(super) enum NameKind {
    Field,
    Variant,
}

impl std::fmt::Display for NameKind {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        output.write_str(match self {
            Self::Field => "field",
            Self::Variant => "variant",
        })
    }
}

#[derive(Debug)]
pub(super) enum FieldFailure {
    Missing(&'static str),
    Duplicate(&'static str),
    Unknown {
        kind: NameKind,
        name: Option<Produced<String>>,
        expected: &'static [&'static str],
    },
    Length {
        actual: usize,
        expected: &'static str,
    },
    FieldsLength {
        actual: usize,
        expected: &'static str,
        fields: usize,
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
    /// A string enum tag cannot supply the payload of a non-unit variant.
    pub fn unit_variant(expected: &'static str, position: JsonPosition) -> Self {
        Self {
            position,
            cause: Cause::Type {
                expected,
                actual: Unexpected::Type("unit variant"),
            },
        }
    }

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
        name: impl Into<Produced<String>>,
        expected: &'static [&'static str],
        position: JsonPosition,
    ) -> Self {
        Self::unknown_name(NameKind::Field, name.into(), expected, position)
    }

    pub fn unknown_variant(
        name: impl Into<Produced<String>>,
        expected: &'static [&'static str],
        position: JsonPosition,
    ) -> Self {
        Self::unknown_name(NameKind::Variant, name.into(), expected, position)
    }

    fn unknown_name(
        kind: NameKind,
        name: Produced<String>,
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

    /// Retain a declaration's positional field count without formatting an allocated expected-type string.
    pub fn invalid_field_length(
        actual: usize,
        expected: &'static str,
        fields: usize,
        position: JsonPosition,
    ) -> Self {
        Self {
            position,
            cause: Cause::Field(FieldFailure::FieldsLength {
                actual,
                expected,
                fields,
            }),
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
            Self::FieldsLength {
                actual,
                expected,
                fields,
            } => {
                let plural = if *fields == 1 { "" } else { "s" };
                write!(
                    f,
                    "invalid length {actual}, expected {expected} with {fields} element{plural}"
                )
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
