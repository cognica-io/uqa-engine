//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fixed function and overload indexes borrow their declarations and use fixed-size storage.

use super::{Declaration, Signature};
use crate::{
    ast::{ColumnType, FunctionBinding},
    type_resolution::overload_resolution::{
        builtin_canonical_type_name, canonical_routine_type_name_with_control,
    },
    SQLError,
};
use std::sync::OnceLock;
use uqa_core::memory::ProductionControl;

pub(in crate::type_resolution::fixed_builtin) struct Overloads {
    pub(super) signatures: &'static [Signature],
    ordered: &'static [usize],
}

fn parameter_name(ty: &'static ColumnType) -> &'static str {
    match ty {
        ColumnType::Named(name) => name.as_str(),
        ty => builtin_canonical_type_name(ty)
            .or_else(|| crate::catalog::type_metadata::builtin_type_name(ty))
            .expect("fixed signature parameter has a catalog name"),
    }
}

impl Overloads {
    pub(in crate::type_resolution::fixed_builtin) fn new<const N: usize>(
        signatures: &'static [Signature],
        order: &'static OnceLock<[usize; N]>,
    ) -> Self {
        let ordered = order.get_or_init(|| {
            assert_eq!(N, signatures.len());
            let mut indices = std::array::from_fn(|i| i);
            indices.sort_unstable_by(|&left, &right| {
                let a = signatures[left].argument_types;
                let b = signatures[right].argument_types;
                a.len()
                    .cmp(&b.len())
                    .then_with(|| {
                        a.iter()
                            .map(parameter_name)
                            .cmp(b.iter().map(parameter_name))
                    })
                    .then_with(|| left.cmp(&right))
            });
            indices
        });
        Self {
            signatures,
            ordered,
        }
    }

    pub(super) fn bound_signature(
        &self,
        binding: &FunctionBinding,
        control: &ProductionControl<'_>,
    ) -> Result<Option<&'static Signature>, SQLError> {
        let arity = binding.argument_types.len();
        let count = |row: usize| self.signatures[row].argument_types.len();
        let start = self.ordered.partition_point(|&row| count(row) < arity);
        let tail = &self.ordered[start..];
        let mut candidates = &tail[..tail.partition_point(|&row| count(row) == arity)];
        for (index, actual) in binding.argument_types.iter().enumerate() {
            if candidates.is_empty() {
                return Ok(None);
            }
            let actual = canonical_routine_type_name_with_control(actual, control)?;
            let parameter =
                |row: usize| parameter_name(&self.signatures[row].argument_types[index]);
            let start = candidates.partition_point(|&row| parameter(row) < actual.as_str());
            let tail = &candidates[start..];
            candidates = &tail[..tail.partition_point(|&row| parameter(row) == actual.as_str())];
        }
        Ok(candidates.first().map(|&row| &self.signatures[row]))
    }
}

const GROUPS: &[&[Declaration]] = &[
    super::super::standard::DECLARATIONS,
    super::super::regular_expressions::DECLARATIONS,
    super::super::regular_expressions::TEXT_ARRAY_DECLARATIONS,
    super::POLYMORPHIC,
    super::LOCAL,
];

const NAME_COUNT: usize = {
    let mut count = 0;
    let mut group = 0;
    while group < GROUPS.len() {
        let mut item = 0;
        while item < GROUPS[group].len() {
            count += GROUPS[group][item].names.len();
            item += 1;
        }
        group += 1;
    }
    count
};

const CAPACITY: usize = (2 * NAME_COUNT).next_power_of_two();
type Entry = (&'static str, &'static Declaration);

const fn name_slot(name: &str) -> usize {
    let bytes = name.as_bytes();
    let mut hash = 0x811c_9dc5_u32;
    let mut i = 0;
    while i < bytes.len() {
        hash = (hash ^ bytes[i].to_ascii_lowercase() as u32).wrapping_mul(0x0100_0193);
        i += 1;
    }
    hash as usize % CAPACITY
}

struct Registry {
    slots: [Option<Entry>; CAPACITY],
    longest: usize,
}

static REGISTRY: Registry = {
    let mut slots = [None; CAPACITY];
    let mut longest = 0;
    let mut group = 0;
    while group < GROUPS.len() {
        let mut index = 0;
        while index < GROUPS[group].len() {
            let declaration = &GROUPS[group][index];
            let mut alias = 0;
            while alias < declaration.names.len() {
                let name = declaration.names[alias];
                let mut slot = name_slot(name);
                while slots[slot].is_some() {
                    slot = (slot + 1) % CAPACITY;
                }
                slots[slot] = Some((name, declaration));
                if longest < name.len() {
                    longest = name.len();
                }
                alias += 1;
            }
            index += 1;
        }
        group += 1;
    }
    Registry { slots, longest }
};

pub(super) fn lookup(name: &str) -> Option<Entry> {
    let local = match name.split_once('.') {
        Some((namespace, local)) if namespace.eq_ignore_ascii_case("pg_catalog") => local,
        Some(_) => return None,
        None => name,
    };
    if local.len() > REGISTRY.longest {
        return None;
    }
    let mut slot = name_slot(local);
    while let Some(entry) = REGISTRY.slots[slot] {
        if entry.0.eq_ignore_ascii_case(local) {
            return Some(entry);
        }
        slot = (slot + 1) % CAPACITY;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::type_resolution::overload_resolution::canonical_column_type_name;

    #[test]
    fn indexed_signatures_match_the_first_declared_overload_for_every_name_and_type_tuple() {
        for declaration in GROUPS.iter().flat_map(|group| group.iter()) {
            let signatures = (declaration.overloads)().signatures;
            for &name in declaration.names {
                let qualified = format!("PG_CATALOG.{}", name.to_ascii_uppercase());
                let (registered, entry) = lookup(&qualified).unwrap();
                assert_eq!(registered, name);
                for signature in signatures {
                    let arguments: Vec<_> = signature
                        .argument_types
                        .iter()
                        .map(canonical_column_type_name)
                        .collect();
                    assert!(
                        signature
                            .argument_types
                            .iter()
                            .map(parameter_name)
                            .eq(arguments.iter().map(String::as_str)),
                        "{name}: {arguments:?}"
                    );
                    let expected = signatures
                        .iter()
                        .find(|other| {
                            other
                                .argument_types
                                .iter()
                                .map(canonical_column_type_name)
                                .eq(arguments.iter().cloned())
                        })
                        .unwrap();
                    let binding = FunctionBinding {
                        object_id: None,
                        name: qualified.clone(),
                        argument_types: arguments,
                        builtin: true,
                        dispatch: None,
                        invocation: None,
                        value_binding: None,
                        resolution_error: None,
                        input_collation: None,
                    };
                    let selected = (entry.overloads)()
                        .bound_signature(&binding, &ProductionControl::uncontrolled())
                        .unwrap()
                        .unwrap();
                    assert!(
                        std::ptr::eq(selected, expected),
                        "{name}: {:?}",
                        binding.argument_types
                    );
                }
            }
        }
    }
}
