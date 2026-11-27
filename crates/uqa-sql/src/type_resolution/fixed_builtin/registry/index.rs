//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Process-wide indexes for fixed function names and stored overload signatures.

use super::Signature;
use crate::type_resolution::overload_resolution::{
    builtin_canonical_type_name, canonical_column_type_name,
    canonical_routine_type_name_with_control,
};
use std::{
    borrow::Cow,
    collections::HashMap,
    hash::{Hash, Hasher},
    sync::{LazyLock, OnceLock},
};

#[derive(Clone, Copy, Eq)]
struct Name<'a>(&'a str);

impl PartialEq for Name<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.0.eq_ignore_ascii_case(other.0)
    }
}

impl Hash for Name<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.len().hash(state);
        for byte in self.0.bytes() {
            state.write_u8(byte.to_ascii_lowercase());
        }
    }
}

pub(super) struct Entry {
    pub(super) name: &'static str,
    pub(super) signatures: &'static [Signature],
    by_arity: OnceLock<HashMap<usize, Parameters>>,
}

#[derive(Default)]
struct Parameters {
    next: HashMap<Cow<'static, str>, Self>,
    signature: Option<&'static Signature>,
}

impl Entry {
    pub(super) fn bound_signature(
        &self,
        binding: &crate::ast::FunctionBinding,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<&'static Signature>, crate::SQLError> {
        let index = self.by_arity.get_or_init(|| {
            let mut index = HashMap::<usize, Parameters>::new();
            for signature in self.signatures {
                let mut node = index.entry(signature.argument_types.len()).or_default();
                for ty in signature.argument_types {
                    let name = builtin_canonical_type_name(ty)
                        .map_or_else(|| Cow::Owned(canonical_column_type_name(ty)), Cow::Borrowed);
                    node = node.next.entry(name).or_default();
                }
                node.signature.get_or_insert(signature);
            }
            index
        });
        let Some(mut node) = index.get(&binding.argument_types.len()) else {
            return Ok(None);
        };
        for actual in &binding.argument_types {
            let actual = canonical_routine_type_name_with_control(actual, control)?;
            let Some(next) = node.next.get(actual.as_str()) else {
                return Ok(None);
            };
            node = next;
        }
        Ok(node.signature)
    }
}

#[derive(Default)]
struct Registry {
    positions: HashMap<Name<'static>, usize>,
    entries: Vec<Entry>,
    longest: usize,
}

static REGISTRY: LazyLock<Registry> = LazyLock::new(|| {
    let mut registry = Registry::default();
    let mut register = |name: &'static str, signatures: &'static [Signature]| {
        registry.longest = registry.longest.max(name.len());
        registry.positions.entry(Name(name)).or_insert_with(|| {
            let position = registry.entries.len();
            registry.entries.push(Entry {
                name,
                signatures,
                by_arity: OnceLock::new(),
            });
            position
        });
    };
    super::super::standard::register(&mut register);
    super::super::regular_expressions::register(&mut register);
    super::register_polymorphic(&mut register);
    super::register_local(&mut register);
    registry
});

pub(super) fn lookup(name: &str) -> Option<&'static Entry> {
    let local = match name.split_once('.') {
        Some((namespace, local)) if namespace.eq_ignore_ascii_case("pg_catalog") => local,
        Some(_) => return None,
        None => name,
    };
    if local.len() > REGISTRY.longest {
        return None;
    }
    let position = *REGISTRY.positions.get(&Name(local))?;
    Some(&REGISTRY.entries[position])
}
