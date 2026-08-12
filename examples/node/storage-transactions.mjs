//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createRequire } from "node:module";

import { runStorageTransactions } from "../javascript/storage-transactions.mjs";

const require = createRequire(import.meta.url);
const uqa = require("../../crates/uqa-node");
const directory = mkdtempSync(join(tmpdir(), "uqa-node-storage-"));
const path = join(directory, "accounts.db");

try {
  const results = await runStorageTransactions(
    (databasePath) => uqa.open(databasePath),
    path,
    async () => {},
  );
  console.log(JSON.stringify(results));
} finally {
  rmSync(directory, { recursive: true, force: true });
}
