//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL reference names with transient parser origins, independent of name identity.

use std::{
    borrow::Borrow,
    cmp::Ordering,
    fmt,
    hash::{Hash, Hasher},
    ops::{Deref, DerefMut},
};

/// A reference's name and optional one-based character position. Identity, ordering, hashing, debug rendering and serialization depend only on the name; serialized expressions never retain an earlier client's SQL position.
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct ReferenceName {
    name: String,
    #[serde(skip)]
    position: Option<u32>,
}

impl ReferenceName {
    #[must_use]
    pub fn with_position(mut self, position: Option<u32>) -> Self {
        self.position = position.filter(|position| *position != 0);
        self
    }

    pub(crate) fn set_position(&mut self, position: Option<u32>) {
        self.position = position.filter(|position| *position != 0);
    }

    #[must_use]
    pub const fn position(&self) -> Option<u32> {
        self.position
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.name
    }

    /// Replace a bound name while preserving the reference's original source position.
    pub fn rename(&mut self, name: &str) {
        name.clone_into(&mut self.name);
    }
}

impl From<String> for ReferenceName {
    fn from(name: String) -> Self {
        Self {
            name,
            position: None,
        }
    }
}
impl From<&str> for ReferenceName {
    fn from(name: &str) -> Self {
        name.to_owned().into()
    }
}
impl From<&String> for ReferenceName {
    fn from(name: &String) -> Self {
        name.clone().into()
    }
}
impl From<&ReferenceName> for ReferenceName {
    fn from(name: &ReferenceName) -> Self {
        name.clone()
    }
}
impl From<ReferenceName> for String {
    fn from(name: ReferenceName) -> Self {
        name.name
    }
}
impl From<&ReferenceName> for String {
    fn from(name: &ReferenceName) -> Self {
        name.name.clone()
    }
}
impl Deref for ReferenceName {
    type Target = String;
    fn deref(&self) -> &String {
        &self.name
    }
}
impl DerefMut for ReferenceName {
    fn deref_mut(&mut self) -> &mut String {
        &mut self.name
    }
}
impl AsRef<str> for ReferenceName {
    fn as_ref(&self) -> &str {
        &self.name
    }
}
impl Borrow<str> for ReferenceName {
    fn borrow(&self) -> &str {
        &self.name
    }
}
impl fmt::Display for ReferenceName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.name.fmt(formatter)
    }
}
impl fmt::Debug for ReferenceName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.name, formatter)
    }
}
impl PartialEq for ReferenceName {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}
impl Eq for ReferenceName {}
impl PartialOrd for ReferenceName {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for ReferenceName {
    fn cmp(&self, other: &Self) -> Ordering {
        self.name.cmp(&other.name)
    }
}
impl Hash for ReferenceName {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.name.hash(state);
    }
}
impl PartialEq<String> for ReferenceName {
    fn eq(&self, other: &String) -> bool {
        self.name == *other
    }
}
impl PartialEq<ReferenceName> for String {
    fn eq(&self, other: &ReferenceName) -> bool {
        *self == other.name
    }
}
impl PartialEq<str> for ReferenceName {
    fn eq(&self, other: &str) -> bool {
        self.name == other
    }
}
impl PartialEq<&str> for ReferenceName {
    fn eq(&self, other: &&str) -> bool {
        self.name == *other
    }
}
impl PartialEq<ReferenceName> for str {
    fn eq(&self, other: &ReferenceName) -> bool {
        self == other.name
    }
}

#[cfg(test)]
mod tests {
    use super::ReferenceName;
    use std::collections::{BTreeSet, HashSet};

    #[test]
    fn diagnostic_origin_is_not_name_identity_or_persistent_state() {
        let first = ReferenceName::from("Column").with_position(Some(12));
        let second = ReferenceName::from("Column").with_position(Some(41));
        assert_eq!(first, second);
        assert_eq!(HashSet::from([first.clone(), second.clone()]).len(), 1);
        assert_eq!(BTreeSet::from([first.clone(), second]).len(), 1);
        assert_eq!(format!("{first:?}"), format!("{:?}", "Column"));
        let encoded = serde_json::to_string(&first).unwrap();
        assert_eq!(encoded, serde_json::to_string("Column").unwrap());
        let restored: ReferenceName = serde_json::from_str(&encoded).unwrap();
        assert_eq!(restored, first);
        assert_eq!(restored.position(), None);
        let mut renamed = first;
        renamed.rename("bound_column");
        assert_eq!(renamed, "bound_column");
        assert_eq!(renamed.position(), Some(12));
    }
}
