//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A single borrowed fixed-signature registry serves binding validation and ordinary candidate construction.

use crate::ast::ColumnType;

pub(super) struct Signature {
    pub(super) argument_types: &'static [ColumnType],
    pub(super) argument_names: &'static [&'static str],
    pub(super) default_arguments: usize,
    pub(super) return_type: ColumnType,
}

impl Signature {
    pub(super) const fn new(
        argument_types: &'static [ColumnType],
        return_type: ColumnType,
    ) -> Self {
        Self::defaulted(argument_types, &[], 0, return_type)
    }

    pub(super) const fn named(
        argument_types: &'static [ColumnType],
        argument_names: &'static [&'static str],
        return_type: ColumnType,
    ) -> Self {
        Self::defaulted(argument_types, argument_names, 0, return_type)
    }

    pub(super) const fn defaulted(
        argument_types: &'static [ColumnType],
        argument_names: &'static [&'static str],
        default_arguments: usize,
        return_type: ColumnType,
    ) -> Self {
        Self {
            argument_types,
            argument_names,
            default_arguments,
            return_type,
        }
    }
}

macro_rules! declarations {
    ($visibility:vis fn $function:ident($local:ident); $($($name:literal)|+ => $signatures:expr),* $(,)?) => {
        $visibility fn $function($local: &str) -> Option<(&'static str, &'static [Signature])> {
            $(
                if let Some(name) = [$($name),+].into_iter().find(|name| $local.eq_ignore_ascii_case(name)) {
                    const SIGNATURES: &[Signature] = $signatures;
                    return Some((name, SIGNATURES));
                }
            )*
            None
        }
    };
}
pub(super) use declarations;

pub(super) fn lookup(name: &str) -> Option<(&'static str, &'static [Signature])> {
    let local = match name.split_once('.') {
        Some((namespace, local)) if namespace.eq_ignore_ascii_case("pg_catalog") => local,
        Some(_) => return None,
        None => name,
    };
    if let Some(signatures) = super::standard::lookup(local) {
        return Some(signatures);
    }
    lookup_local(local)
}

declarations! { fn lookup_local(local);

        "abs" => &[
            Signature::new(&[ColumnType::SmallInteger], ColumnType::SmallInteger),
            Signature::new(&[ColumnType::Integer], ColumnType::Integer),
            Signature::new(&[ColumnType::BigInteger], ColumnType::BigInteger),
            Signature::new(&[ColumnType::Real], ColumnType::Real),
            Signature::new(&[ColumnType::DoublePrecision], ColumnType::DoublePrecision),
            Signature::new(&NUMERIC_UNARY_ARGUMENTS, numeric_type()),
        ],
        "reverse" => &[
            Signature::new(&[ColumnType::Text], ColumnType::Text),
            Signature::new(&[ColumnType::Bytea], ColumnType::Bytea),
        ],
        "md5" => &[
            Signature::new(&[ColumnType::Text], ColumnType::Text),
            Signature::new(&[ColumnType::Bytea], ColumnType::Text),
        ],
        "crc32" | "crc32c" => {
            &[Signature::new(&[ColumnType::Bytea],
                ColumnType::BigInteger,
            )]
        },
        "length" | "octet_length" => &[
            Signature::new(&[ColumnType::Text], ColumnType::Integer),
            Signature::new(&[ColumnType::Bpchar], ColumnType::Integer),
            Signature::new(&[ColumnType::Bytea], ColumnType::Integer),
        ],
        "char_length" | "character_length" => &[
            Signature::new(&[ColumnType::Text], ColumnType::Integer),
            Signature::new(&[ColumnType::Bpchar], ColumnType::Integer),
        ],
        "bit_length" => &[
            Signature::new(&[ColumnType::Text], ColumnType::Integer),
            Signature::new(&[ColumnType::Bytea], ColumnType::Integer),
        ],
        "gamma" | "lgamma" => &[Signature::new(&[ColumnType::DoublePrecision],
            ColumnType::DoublePrecision,
        )],
        "json_strip_nulls" => &[Signature::defaulted(&[ColumnType::Json, ColumnType::Boolean],
            &["target", "strip_in_arrays"],
            1,
            ColumnType::Json,
        )],
        "jsonb_strip_nulls" => &[Signature::defaulted(&[ColumnType::JsonB, ColumnType::Boolean],
            &["target", "strip_in_arrays"],
            1,
            ColumnType::JsonB,
        )],
        "to_bin" | "to_hex" | "to_oct" => &[
            Signature::new(&[ColumnType::Integer], ColumnType::Text),
            Signature::new(&[ColumnType::BigInteger], ColumnType::Text),
        ],
        "random" => &[
            Signature::new(&[], ColumnType::DoublePrecision),
            Signature::named(&[ColumnType::Integer, ColumnType::Integer],
                &["min", "max"],
                ColumnType::Integer,
            ),
            Signature::named(&[ColumnType::BigInteger, ColumnType::BigInteger],
                &["min", "max"],
                ColumnType::BigInteger,
            ),
            Signature::named(&NUMERIC_BINARY_ARGUMENTS,
                &["min", "max"],
                numeric_type(),
            ),
        ],
        "uuid_extract_timestamp" => &[Signature::new(&[ColumnType::Uuid],
            ColumnType::TimestampTz,
        )],
        "uuid_extract_version" => &[Signature::new(&[ColumnType::Uuid],
            ColumnType::SmallInteger,
        )],
        "gen_random_uuid" | "uuidv4" => &[Signature::new(&[], ColumnType::Uuid)],
        "uuidv7" => &[
            Signature::new(&[], ColumnType::Uuid),
            Signature::named(&[ColumnType::Interval],
                &["shift"],
                ColumnType::Uuid,
            ),
        ],
        "casefold" => &[Signature::new(&[ColumnType::Text], ColumnType::Text)],
        "to_regproc" => &[Signature::new(&[ColumnType::Text], ColumnType::Regproc)],
        "to_regprocedure" => &[Signature::new(&[ColumnType::Text],
            ColumnType::Regprocedure,
        )],
        "to_regclass" => &[Signature::new(&[ColumnType::Text], ColumnType::Regclass)],
        "to_regnamespace" => &[Signature::new(&[ColumnType::Text],
            ColumnType::Regnamespace,
        )],
        "to_regrole" => &[Signature::new(&[ColumnType::Text], ColumnType::Regrole)],
        "to_regtype" => &[Signature::new(&[ColumnType::Text], ColumnType::Regtype)],
        "pg_get_expr" => &[
            Signature::new(&[ColumnType::PgNodeTree, ColumnType::Oid],
                ColumnType::Text,
            ),
            Signature::new(&[ColumnType::PgNodeTree, ColumnType::Oid, ColumnType::Boolean],
                ColumnType::Text,
            ),
        ],
        "pg_get_partkeydef" => &[Signature::new(&[ColumnType::Oid], ColumnType::Text)],
        "pg_backend_pid" => &[Signature::new(&[], ColumnType::Integer)],
        "current_setting" => &[
            Signature::new(&[ColumnType::Text], ColumnType::Text),
            Signature::new(&[ColumnType::Text, ColumnType::Boolean],
                ColumnType::Text,
            ),
        ],
        "version" | "pg_listening_channels" => &[Signature::new(&[], ColumnType::Text)],
        "pg_notify" => &[Signature::new(&[ColumnType::Text, ColumnType::Text],
            ColumnType::Void,
        )],
        "pg_notification_queue_usage" => {
            &[Signature::new(&[], ColumnType::DoublePrecision)]
        },
        "pg_get_serial_sequence" => &[Signature::new(&[ColumnType::Text, ColumnType::Text],
            ColumnType::Text,
        )],
        "pg_get_sequence_data" => &[Signature::new(&[ColumnType::Regclass],
            ColumnType::Record,
        )],
        "pg_sequence_last_value" => &[Signature::new(&[ColumnType::Regclass],
            ColumnType::BigInteger,
        )],
        "pg_sequence_parameters" => &[Signature::new(&[ColumnType::Oid], ColumnType::Record)],
        "pg_get_triggerdef" | "pg_get_ruledef" => &[
            Signature::new(&[ColumnType::Oid], ColumnType::Text),
            Signature::new(&[ColumnType::Oid, ColumnType::Boolean],
                ColumnType::Text,
            ),
        ],
        "format_type" => &[Signature::new(&[ColumnType::Oid, ColumnType::Integer],
            ColumnType::Text,
        )],
        "pg_get_indexdef" => &[
            Signature::new(&[ColumnType::Oid], ColumnType::Text),
            Signature::new(&[ColumnType::Oid, ColumnType::Integer, ColumnType::Boolean],
                ColumnType::Text,
            ),
        ],
        "pg_get_viewdef" => &[
            Signature::new(&[ColumnType::Text], ColumnType::Text),
            Signature::new(&[ColumnType::Oid], ColumnType::Text),
            Signature::new(&[ColumnType::Text, ColumnType::Boolean],
                ColumnType::Text,
            ),
            Signature::new(&[ColumnType::Oid, ColumnType::Boolean],
                ColumnType::Text,
            ),
            Signature::new(&[ColumnType::Oid, ColumnType::Integer],
                ColumnType::Text,
            ),
        ],
        "pg_has_role" => &[
            Signature::new(&[ColumnType::Name, ColumnType::Name, ColumnType::Text],
                ColumnType::Boolean,
            ),
            Signature::new(&[ColumnType::Name, ColumnType::Oid, ColumnType::Text],
                ColumnType::Boolean,
            ),
            Signature::new(&[ColumnType::Oid, ColumnType::Name, ColumnType::Text],
                ColumnType::Boolean,
            ),
            Signature::new(&[ColumnType::Oid, ColumnType::Oid, ColumnType::Text],
                ColumnType::Boolean,
            ),
            Signature::new(&[ColumnType::Name, ColumnType::Text],
                ColumnType::Boolean,
            ),
            Signature::new(&[ColumnType::Oid, ColumnType::Text],
                ColumnType::Boolean,
            ),
        ],
        "has_column_privilege" => &[
            Signature::new(&[
                    ColumnType::Name,
                    ColumnType::Text,
                    ColumnType::Text,
                    ColumnType::Text,
                ],
                ColumnType::Boolean,
            ),
            Signature::new(&[
                    ColumnType::Name,
                    ColumnType::Text,
                    ColumnType::SmallInteger,
                    ColumnType::Text,
                ],
                ColumnType::Boolean,
            ),
            Signature::new(&[
                    ColumnType::Name,
                    ColumnType::Oid,
                    ColumnType::Text,
                    ColumnType::Text,
                ],
                ColumnType::Boolean,
            ),
            Signature::new(&[
                    ColumnType::Name,
                    ColumnType::Oid,
                    ColumnType::SmallInteger,
                    ColumnType::Text,
                ],
                ColumnType::Boolean,
            ),
            Signature::new(&[
                    ColumnType::Oid,
                    ColumnType::Text,
                    ColumnType::Text,
                    ColumnType::Text,
                ],
                ColumnType::Boolean,
            ),
            Signature::new(&[
                    ColumnType::Oid,
                    ColumnType::Text,
                    ColumnType::SmallInteger,
                    ColumnType::Text,
                ],
                ColumnType::Boolean,
            ),
            Signature::new(&[
                    ColumnType::Oid,
                    ColumnType::Oid,
                    ColumnType::Text,
                    ColumnType::Text,
                ],
                ColumnType::Boolean,
            ),
            Signature::new(&[
                    ColumnType::Oid,
                    ColumnType::Oid,
                    ColumnType::SmallInteger,
                    ColumnType::Text,
                ],
                ColumnType::Boolean,
            ),
            Signature::new(&[ColumnType::Text, ColumnType::Text, ColumnType::Text],
                ColumnType::Boolean,
            ),
            Signature::new(&[ColumnType::Text, ColumnType::SmallInteger, ColumnType::Text],
                ColumnType::Boolean,
            ),
            Signature::new(&[ColumnType::Oid, ColumnType::Text, ColumnType::Text],
                ColumnType::Boolean,
            ),
            Signature::new(&[ColumnType::Oid, ColumnType::SmallInteger, ColumnType::Text],
                ColumnType::Boolean,
            ),
        ],
        "has_table_privilege"
        | "has_database_privilege"
        | "has_schema_privilege"
        | "has_sequence_privilege"
        | "has_function_privilege" => &[
            Signature::new(&[ColumnType::Name, ColumnType::Text, ColumnType::Text],
                ColumnType::Boolean,
            ),
            Signature::new(&[ColumnType::Name, ColumnType::Oid, ColumnType::Text],
                ColumnType::Boolean,
            ),
            Signature::new(&[ColumnType::Oid, ColumnType::Text, ColumnType::Text],
                ColumnType::Boolean,
            ),
            Signature::new(&[ColumnType::Oid, ColumnType::Oid, ColumnType::Text],
                ColumnType::Boolean,
            ),
            Signature::new(&[ColumnType::Text, ColumnType::Text],
                ColumnType::Boolean,
            ),
            Signature::new(&[ColumnType::Oid, ColumnType::Text],
                ColumnType::Boolean,
            ),
        ],

}

pub(super) static NUMERIC_UNARY_ARGUMENTS: [ColumnType; 1] = [numeric_type()];
pub(super) static NUMERIC_BINARY_ARGUMENTS: [ColumnType; 2] = [numeric_type(), numeric_type()];
pub(super) static NUMERIC_TERNARY_ARGUMENTS: [ColumnType; 3] =
    [numeric_type(), numeric_type(), numeric_type()];
pub(super) static NUMERIC_SCALE_ARGUMENTS: [ColumnType; 2] = [numeric_type(), ColumnType::Integer];

pub(super) const fn numeric_type() -> ColumnType {
    ColumnType::Numeric {
        precision: None,
        scale: None,
    }
}

impl super::super::overload_resolution::SignatureParameters for Signature {
    fn parameter_count(&self) -> usize {
        self.argument_types.len()
    }
    fn name(&self, index: usize) -> Option<&str> {
        self.argument_names.get(index).copied()
    }
    fn has_default(&self, index: usize) -> bool {
        index
            >= self
                .argument_types
                .len()
                .saturating_sub(self.default_arguments)
    }
    fn canonical_type(
        &self,
        index: usize,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_core::memory::Produced<String>, uqa_core::ValueRetentionError> {
        super::super::overload_resolution::canonical_column_type_name_with_control(
            &self.argument_types[index],
            control,
        )
    }
}

/// A stored fixed binding selects one immutable descriptor without constructing candidate payloads.
pub(super) fn bound_signature(
    binding: &crate::ast::FunctionBinding,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<Option<&'static Signature>, crate::SQLError> {
    use super::super::overload_resolution::{
        canonical_column_type_name_with_control, canonical_routine_type_name_with_control,
    };
    control.check()?;
    if !binding.builtin {
        return Ok(None);
    }
    let Some((namespace, _)) = binding.name.split_once('.') else {
        return Ok(None);
    };
    if !namespace.eq_ignore_ascii_case("pg_catalog") {
        return Ok(None);
    }
    let Some((_, signatures)) = lookup(&binding.name) else {
        return Ok(None);
    };
    for signature in signatures {
        control.check()?;
        if signature.argument_types.len() != binding.argument_types.len() {
            continue;
        }
        let mut matched = true;
        for (declared, actual) in signature.argument_types.iter().zip(&binding.argument_types) {
            let declared = canonical_column_type_name_with_control(declared, control)?;
            let actual = canonical_routine_type_name_with_control(actual, control)?;
            if *declared != *actual {
                matched = false;
                break;
            }
        }
        if matched {
            return Ok(Some(signature));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests;
