//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { join } from "node:path";

const require = createRequire(import.meta.url);
const { Engine, createBackupRepository } = require("../../crates/uqa-node");

async function finish(jobPromise) {
  const job = await jobPromise;
  try {
    return await job.finish();
  } finally {
    await job.close();
  }
}

const directory = await mkdtemp(join(tmpdir(), "uqa-backup-"));
const source = new Engine();
try {
  const repository = await createBackupRepository(join(directory, "backups"));
  let restored;
  try {
    await source.sql("CREATE TABLE accounts (id INTEGER PRIMARY KEY, balance INTEGER)");
    await source.sql("INSERT INTO accounts VALUES (1, 90)");
    const full = await finish(source.beginBackup(repository));
    await source.sql("UPDATE accounts SET balance = 105 WHERE id = 1");
    const child = await finish(source.beginBackup(repository, { parent: full.id }));
    await repository.remove(full.id);
    await repository.verify(child.id);
    restored = await finish(source.beginRestore(repository, child.id, { provider: "memory" }));
  } finally {
    await repository.close();
  }
  try {
    const result = await restored.sql("SELECT balance FROM accounts");
    assert.equal(Number(result.rows[0].balance), 105);
  } finally {
    restored.close();
  }
} finally {
  source.close();
  await rm(directory, { recursive: true, force: true });
}
