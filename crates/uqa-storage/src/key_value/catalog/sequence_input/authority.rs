//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Flattened sequence authority retains sorted last-value fields and explicit format selection.

use crate::{
    catalog::{
        input::strings::{invalid, json_error, next, take},
        SequenceSecurityRow,
    },
    read_control::StorageReadControl,
    StorageBackendResult,
};
use uqa_core::{
    catalog_acl::input::{projected, sequence},
    catalog_sequence::{BoundSequenceSecurity, LegacySequenceSecurity},
    json::{decode_json_string, JsonReader, JsonToken},
    memory::{Budgeted, OwnedMap, ProductionControl},
};

pub(super) fn read(
    fields: &OwnedMap<String, &[u8]>,
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<SequenceSecurityRow>> {
    let bound = if let Some(version) = fields.get("sequence_security_format") {
        if serde_json::from_slice::<u64>(version).ok() != Some(1) {
            return Err(invalid("unsupported sequence security format"));
        }
        true
    } else {
        false
    };
    let mut role = None;
    let mut name = None;
    let mut current_acl = None;
    let mut legacy_acl = Budgeted::new(None, control.memory().empty_reservation());
    for (key, bytes) in fields {
        control.check()?;
        match key.as_str() {
            "sequence_security_format" if bound => (),
            "role_owner" if bound => {
                role = Some(
                    projected::role_identity(bytes, control.memory(), control.cancellation())
                        .map_err(json_error)?,
                );
            }
            "acl" if bound => {
                current_acl = Some(
                    sequence::projected_entries(bytes, control.memory(), control.cancellation())
                        .map_err(json_error)?,
                );
            }
            "role_owner" => {
                let mut reader =
                    JsonReader::from_slice(bytes, control.memory(), control.cancellation());
                let JsonToken::String(value) = next(&mut reader)? else {
                    return Err(invalid("expected a sequence role owner name"));
                };
                name = Some(
                    decode_json_string(value, control.memory(), control.cancellation())
                        .map_err(json_error)?,
                );
            }
            "acl" => {
                legacy_acl = sequence::projected_legacy_entries(
                    bytes,
                    control.memory(),
                    control.cancellation(),
                )
                .map_err(json_error)?;
            }
            _ => return Err(invalid(format!("unknown field `{key}`"))),
        }
    }
    let mut memory = control.memory().empty_reservation();
    let row = if bound {
        SequenceSecurityRow::Bound(BoundSequenceSecurity {
            role_owner: role.ok_or_else(|| invalid("missing field `role_owner`"))?,
            acl: take(
                current_acl.ok_or_else(|| invalid("missing field `acl`"))?,
                &mut memory,
            ),
        })
    } else {
        let owner = if let Some(name) = name {
            name
        } else {
            ProductionControl::new(
                control.memory(),
                control.cancellation(),
                control.cancellation(),
            )
            .copy_text("uqa")?
            .into_budgeted()
            .expect("admitted default sequence owner")
        };
        SequenceSecurityRow::Legacy(LegacySequenceSecurity {
            role_owner: take(owner, &mut memory),
            acl: take(legacy_acl, &mut memory),
        })
    };
    Ok(Budgeted::new(row, memory))
}
