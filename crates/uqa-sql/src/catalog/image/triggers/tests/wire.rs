//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent literal framing and native declaration metadata.

use std::collections::BTreeMap;

pub(super) struct Wire {
    pub bytes: Vec<u8>,
    pub positions: BTreeMap<&'static str, usize>,
}
impl Wire {
    fn mark(&mut self, label: &'static str) {
        self.positions.insert(label, self.bytes.len());
    }
    fn text(&mut self, value: &str) {
        self.bytes.extend((value.len() as u64).to_le_bytes());
        self.bytes.extend(value.as_bytes());
    }
}

pub(super) fn fixture() -> Wire {
    let mut wire = Wire {
        bytes: b"UQATRG01\x01\0\0\0\0\0\0\0".to_vec(),
        positions: BTreeMap::new(),
    };
    wire.text("s");
    wire.text("t");
    wire.mark("entry_count");
    wire.bytes.extend(1u64.to_le_bytes());
    wire.mark("entry");
    wire.text("watch");
    wire.mark("function_flag");
    wire.bytes.push(1);
    wire.mark("function_id");
    wire.bytes.extend([1; 16]);
    wire.mark("enabled");
    wire.bytes.push(2);
    wire.bytes.push(1);
    wire.mark("object_id");
    wire.bytes.extend([2; 16]);
    wire.bytes.push(1);
    wire.text("check_watch");
    wire.bytes.push(1);
    wire.mark("oid");
    wire.bytes.extend(65536i64.to_le_bytes());
    wire.bytes.push(1);
    wire.mark("constraint_oid");
    wire.bytes.extend(65537i64.to_le_bytes());
    wire.mark("name");
    wire.text("watch");
    wire.mark("table");
    wire.text("s.t");
    wire.text("s.fire");
    wire.mark("constraint");
    wire.bytes.push(1);
    wire.bytes.push(1);
    wire.text("s.parent");
    wire.mark("deferrability");
    wire.bytes.push(2);
    wire.mark("row");
    wire.bytes.push(1);
    wire.mark("timing");
    wire.bytes.push(1);
    wire.mark("replace");
    wire.bytes.push(0);
    wire.mark("when");
    wire.bytes.push(1);
    wire.mark("arguments");
    wire.bytes.extend(2u64.to_le_bytes());
    wire.bytes.extend(2u64.to_le_bytes());
    wire.bytes.extend(1u64.to_le_bytes());
    wire.bytes.extend(1u64.to_le_bytes());
    wire.text("original\0argument");
    wire.text("");
    wire.mark("event");
    wire.bytes.extend([1, 2]);
    wire.text("Value.Name");
    wire.text("old.rows");
    wire.mark("transition_new");
    wire.bytes.push(0);
    wire.mark("transition_table");
    wire.bytes.push(1);
    wire.mark("condition");
    wire.bytes.extend(b"UQAEXP01\x01\0\0\0\0\0\0\0");
    wire.bytes.extend([18, 1, 4]); // IS NOT NULL, qualified column.
    wire.text("new");
    wire.text("Value.Name");
    wire
}
