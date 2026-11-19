//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete legacy ACL syntax precedes owner lookup and ordered incarnation binding.

use super::LegacyRoutineAclEntry;
use uqa_core::{
    catalog_acl::AclGrantee,
    json::{
        JsonDiagnostic, JsonInputError, JsonPosition, JsonReadError, JsonReadWork, JsonValue,
        JsonValueKind,
    },
    memory::{Produced, ProductionControl, ProductionVec},
};

/// Construct the complete optional legacy ACL from a projected generic value. Missing and null ACLs remain absent; all entry syntax is checked before any named role is bound.
pub async fn read_legacy_acl_with_work<W: JsonReadWork<Error = JsonReadError>>(
    value: Option<JsonValue<'_>>,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<Produced<Option<Vec<LegacyRoutineAclEntry>>>, JsonInputError> {
    checkpoint(control, work).await?;
    let Some(value) = value.filter(|value| value.kind() != JsonValueKind::Null) else {
        return Ok(control
            .finish(None, control.empty_reservation())
            .map_err(JsonReadError::from)?);
    };
    let Some(items) = value.elements() else {
        return Err(
            JsonDiagnostic::projected_type_with_work(value, "a sequence", control, work)
                .await?
                .into(),
        );
    };
    let mut entries = ProductionVec::new(*control);
    for value in items {
        checkpoint(control, work).await?;
        entries
            .push_produced(read_entry(value, control, work).await?)
            .map_err(JsonReadError::from)?;
    }
    let (entries, memory) = entries.finish().map_err(JsonReadError::from)?.into_parts();
    Ok(control
        .finish(Some(entries), memory)
        .map_err(JsonReadError::from)?)
}

#[derive(Default)]
struct Fields {
    role: Option<Produced<AclGrantee>>,
    grantor: Option<Produced<String>>,
    grant_option: Option<bool>,
}

async fn read_entry<W: JsonReadWork<Error = JsonReadError>>(
    value: JsonValue<'_>,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<Produced<LegacyRoutineAclEntry>, JsonInputError> {
    let mut fields = Fields::default();
    if let Some(items) = value.fields() {
        for (name, value) in items {
            checkpoint(control, work).await?;
            let index = match name {
                "role" => 0,
                "grantor" => 1,
                "grant_option" => 2,
                _ => continue,
            };
            fields.read(index, value, control, work).await?;
        }
    } else if let Some(items) = value.elements() {
        let length = items.len();
        for (index, value) in items.enumerate() {
            checkpoint(control, work).await?;
            if index == 3 {
                return Err(JsonDiagnostic::invalid_length(
                    length,
                    "fewer elements in array",
                    JsonPosition::unlocated(),
                )
                .into());
            }
            fields.read(index, value, control, work).await?;
        }
        if length < 3 {
            // The defaulted grantor consumes its position before the required third field.
            return Err(JsonDiagnostic::invalid_length(
                if length == 0 { 0 } else { 2 },
                "struct LegacyRoutineAclEntry with 3 elements",
                JsonPosition::unlocated(),
            )
            .into());
        }
    } else {
        return Err(JsonDiagnostic::projected_type_with_work(
            value,
            "struct LegacyRoutineAclEntry",
            control,
            work,
        )
        .await?
        .into());
    }
    fields.finish(control)
}

impl Fields {
    async fn read<W: JsonReadWork<Error = JsonReadError>>(
        &mut self,
        index: usize,
        value: JsonValue<'_>,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<(), JsonInputError> {
        match index {
            0 => {
                self.role =
                    Some(AclGrantee::from_json_value_with_work(value, control, work).await?);
            }
            1 if value.kind() == JsonValueKind::Null => self.grantor = None,
            1 => self.grantor = Some(value.read_string_with_work(control, work).await?),
            2 => self.grant_option = Some(value.read_bool_with_work(control, work).await?),
            _ => unreachable!("declared legacy ACL field"),
        }
        Ok(())
    }

    fn finish(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<LegacyRoutineAclEntry>, JsonInputError> {
        let role = self
            .role
            .ok_or_else(|| JsonDiagnostic::missing_field("role", JsonPosition::unlocated()))?;
        let grant_option = self.grant_option.ok_or_else(|| {
            JsonDiagnostic::missing_field("grant_option", JsonPosition::unlocated())
        })?;
        let (role, mut memory) = role.into_parts();
        let grantor = self.grantor.map(|value| {
            let (value, allocation) = value.into_parts();
            memory = control.combine(memory.take(), allocation);
            value
        });
        Ok(control
            .finish(
                LegacyRoutineAclEntry {
                    role,
                    grantor,
                    grant_option,
                },
                memory,
            )
            .map_err(JsonReadError::from)?)
    }
}

async fn checkpoint<W: JsonReadWork<Error = JsonReadError>>(
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<(), JsonInputError> {
    control.check().map_err(JsonReadError::from)?;
    work.checkpoint().await?;
    Ok(())
}

#[cfg(test)]
mod tests;
