//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical SQL relation identities and legacy name decoding.

use serde::{Deserialize, Serialize};

mod borrowed;
mod production;
pub use borrowed::{RelationName, RelationReference, RelationReferenceError};
#[cfg(test)]
mod tests;

/// Durable identity of a SQL relation.
///
/// The schema and local name are stored separately so `foo` and
/// `public.foo` can never become two physical catalog identities for the
/// same SQL object.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RelationIdentity {
    pub schema: String,
    pub name: String,
}

impl RelationIdentity {
    pub fn new(schema: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            schema: schema.into(),
            name: name.into(),
        }
    }

    pub fn qualified_name(&self) -> String {
        self.qualified_name_with_control(&crate::memory::ProductionControl::uncontrolled())
            .expect("ordinary relation names cannot be cancelled or limited")
            .into_uncontrolled()
            .expect("ordinary relation names have no reservation")
    }

    pub fn qualified_name_with_control(
        &self,
        control: &crate::memory::ProductionControl<'_>,
    ) -> Result<crate::memory::Produced<String>, crate::ValueRetentionError> {
        RelationReference {
            schema: Some(RelationName::decoded(&self.schema)),
            name: RelationName::decoded(&self.name),
        }
        .render_with_control(control)
    }

    /// Physical owner keys that can refer to this relation. New writes use
    /// only the canonical qualified name. Catalog cleanup also accepts the
    /// former unqualified key for `public` relations so data written before
    /// relation identities became schema-aware cannot survive its owner.
    pub fn canonical_and_legacy_public_names(&self) -> Vec<String> {
        let canonical = self.qualified_name();
        if self.schema != "public" {
            return vec![canonical];
        }
        let mut names = vec![canonical];
        let rendered_alias = render_relation_component(&self.name);
        if !names.contains(&rendered_alias) {
            names.push(rendered_alias);
        }
        // The direct Rust API historically accepted a decoded local name as
        // well as SQL-rendered text. Include that spelling only when parsing
        // it maps back to this exact relation; for example, raw `a.b` must not
        // be removed while dropping the distinct public relation `"a.b"`.
        if RelationIdentity::from_legacy_name(&self.name).is_ok_and(|raw| raw == *self)
            && !names.contains(&self.name)
        {
            names.push(self.name.clone());
        }
        names
    }

    /// Decode a SQL relation reference or a former flat catalog key.
    /// Unqualified objects belong to `public`. Quoted components preserve
    /// embedded dots and escaped quotes, so `public.\"a.b\"` is distinct from
    /// `\"public.a\".b` all the way down to physical storage keys.
    pub fn from_legacy_name(value: &str) -> Result<Self, String> {
        let (schema, name) = Self::parse_reference(value)?;
        Ok(Self::new(
            schema.unwrap_or_else(|| "public".to_string()),
            name,
        ))
    }

    /// Recover an index identity from the former flat index catalog. The stored value is a decoded local identifier rather than a relation reference, so dots and quotes remain part of the local name and the owning table supplies the schema.
    pub fn from_legacy_index_name(value: &str, table: &Self) -> Self {
        Self::new(&table.schema, value)
    }

    /// Parse a possibly-unqualified SQL relation reference without choosing a
    /// search-path schema. Components use `PostgreSQL` double-quote escaping.
    pub fn parse_reference(value: &str) -> Result<(Option<String>, String), String> {
        let reference = Self::try_borrowed_reference(value).map_err(|error| error.to_string())?;
        Ok((
            reference.schema.map(|schema| schema.to_string()),
            reference.name.to_string(),
        ))
    }

    /// Parse a relation reference without allocating decoded names or error text. Qualified references retain their separate schema component.
    pub fn borrowed_reference(value: &str) -> Option<RelationReference<'_>> {
        Self::try_borrowed_reference(value).ok()
    }

    /// Retain an invalid reference as a borrowing diagnostic so controlled consumers can admit its formatting.
    pub fn try_borrowed_reference(
        value: &str,
    ) -> Result<RelationReference<'_>, RelationReferenceError<'_>> {
        borrowed::parse_result(value)
    }
}

fn render_relation_component(component: &str) -> String {
    let can_render_bare = component
        .bytes()
        .enumerate()
        .all(|(index, byte)| match byte {
            b'a'..=b'z' | b'_' => true,
            b'0'..=b'9' | b'$' => index != 0,
            _ => false,
        });
    if can_render_bare && !component.is_empty() {
        component.to_string()
    } else {
        format!("\"{}\"", component.replace('"', "\"\""))
    }
}
