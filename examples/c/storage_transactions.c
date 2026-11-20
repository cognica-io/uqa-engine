/*
 * Unified Query Algebra
 *
 * Copyright (c) 2023-2026 Cognica, Inc.
 */

#include "example.h"
#include <uqa/uqa_backup.h>

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

static void restore_incremental(uqa_engine engine, example_database database) {
    char repository_path[4096], destination[4096];
    int written = snprintf(repository_path, sizeof(repository_path), "%s.backups", database.path);
    REQUIRE(written > 0 && (size_t)written < sizeof(repository_path));
    written = snprintf(destination, sizeof(destination), "%s.restored", database.path);
    REQUIRE(written > 0 && (size_t)written < sizeof(destination));

    uqa_backup_repository repository = NULL;
    uqa_backup_job capture = NULL;
    uqa_backup_id full, child;
    CHECK(uqa_backup_repository_create(repository_path, NULL, NULL, &repository));
    CHECK(uqa_backup_job_create(engine, repository, NULL, NULL, &capture));
    CHECK(uqa_backup_job_finish(capture, &full));
    CHECK(uqa_backup_job_destroy(&capture));
    example_execute(engine, "UPDATE accounts SET balance = 105 WHERE id = 1");
    CHECK(uqa_backup_job_create(engine, repository, &full, NULL, &capture));
    CHECK(uqa_backup_job_finish(capture, &child));
    CHECK(uqa_backup_job_destroy(&capture));
    CHECK(uqa_backup_repository_remove(repository, &full));
    CHECK(uqa_backup_repository_verify(repository, &child));

    uqa_restore_job restore = NULL;
    uqa_engine restored = NULL;
    CHECK(uqa_restore_job_create(engine, repository, &child, destination, database.flags,
                                 NULL, NULL, &restore));
    CHECK(uqa_restore_job_finish(&restore, &restored));
    CHECK(uqa_backup_repository_close(&repository));
    REQUIRE(balance(restored) == 105);
    CHECK(uqa_engine_close(&restored));
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
    restore_incremental(engine, database);
    CHECK(uqa_engine_close(&engine));
    puts("C storage and transactions example passed");
    return EXIT_SUCCESS;
}
