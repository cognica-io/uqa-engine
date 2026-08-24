package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"runtime/debug"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgconn"
	"github.com/jackc/pgx/v5/pgxpool"
)

func requireSQLState(err error, expected string) {
	var pgErr *pgconn.PgError
	if !errors.As(err, &pgErr) || pgErr.Code != expected {
		panic(fmt.Sprintf("expected SQLSTATE %s, got %T: %v", expected, err, err))
	}
}

func pgxVersion() string {
	info, ok := debug.ReadBuildInfo()
	if !ok {
		return "unknown"
	}
	for _, dependency := range info.Deps {
		if dependency.Path == "github.com/jackc/pgx/v5" {
			return dependency.Version
		}
	}
	return "unknown"
}

func main() {
	ctx := context.Background()
	config, err := pgxpool.ParseConfig(os.Getenv("UQA_PG18_MATRIX_DSN"))
	if err != nil {
		panic(err)
	}
	config.MaxConns = 1
	config.MinConns = 1
	pool, err := pgxpool.NewWithConfig(ctx, config)
	if err != nil {
		panic(err)
	}
	defer pool.Close()

	connection, err := pool.Acquire(ctx)
	if err != nil {
		panic(err)
	}
	released := false
	defer func() {
		if !released {
			connection.Release()
		}
	}()
	conn := connection.Conn()
	if _, err = conn.Prepare(ctx, "matrix-add", "SELECT $1::int4 + 1 AS value"); err != nil {
		panic(err)
	}
	for input, expected := range map[int32]int32{41: 42, 99: 100} {
		var actual int32
		if err = conn.QueryRow(ctx, "matrix-add", input).Scan(&actual); err != nil {
			panic(err)
		}
		if actual != expected {
			panic(fmt.Sprintf("prepared result %d, expected %d", actual, expected))
		}
	}

	if _, err = conn.Exec(ctx, "BEGIN"); err != nil {
		panic(err)
	}
	var ignored int
	err = conn.QueryRow(ctx, "SELECT 1 / 0").Scan(&ignored)
	requireSQLState(err, "22012")
	err = conn.QueryRow(ctx, "SELECT 1").Scan(&ignored)
	requireSQLState(err, "25P02")
	if _, err = conn.Exec(ctx, "ROLLBACK"); err != nil {
		panic(err)
	}

	if _, err = conn.Exec(ctx, "CREATE TEMP TABLE matrix_copy (id int4, value text)"); err != nil {
		panic(err)
	}
	rows := [][]any{{int32(1), "one"}, {int32(2), "two"}}
	count, err := conn.CopyFrom(ctx, pgx.Identifier{"matrix_copy"}, []string{"id", "value"}, pgx.CopyFromRows(rows))
	if err != nil || count != 2 {
		panic(fmt.Sprintf("COPY FROM count=%d err=%v", count, err))
	}
	var copiedCount int64
	if err = conn.QueryRow(ctx, "SELECT count(*)::int8 FROM matrix_copy").Scan(&copiedCount); err != nil || copiedCount != 2 {
		panic(fmt.Sprintf("copied row count=%d err=%v", copiedCount, err))
	}
	var output bytes.Buffer
	if _, err = conn.PgConn().CopyTo(ctx, &output, "COPY matrix_copy TO STDOUT"); err != nil {
		panic(err)
	}
	if output.String() != "1\tone\n2\ttwo\n" {
		panic(fmt.Sprintf("unexpected COPY output %q", output.String()))
	}

	connection.Release()
	released = true
	var one int
	if err = pool.QueryRow(ctx, "SELECT 1").Scan(&one); err != nil || one != 1 {
		panic(fmt.Sprintf("pooled query result=%d err=%v", one, err))
	}

	evidence := map[string]any{
		"driver": "pgx",
		"pgx":    pgxVersion(),
		"operations": []string{
			"binary-bind-result",
			"prepared-reuse",
			"copy-in-out",
			"transaction-error-recovery",
			"pool-reuse",
		},
	}
	if err = json.NewEncoder(os.Stdout).Encode(evidence); err != nil {
		panic(err)
	}
}
