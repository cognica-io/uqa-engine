#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Exercise native HttpEngine exception attributes through actual HTTP responses."""

import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import pytest
import uqa


@pytest.fixture
def limit_origin():
    state = {"limit": None, "code": "TOO_MANY_STATEMENTS", "requests": 0}

    class Handler(BaseHTTPRequestHandler):
        def do_POST(self):
            assert self.headers["Authorization"] == "Bearer limit-test"
            json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            state["requests"] += 1
            payload = {"error": {"code": state["code"], "message": "private server query",
                                 "limit": state["limit"]}, "request_id": "limit-request"}
            body = json.dumps(payload).encode()
            self.send_response(413)
            self.send_header("Content-Type", "application/json")
            self.send_header("X-Request-Id", "limit-request")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *_args):
            pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever)
    thread.start()
    try:
        yield state, uqa.HttpEngine(f"http://127.0.0.1:{server.server_port}", "limit-test")
    finally:
        server.shutdown()
        thread.join()
        server.server_close()


@pytest.mark.parametrize("actual", [200, 400])
def test_batch_limit_retains_numeric_evidence_and_request_identity(limit_origin, actual):
    state, engine = limit_origin
    expected = {"kind": "batch_statements", "limit": 128, "actual": actual,
                "actual_is_lower_bound": False}
    state["limit"] = expected
    with pytest.raises(uqa.HttpEngineError) as raised:
        engine.sql_batch([("SELECT 1", [])] * actual)
    error = raised.value
    assert error.limit == expected
    assert error.status == 413 and error.code == "TOO_MANY_STATEMENTS"
    assert error.request_id == "limit-request"
    assert f"batch_statements limit 128; observed {actual}" in str(error)
    assert "private server query" not in repr(error)
    assert state["requests"] == 1


def test_body_lower_bound_is_not_presented_as_an_exact_size(limit_origin):
    state, engine = limit_origin
    state["code"] = "REQUEST_BODY_TOO_LARGE"
    state["limit"] = {"kind": "request_body_bytes", "limit": 128, "actual": 129,
                      "actual_is_lower_bound": True}
    with pytest.raises(uqa.HttpEngineError) as raised:
        engine.sql("SELECT 1")
    assert raised.value.limit == state["limit"]
    assert "observed at least 129" in str(raised.value)
    assert state["requests"] == 1


@pytest.mark.parametrize("limit", [None, [],
    {"kind": "batch_statements", "limit": 128, "actual": 200, "actual_is_lower_bound": False,
     "hint": "private server query"},
    {"kind": "batch_statements", "limit": 128, "actual": 200, "actual_is_lower_bound": True},
    {"kind": "batch_statements", "limit": 128, "actual": 9007199254740992, "actual_is_lower_bound": False},
    {"kind": "sql_bytes", "limit": 128, "actual": 200, "actual_is_lower_bound": False},
])
def test_malformed_optional_limit_preserves_the_original_error(limit_origin, limit):
    state, engine = limit_origin
    state["limit"] = limit
    with pytest.raises(uqa.HttpEngineError) as raised:
        engine.sql_batch([("SELECT 1", [])])
    error = raised.value
    assert error.limit is None and error.diagnostic is None
    assert error.code == "TOO_MANY_STATEMENTS" and error.status == 413
    assert error.request_id == "limit-request"
    assert "private server query" not in repr(error)
    assert "observed" not in str(error)
    assert state["requests"] == 1
