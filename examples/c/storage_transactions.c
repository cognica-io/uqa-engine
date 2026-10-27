/*
 * Unified Query Algebra
 *
 * Copyright (c) 2023-2026 Cognica, Inc.
 */

#include "example.h"

static int64_t balance(uqa_engine engine) {
    uqa_value owner = NULL;
    uqa_result result = NULL;
    CHECK(uqa_value_create_text("alice", &owner));
    uqa_value_view parameters[] = {owner};
    CHECK(uqa_result_create(&result));
    CHECK(uqa_engine_execute_params(engine,
        "SELECT balance FROM accounts WHERE owner = $1", parameters, 1, result));
    CHECK(uqa_value_destroy(&owner));
    REQUIRE(example_rows(result) == 1);
    int64_t value = example_integer(result, 0, 0);
    CHECK(uqa_result_destroy(&result));
    return value;
}

int main(int argc, char **argv) {
    example_database database = example_arguments(argc, argv, 1);
    uqa_engine engine = example_open(database);
    example_execute(engine,
        "CREATE TABLE accounts (id INTEGER PRIMARY KEY, owner TEXT, balance INTEGER);"
        "INSERT INTO accounts VALUES (1, 'alice', 100), (2, 'bob', 50)");
    CHECK(uqa_engine_close(&engine));
    engine = example_open(database);
    REQUIRE(example_one_integer(engine, "SELECT count(*) FROM accounts") == 2);
    example_execute(engine, "BEGIN; UPDATE accounts SET balance = 0");
    REQUIRE(balance(engine) == 0);
    example_execute(engine, "ROLLBACK");
    REQUIRE(balance(engine) == 100);
    example_execute(engine,
        "BEGIN; UPDATE accounts SET balance = balance - 10 WHERE owner = 'alice';"
        "SAVEPOINT after_debit;"
        "UPDATE accounts SET balance = balance - 90 WHERE owner = 'alice'");
    REQUIRE(balance(engine) == 0);
    example_execute(engine, "ROLLBACK TO SAVEPOINT after_debit");
    REQUIRE(balance(engine) == 90);
    example_execute(engine, "RELEASE SAVEPOINT after_debit; COMMIT");
    uqa_engine session = NULL;
    CHECK(uqa_engine_open_session(engine, &session));
    REQUIRE(balance(session) == 90);
    CHECK(uqa_engine_close(&session));
    CHECK(uqa_engine_close(&engine));
    puts("C storage and transactions example passed");
    return EXIT_SUCCESS;
}
