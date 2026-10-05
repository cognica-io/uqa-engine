//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Initial-transaction upgrade of the user-defined type names that earlier releases recorded in stored syntax, stored plans, routine declarations and routine bindings to the identities current releases record, so that a type renamed or moved after the upgrade keeps every stored reference meaning the same type. Earlier releases wrote a type as its name was spelled at definition time; a name without a schema meant the type the open session's default search path found.

use std::collections::BTreeMap;

use serde::Deserialize;
use uqa_core::RelationIdentity;
use uqa_sql::binding::stored_types::TypeNameSite;
use uqa_storage::{CatalogFacade, StorageBackendError, StorageBackendResult};

mod forms;

const VERSION_KEY: &str = "sql_stored_type_identity_version";
/// The schema an unqualified name meant when earlier releases stored it: the default search path's.
const DEFAULT_SCHEMA: &str = "public";
const DOMAIN_RECORD_PREFIX: &str = "uqa.sql.domain.v1:";
/// The metadata key of the domain catalog: the format marker of per-domain records, or in earlier formats the domains themselves.
const DOMAINS_METADATA_KEY: &str = "sql_domains_json";

/// Upgrade every stored definition once, inside the caller's initial catalog transaction; the version marker is written only after every changed record.
pub fn upgrade_stored_type_names(catalog: &dyn CatalogFacade) -> StorageBackendResult<()> {
    match catalog.get_metadata(VERSION_KEY)?.as_deref() {
        Some("1") => return Ok(()),
        None => {}
        Some(_) => {
            return Err(StorageBackendError::Other(
                "unsupported stored type identity version".into(),
            ))
        }
    }
    let names = UserTypeNames::load(catalog)?;
    if !names.is_empty() {
        forms::upgrade(catalog, &names)
            .map_err(|error| StorageBackendError::backend("stored type identity upgrade", error))?;
    }
    catalog.set_metadata(VERSION_KEY, "1")
}

/// The user-defined types of the catalog by schema and name, each with its identity.
struct UserTypeNames {
    identities: BTreeMap<(String, String), String>,
}

/// The fields of a stored domain record that name it.
#[derive(Deserialize)]
struct StoredDomainName {
    oid: u32,
    identity: RelationIdentity,
}

impl UserTypeNames {
    /// The domains of per-domain records, and of the domain catalog itself in the formats that kept them there, which a later restoration step converts.
    fn load(catalog: &dyn CatalogFacade) -> StorageBackendResult<Self> {
        let mut names = Self {
            identities: BTreeMap::new(),
        };
        for (_, json) in catalog.metadata_with_prefix(DOMAIN_RECORD_PREFIX)? {
            names.insert(serde_json::from_str(&json)?);
        }
        if let Some(json) = catalog.get_metadata(DOMAINS_METADATA_KEY)? {
            let value: serde_json::Value = serde_json::from_str(&json)?;
            let embedded = match value.get("domain_catalog_format") {
                Some(_) => value.get("domains"),
                None => Some(&value),
            };
            if let Some(serde_json::Value::Object(domains)) = embedded {
                for domain in domains.values() {
                    names.insert(serde_json::from_value(domain.clone())?);
                }
            }
        }
        Ok(names)
    }

    fn insert(&mut self, domain: StoredDomainName) {
        self.identities.insert(
            (domain.identity.schema, domain.identity.name),
            format!("domain#{}", domain.oid),
        );
    }

    fn is_empty(&self) -> bool {
        self.identities.is_empty()
    }

    /// The identity of the type a stored name meant, with its array dimensions, or `None` for a name that is no user-defined type. An unqualified name means the first schema of `search_path` holding a type of that name. A canonical name was folded to lower case, so it matches a type whose name differs only in case when no other does.
    fn identity(&self, name: &str, site: TypeNameSite, search_path: &[String]) -> Option<String> {
        let mut element = name.trim();
        let mut dimensions = 0usize;
        while let Some(inner) = element.strip_suffix("[]") {
            element = inner.trim_end();
            dimensions += 1;
        }
        let parts = uqa_sql::parse_regobject_name(element)?;
        let identity = match parts.as_slice() {
            [local] => search_path
                .iter()
                .find_map(|schema| self.lookup(schema, local, site))?,
            [schema, local] => self.lookup(schema, local, site)?,
            _ => return None,
        };
        Some(format!("{identity}{}", "[]".repeat(dimensions)))
    }

    fn lookup(&self, schema: &str, local: &str, site: TypeNameSite) -> Option<&String> {
        if let Some(identity) = self
            .identities
            .get(&(schema.to_string(), local.to_string()))
        {
            return Some(identity);
        }
        if site != TypeNameSite::Canonical {
            return None;
        }
        let mut folded = self
            .identities
            .iter()
            .filter(|((stored_schema, stored_local), _)| {
                stored_schema.to_lowercase() == schema.to_lowercase()
                    && stored_local.to_lowercase() == local.to_lowercase()
            });
        match (folded.next(), folded.next()) {
            (Some((_, identity)), None) => Some(identity),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests;
