//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table owner bindings read at one committed snapshot.

use parking_lot::Mutex;

use crate::mvcc::native::NativeRecordOwner;

/// A table's owner and whether a catalog definition holds it, or `None` for a table without a binding.
pub(crate) type TableBinding = Option<(NativeRecordOwner, bool)>;

const TABLES: usize = 32;
const NAME_BYTES: usize = 256;

/// The committed owner bindings of the tables read through one snapshot. Every document operation of a table begins with its binding, and a committed record never changes at a snapshot, so each binding is read once for as long as a session keeps the snapshot. A binding the reading transaction changed is private to it and is not held here.
#[derive(Default)]
pub(crate) struct TableOwners {
    bindings: Mutex<Vec<(Box<str>, TableBinding)>>,
}

impl TableOwners {
    pub(crate) fn get(&self, table: &str) -> Option<TableBinding> {
        self.bindings
            .lock()
            .iter()
            .find(|(name, _)| &**name == table)
            .map(|(_, binding)| *binding)
    }

    pub(crate) fn remember(&self, table: &str, binding: TableBinding) {
        if table.len() > NAME_BYTES {
            return;
        }
        let mut bindings = self.bindings.lock();
        if bindings.iter().any(|(name, _)| &**name == table) {
            return;
        }
        if bindings.len() == TABLES {
            bindings.remove(0);
        }
        bindings.push((table.into(), binding));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner(byte: u8) -> (NativeRecordOwner, bool) {
        (
            NativeRecordOwner::Object {
                identity: [byte; 16],
                generation: [byte.wrapping_add(1); 16],
            },
            byte.is_multiple_of(2),
        )
    }

    #[test]
    fn bindings_are_kept_by_name_up_to_a_bound() {
        let owners = TableOwners::default();
        assert_eq!(owners.get("public.a"), None);
        owners.remember("public.a", Some(owner(1)));
        owners.remember("public.missing", None);
        // The first binding read for a name is the committed one and stays.
        owners.remember("public.a", Some(owner(9)));
        assert_eq!(owners.get("public.a"), Some(Some(owner(1))));
        assert_eq!(owners.get("public.missing"), Some(None));
        assert_eq!(owners.get("public.b"), None);
        for table in 0..TABLES {
            owners.remember(&format!("public.t{table}"), Some(owner(table as u8)));
        }
        // The oldest names made room.
        assert_eq!(owners.get("public.a"), None);
        assert_eq!(owners.get("public.t31"), Some(Some(owner(31))));
        assert_eq!(owners.bindings.lock().len(), TABLES);
        let long = "n".repeat(NAME_BYTES + 1);
        owners.remember(&long, Some(owner(3)));
        assert_eq!(owners.get(&long), None);
    }
}
