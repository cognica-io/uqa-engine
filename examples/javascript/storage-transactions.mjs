//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

import { assertEqual } from "./common.mjs";

export async function runStorageTransactions(open, path, persist) {
  const first = await open(path);
  try {
    await first.sql("DROP TABLE IF EXISTS accounts");
    await first.sql(
      "CREATE TABLE accounts (id INTEGER PRIMARY KEY, owner TEXT, balance INTEGER)",
    );
    await first.sql(
      "INSERT INTO accounts (id, owner, balance) VALUES (1, 'alice', 100), (2, 'bob', 50)",
    );
  } finally {
    await first.close();
  }
  await persist();

  const engine = await open(path);
  try {
    const reopenedCount = await count(engine);
    await engine.sql("BEGIN");
    await engine.sql("UPDATE accounts SET balance = 0");
    const insideRollback = await balance(engine, "alice");
    await engine.sql("ROLLBACK");
    const afterRollback = await balance(engine, "alice");

    await engine.sql("BEGIN");
    await engine.sql("UPDATE accounts SET balance = balance - 10 WHERE owner = 'alice'");
    await engine.sql("SAVEPOINT after_debit");
    await engine.sql("UPDATE accounts SET balance = balance - 90 WHERE owner = 'alice'");
    const beforeSavepointRollback = await balance(engine, "alice");
    await engine.sql("ROLLBACK TO SAVEPOINT after_debit");
    const afterSavepointRollback = await balance(engine, "alice");
    await engine.sql("RELEASE SAVEPOINT after_debit");
    await engine.sql("COMMIT");

    const session = await engine.newSession();
    let sessionBalance;
    try {
      sessionBalance = await balance(session, "alice");
    } finally {
      await session.close();
    }
    const results = {
      reopenedCount,
      insideRollback,
      afterRollback,
      beforeSavepointRollback,
      afterSavepointRollback,
      sessionBalance,
    };
    assertEqual(
      results,
      {
        reopenedCount: 2,
        insideRollback: 0,
        afterRollback: 100,
        beforeSavepointRollback: 0,
        afterSavepointRollback: 90,
        sessionBalance: 90,
      },
      "storage transaction scenario",
    );
    return results;
  } finally {
    await engine.close();
  }
}

async function count(engine) {
  return (await engine.sql("SELECT COUNT(*) AS n FROM accounts")).rows[0].n;
}

async function balance(engine, owner) {
  return (await engine.sql("SELECT balance FROM accounts WHERE owner = $1", [owner])).rows[0]
    .balance;
}
