//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

import { Engine, UQA } from "../../crates/uqa-wasm/js/index.mjs";
import { runStorageTransactions } from "../javascript/storage-transactions.mjs";

if (typeof document === "undefined") {
  console.log(JSON.stringify(await run()));
}

export async function run() {
  const path = `${UQA.persistDir}/storage-transactions.db`;
  return runStorageTransactions(
    (databasePath) => Engine.open(databasePath),
    path,
    async () => {
      if (typeof indexedDB !== "undefined") {
        await UQA.persist();
      }
    },
  );
}
