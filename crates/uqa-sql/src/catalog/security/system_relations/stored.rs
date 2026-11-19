//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Current system ACL fields and mutable assembly retain native payloads until immutable publication.

use super::{SystemAcl, SystemRelation, SystemRelationSecurities, SystemRelationSecurity};
use crate::{
    ast::stored_input::{invalid, InputError, Parts},
    catalog::{
        security::{
            projected_input::{Input, ScalarProtocol},
            TablePrivileges,
        },
        work::Work,
    },
    SQLError,
};
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{Budgeted, OwnedMap, Produced, ProductionControl},
    RelationIdentity,
};

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredSystemAcl<T> {
    pub system_acl_format: u32,
    pub entry: T,
}

impl StoredSystemAcl<SystemAcl> {
    /// The enclosing owner first validates the complete generic JSON value, including discarded fields and public scalar representations.
    pub fn from_stored_value_json(
        text: &str,
        protocol: ScalarProtocol,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, SQLError> {
        control.budget().ok_or_else(|| {
            SQLError::Internal("stored authority input requires admitted production".into())
        })?;
        let input = Input {
            control: *control,
            protocol,
        };
        let mut parts = Parts::new(*control);
        let (mut version, mut entry) = (0, None);
        input.fields(
            text.as_bytes(),
            &["system_acl_format", "entry"],
            [3, 3],
            true,
            |field, bytes| {
                if field == 0 {
                    version = input.unsigned(bytes)?.try_into().map_err(|_| invalid())?;
                } else {
                    entry = Some(parts.take(input.system_acl(bytes)?));
                }
                Ok(())
            },
        )?;
        Ok(parts.finish(Self {
            system_acl_format: version,
            entry: entry.expect("required system ACL"),
        })?)
    }
}
impl Input<'_> {
    fn system_acl(&self, bytes: &[u8]) -> Result<Produced<SystemAcl>, InputError> {
        let mut parts = Parts::new(self.control);
        let mut revision = [0; 16];
        let mut acl = None;
        self.fields(
            bytes,
            &["revision", "acl"],
            [3, 3],
            false,
            |field, bytes| {
                if field == 0 {
                    revision = self.identity(bytes)?;
                } else {
                    acl = parts.take(self.acl(bytes, Self::table_privileges)?);
                    if acl.is_none() {
                        return Err(invalid());
                    }
                }
                Ok(())
            },
        )?;
        parts.finish(SystemAcl {
            revision,
            acl: acl.expect("required system ACL entries"),
        })
    }
    fn table_privileges(&self, bytes: &[u8]) -> Result<TablePrivileges, InputError> {
        let mut value = TablePrivileges::default();
        self.fields(
            bytes,
            &[
                "select",
                "insert",
                "update",
                "delete",
                "truncate",
                "references",
                "trigger",
                "maintain",
            ],
            [0, 0],
            false,
            |field, bytes| {
                let boolean = self.boolean(bytes)?;
                match field {
                    0 => value.select = boolean,
                    1 => value.insert = boolean,
                    2 => value.update = boolean,
                    3 => value.delete = boolean,
                    4 => value.truncate = boolean,
                    5 => value.references = boolean,
                    6 => value.trigger = boolean,
                    _ => value.maintain = boolean,
                }
                Ok(())
            },
        )?;
        Ok(value)
    }
}

/// Borrow physical column keys while retaining each complete ACL. Replacements retire the old payload immediately; the final native column map is allocated once.
pub struct SystemRelationAssembly<'a> {
    relation: SystemRelation,
    table: Option<Budgeted<SystemAcl>>,
    columns: OwnedMap<&'a str, Budgeted<SystemAcl>>,
}
impl<'a> SystemRelationAssembly<'a> {
    pub fn new(relation: SystemRelation) -> Self {
        Self {
            relation,
            table: None,
            columns: OwnedMap::new(),
        }
    }

    pub fn insert(
        &mut self,
        column: &'a str,
        entry: Budgeted<SystemAcl>,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        control.check()?;
        if column.is_empty() {
            self.table = Some(entry);
        } else if self
            .relation
            .column_names_with_control(control)?
            .iter()
            .any(|name| name == column)
        {
            self.columns.insert_budgeted(
                column,
                entry,
                control.budget().expect("admitted system authority"),
            )?;
        } else {
            return Err(SQLError::Internal("unknown system ACL attribute".into()));
        }
        Ok(())
    }

    pub async fn finish(
        self,
        control: &ProductionControl<'_>,
        work: &mut AsyncImageInput<'_>,
    ) -> Result<Budgeted<SystemRelationSecurity>, SQLError> {
        let mut memory = control.empty_reservation();
        let mut value = SystemRelationSecurity::default();
        if let Some(entry) = self.table {
            let (entry, lease) = entry.into_parts();
            value.table = Some(entry);
            memory = control.combine(memory, Some(lease));
        }
        for (column, entry) in self.columns {
            work.checkpoint().await?;
            control.check()?;
            let name = Work::text(work, column, control).await?;
            let node = control.reserve(OwnedMap::<String, SystemAcl>::entry_bytes())?;
            let (name, name_memory) = name.into_parts();
            let (entry, entry_memory) = entry.into_parts();
            memory = control.combine(memory, node);
            memory = control.combine(memory, name_memory);
            memory = control.combine(memory, Some(entry_memory));
            value.columns.insert(name, entry);
        }
        Ok(control
            .finish(value, memory)?
            .into_budgeted()
            .unwrap_or_else(|_| unreachable!("admitted system authority")))
    }
}

pub fn insert_stored_entry(
    registry: &mut SystemRelationSecurities,
    relation: SystemRelation,
    value: Budgeted<SystemRelationSecurity>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let mut parts = Parts::new(*control);
    let schema = parts.take(control.copy_text(relation.namespace())?);
    let name = parts.take(control.copy_text(relation.name())?);
    let key = parts
        .finish(RelationIdentity::new(schema, name))?
        .into_budgeted()
        .unwrap_or_else(|_| unreachable!("admitted system authority"));
    registry.insert_image_entry(key, value)
}

#[cfg(test)]
mod tests;
