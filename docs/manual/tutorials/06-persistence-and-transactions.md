# Tutorial 6: Persistence and Transactions

This tutorial makes transaction boundaries explicit, uses savepoints, reopens durable state, and creates independent sessions over one provider.

## 1. Create an account database

```sh
cargo run -p uqa-cli --bin usql -- --db accounts.uqa
```

```sql
CREATE TABLE accounts (
    account_id INTEGER PRIMARY KEY,
    owner TEXT NOT NULL,
    balance NUMERIC(18, 2) NOT NULL CHECK (balance >= 0)
);

INSERT INTO accounts (account_id, owner, balance) VALUES
    (1, 'alice', 100.00),
    (2, 'bob', 50.00);
```

## 2. Transfer money atomically

```sql
BEGIN;

UPDATE accounts
SET balance = balance - 25.00
WHERE account_id = 1;

UPDATE accounts
SET balance = balance + 25.00
WHERE account_id = 2;

COMMIT;
```

Verify both sides:

```sql
SELECT account_id, owner, balance
FROM accounts
ORDER BY account_id;
```

The business invariant spans two rows, so both updates belong to one transaction.

## 3. Recover part of a transaction with a savepoint

```sql
BEGIN;

UPDATE accounts
SET balance = balance - 10.00
WHERE account_id = 1;

SAVEPOINT optional_bonus;

UPDATE accounts
SET balance = balance + 1000.00
WHERE account_id = 2;

ROLLBACK TO SAVEPOINT optional_bonus;
RELEASE SAVEPOINT optional_bonus;

UPDATE accounts
SET balance = balance + 10.00
WHERE account_id = 2;

COMMIT;
```

The savepoint rollback discards the optional update while retaining the earlier debit and later credit.

## 4. Use an atomic Rust SQL batch

```rust
use std::path::Path;
use uqa_core::Value;
use uqa_engine::{Engine, SQLParam};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let engine = Engine::open(Path::new("accounts.uqa"))?;
    let debit = [
        SQLParam::scalar(Value::Int(5)),
        SQLParam::scalar(Value::Int(1)),
    ];
    let credit = [
        SQLParam::scalar(Value::Int(5)),
        SQLParam::scalar(Value::Int(2)),
    ];
    let statements = [
        (
            "UPDATE accounts SET balance = balance - $1 WHERE account_id = $2",
            &debit[..],
        ),
        (
            "UPDATE accounts SET balance = balance + $1 WHERE account_id = $2",
            &credit[..],
        ),
    ];
    engine.sql_batch(&statements)?;
    Ok(())
}
```

`sql_batch` commits all statements or rolls them all back. Use `Engine::transaction` when Rust control flow or typed engine calls must participate in the boundary.

## 5. Create independent sessions

```rust
use std::path::Path;
use uqa_engine::Engine;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let admin = Engine::open(Path::new("accounts.uqa"))?;
    let reader = admin.new_session()?;

    admin.sql("SET search_path TO public", &[])?;
    let result = reader.sql(
        "SELECT account_id, balance FROM accounts ORDER BY account_id",
        &[],
    )?;
    println!("{result:?}");
    Ok(())
}
```

Sessions share durable catalog and row state but have independent transactions, prepared statements, variables, caches, and cancellation tokens. A transaction opened by one session does not become the transaction context of another.

## 6. Reopen committed state

Exit the shell, open the same path again, and query the balances. Committed changes remain, while rolled-back work does not appear.

```sh
cargo run -p uqa-cli --bin usql -- --db accounts.uqa -c "SELECT account_id, balance FROM accounts ORDER BY account_id"
```

## 7. Select an encrypted open path

For a new SQLCipher database:

```sh
cargo run -p uqa-cli --bin usql -- --db secure-accounts.uqa --key-file ./database.key
```

Protect the key file with operating-system permissions and a secret lifecycle independent of the database. Do not copy live files as a backup; close writers or use a provider-consistent backup procedure.

Read [Storage and security](../reference/04-storage-and-security.md) before choosing encryption or compressed containers.
