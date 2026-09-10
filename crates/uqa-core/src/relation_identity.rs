//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical SQL relation identities and legacy name decoding.

use serde::{Deserialize, Serialize};

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
        format!(
            "{}.{}",
            render_relation_component(&self.schema),
            render_relation_component(&self.name)
        )
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
        let components = parse_relation_components(value)?;
        match components.as_slice() {
            [name] => Ok((None, name.clone())),
            [schema, name] => Ok((Some(schema.clone()), name.clone())),
            _ => Err(format!("invalid persisted relation name `{value}`")),
        }
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

fn parse_relation_components(value: &str) -> Result<Vec<String>, String> {
    if value.is_empty() {
        return Err("persisted relation name is empty".to_string());
    }
    let mut components = Vec::with_capacity(2);
    let mut chars = value.char_indices().peekable();
    while chars.peek().is_some() {
        let mut component = String::new();
        if chars.peek().is_some_and(|(_, ch)| *ch == '"') {
            chars.next();
            let mut terminated = false;
            while let Some((_, ch)) = chars.next() {
                if ch != '"' {
                    component.push(ch);
                    continue;
                }
                if chars.peek().is_some_and(|(_, next)| *next == '"') {
                    chars.next();
                    component.push('"');
                } else {
                    terminated = true;
                    break;
                }
            }
            if !terminated {
                return Err(format!("unterminated quoted relation name `{value}`"));
            }
            if chars.peek().is_some_and(|(_, ch)| *ch != '.') {
                return Err(format!("invalid persisted relation name `{value}`"));
            }
        } else {
            while let Some((_, ch)) = chars.peek() {
                if *ch == '.' {
                    break;
                }
                if *ch == '"' {
                    return Err(format!("invalid persisted relation name `{value}`"));
                }
                component.push(*ch);
                chars.next();
            }
        }
        if component.is_empty() {
            return Err(format!("invalid persisted relation name `{value}`"));
        }
        components.push(component);
        if components.len() > 2 {
            return Err(format!("invalid persisted relation name `{value}`"));
        }
        match chars.next() {
            Some((_, '.')) if chars.peek().is_some() => {}
            Some(_) => return Err(format!("invalid persisted relation name `{value}`")),
            None => break,
        }
    }
    Ok(components)
}
