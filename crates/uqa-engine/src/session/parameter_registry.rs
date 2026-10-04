//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The configuration state of a session that transactions do not restore.

use std::collections::{BTreeMap, BTreeSet};

/// What a session's transactions leave in place: the custom parameters it has created, which `PostgreSQL` keeps as placeholders for the life of the session even when the transaction that created one rolls back; the libraries it has loaded, which define their parameters and reserve their prefixes; and the settings its client gave at startup, which `RESET` restores.
#[derive(Default)]
pub(crate) struct SessionParameterRegistry {
    /// Lowercase name to the name as the session first spelled it.
    placeholders: BTreeMap<String, String>,
    libraries: BTreeSet<&'static str>,
    /// Lowercase name to the setting the client gave at startup.
    client_settings: BTreeMap<String, String>,
}

impl SessionParameterRegistry {
    /// The name a placeholder was created with.
    pub(crate) fn placeholder(&self, name: &str) -> Option<&str> {
        self.placeholders
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }

    pub(crate) fn add_placeholder(&mut self, name: &str) {
        self.placeholders
            .entry(name.to_ascii_lowercase())
            .or_insert_with(|| name.to_string());
    }

    pub(crate) fn remove_placeholder(&mut self, name: &str) {
        self.placeholders.remove(&name.to_ascii_lowercase());
    }

    /// The placeholders whose first component is `prefix`, by the names they were created with.
    pub(crate) fn placeholders_with_prefix(&self, prefix: &str) -> Vec<String> {
        self.placeholders
            .values()
            .filter(|name| {
                name.split_once('.')
                    .is_some_and(|(component, _)| component == prefix)
            })
            .cloned()
            .collect()
    }

    pub(crate) fn library_loaded(&self, library: &str) -> bool {
        self.libraries.contains(library)
    }

    /// Mark `library` loaded; `false` when the session had already loaded it.
    pub(crate) fn load_library(&mut self, library: &'static str) -> bool {
        self.libraries.insert(library)
    }

    /// The prefixes that loaded libraries reserve for their own parameters.
    pub(crate) fn reserved_prefixes(&self) -> impl Iterator<Item = &str> + '_ {
        self.libraries.iter().copied()
    }

    pub(crate) fn client_setting(&self, name: &str) -> Option<&str> {
        self.client_settings
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }

    pub(crate) fn set_client_setting(&mut self, name: &str, setting: String) {
        self.client_settings
            .insert(name.to_ascii_lowercase(), setting);
    }
}
