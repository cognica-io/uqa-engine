#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Actual wheel notification ownership, GIL/asyncio behavior and shared SSE fixtures."""

import asyncio
from concurrent.futures import ThreadPoolExecutor
import gc
import json
from pathlib import Path
import queue
import socket
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import threading
import time
import weakref

import pytest
import uqa


def direct_options(**changes):
    values = dict(max_active_subscriptions=4, max_channels=2,
                  max_queued_notifications=8, max_queued_bytes=65536,
                  max_registry_entries_per_poll=2)
    values.update(changes)
    return uqa.NotificationSubscriptionOptions(**values)


def http_options(**changes):
    values = dict(max_channels=2, max_queued_events=8, max_queued_bytes=65536,
                  max_transport_chunk_bytes=65536, connect_timeout_ms=1000,
                  ready_timeout_ms=5000, max_idle_timeout_ms=10000)
    values.update(changes)
    return uqa.HttpNotificationOptions(**values)


def test_direct_transaction_boundaries_and_independent_contexts():
    engine = uqa.Engine()
    engine.sql("NOTIFY jobs, 'before registration'")
    with engine.subscribe_notifications(["jobs", "작업"], options=direct_options()) as first:
        engine.sql("BEGIN; NOTIFY jobs, 'one'; NOTIFY jobs, 'one'; SAVEPOINT discarded; NOTIFY jobs, 'discarded'; ROLLBACK TO discarded")
        with engine.subscribe_notifications(["jobs"], options=direct_options()) as second:
            engine.sql('NOTIFY "작업", \'문자😀\'; COMMIT')
            event = next(first)
            assert (event.kind, event.sequence, event.channel, event.payload) == ("notification", 1, "jobs", "one")
            assert event.epoch == first.epoch
            assert event.request_id is None and first.request_id is None
            assert type(event.sequence) is int and type(event.process_id) is int
            assert '"one"' not in repr(event) and "jobs" not in repr(event)
            event = next(first)
            assert (event.sequence, event.channel, event.payload) == (2, "작업", "문자😀")
            assert next(second).payload == "one"
            assert second.epoch != first.epoch
        engine.sql("NOTIFY jobs, 'independent'")
        assert next(first).payload == "independent"
    assert first.is_closed and first.next_event() is None
    first.close()
    assert engine.sql("SELECT 1 AS n").rows == [{"n": 1}]
    engine.close()


def test_direct_wait_releases_gil_and_close_wakes_the_owned_receiver():
    engine = uqa.Engine()
    subscription = engine.subscribe_notifications(["jobs"], options=direct_options())
    started = threading.Event()
    def receive():
        started.set()
        return subscription.next_event()
    with ThreadPoolExecutor(max_workers=1) as executor:
        waiting = executor.submit(receive)
        assert started.wait(5)
        assert engine.sql("SELECT 2 AS n").rows == [{"n": 2}]
        subscription.close()
        assert waiting.result(timeout=5) is None
    engine.close()


@pytest.mark.parametrize("mode", ["plain", "encrypted", "compressed", "compressed_encrypted"])
def test_persistent_subscription_survives_its_creating_engine(tmp_path, mode):
    path = tmp_path / "owned.db"
    if mode == "plain":
        engine = uqa.open(path)
    elif mode == "encrypted":
        engine = uqa.open_encrypted(path, "private-key")
    elif mode == "compressed":
        engine = uqa.open_compressed(path)
    else:
        engine = uqa.open_compressed_encrypted(path, "private-key")
    sender = engine.new_session()
    subscription = engine.subscribe_notifications(["jobs"], options=direct_options())
    engine.close()
    sender.sql("NOTIFY jobs, 'retained source'")
    assert next(subscription).payload == "retained source"
    subscription.close()
    sender.close()


@pytest.mark.parametrize("channels", [[], ["jobs", "jobs"], [""], ["x" * 64], ["a\0b"], ["\ud800"], "jobs", [7], [b"jobs"]])
def test_invalid_direct_input_does_not_retain_partial_admission(channels):
    engine = uqa.Engine()
    options = direct_options(max_active_subscriptions=1)
    with pytest.raises(uqa.NotificationError) as failure:
        engine.subscribe_notifications(channels, options=options)
    assert failure.value.code == "NOTIFICATION_INVALID_REQUEST"
    with engine.subscribe_notifications(["jobs"], options=options):
        pass
    engine.close()


def test_backpressure_is_typed_redacted_and_releases_capacity_on_close():
    engine = uqa.Engine()
    options = direct_options(max_active_subscriptions=1, max_queued_notifications=1)
    subscription = engine.subscribe_notifications(["jobs"], options=options)
    engine.sql("NOTIFY jobs, 'first private payload'")
    engine.sql("NOTIFY jobs, 'second private payload'")
    with pytest.raises(uqa.NotificationError) as failure:
        subscription.next_event()
    assert failure.value.code == "NOTIFICATION_BACKPRESSURE"
    assert failure.value.failure.code == failure.value.code
    assert "private" not in repr(failure.value) + repr(failure.value.failure)
    assert subscription.is_closed
    subscription.close()
    with engine.subscribe_notifications(["jobs"], options=options):
        pass
    engine.close()


def test_gc_cleanup_releases_the_original_admission():
    engine = uqa.Engine()
    options = direct_options(max_active_subscriptions=1)
    subscription = engine.subscribe_notifications(["jobs"], options=options)
    del subscription
    gc.collect()
    deadline = time.monotonic() + 5
    while True:
        try:
            replacement = engine.subscribe_notifications(["jobs"], options=options)
            break
        except uqa.NotificationError as error:
            assert error.code == "NOTIFICATION_CAPACITY"
            assert time.monotonic() < deadline
            time.sleep(0.001)
    replacement.close()
    engine.close()


def test_async_context_iteration_and_cancellation_join_a_single_worker():
    async def scenario():
        loop = asyncio.get_running_loop()
        loop.set_default_executor(ThreadPoolExecutor(max_workers=1))
        engine = uqa.Engine()
        options = direct_options(max_active_subscriptions=1)
        async with engine.subscribe_notifications_async(["jobs"], options=options) as subscription:
            waiting = asyncio.create_task(subscription.__anext__())
            await asyncio.sleep(0)
            engine.sql("NOTIFY jobs, 'async'")
            event = await asyncio.wait_for(waiting, 5)
            assert (event.kind, event.sequence, event.payload) == ("notification", 1, "async")
            waiting = asyncio.create_task(subscription.__anext__())
            await asyncio.sleep(0)
            waiting.cancel()
            with pytest.raises(asyncio.CancelledError):
                await asyncio.wait_for(waiting, 5)
            assert subscription.is_closed
        # Cancellation completion includes releasing the original admission.
        async with engine.subscribe_notifications_async(["jobs"], options=options):
            pass
        engine.close()
    asyncio.run(scenario())


def test_unstarted_async_registration_already_reserves_shared_capacity():
    async def scenario():
        engine = uqa.Engine()
        options = direct_options(max_active_subscriptions=1)
        pending = engine.subscribe_notifications_async(["jobs"], options=options)
        try:
            with pytest.raises(uqa.NotificationError) as failure:
                with engine.subscribe_notifications(["other"], options=direct_options(max_active_subscriptions=64)):
                    pass
            assert failure.value.code == "NOTIFICATION_CAPACITY"
            subscription = await pending
            await subscription.aclose()
            async with engine.subscribe_notifications_async(["jobs"], options=options):
                pass
        finally:
            engine.close()
    asyncio.run(scenario())


def test_async_await_and_explicit_close_join_pending_receive():
    async def scenario():
        engine = uqa.Engine()
        subscription = await engine.subscribe_notifications_async(["jobs"], options=direct_options())
        pending = asyncio.create_task(subscription.__anext__())
        await asyncio.sleep(0)
        with pytest.raises(uqa.NotificationError) as failure:
            await subscription.__anext__()
        assert failure.value.code == "NOTIFICATION_INVALID_REQUEST"
        assert failure.value.failure.code == failure.value.code
        await asyncio.wait_for(subscription.aclose(), 5)
        with pytest.raises(StopAsyncIteration):
            await pending
        await subscription.aclose()
        engine.close()
    asyncio.run(scenario())


def test_idle_async_receiver_does_not_starve_another_registration_cancellation():
    async def scenario():
        loop = asyncio.get_running_loop()
        loop.set_default_executor(ThreadPoolExecutor(max_workers=1))
        engine = uqa.Engine()
        first = await engine.subscribe_notifications_async(["jobs"], options=direct_options())
        receiving = asyncio.create_task(first.__anext__())
        await asyncio.sleep(0)
        opening = asyncio.ensure_future(engine.subscribe_notifications_async(["jobs"], options=direct_options()))
        await asyncio.sleep(0)
        opening.cancel()
        completed, _ = await asyncio.wait({opening}, timeout=5)
        await first.aclose()
        with pytest.raises(StopAsyncIteration):
            await receiving
        with pytest.raises(asyncio.CancelledError):
            await opening
        engine.close()
        assert completed, "an idle receive must not occupy registration/cleanup execution capacity"
    asyncio.run(scenario())


def test_repeated_receive_cancellation_joins_cleanup_before_releasing_admission():
    async def scenario():
        loop = asyncio.get_running_loop()
        loop.set_default_executor(ThreadPoolExecutor(max_workers=1))
        engine = uqa.Engine()
        options = direct_options(max_active_subscriptions=1)
        subscription = await engine.subscribe_notifications_async(["jobs"], options=options)
        started, release = threading.Event(), threading.Event()
        def occupy_worker():
            started.set()
            assert release.wait(5)
        worker = loop.run_in_executor(None, occupy_worker)
        try:
            while not started.is_set():
                await asyncio.sleep(0)
            waiting = asyncio.create_task(subscription.__anext__())
            await asyncio.sleep(0)
            waiting.cancel()
            await asyncio.sleep(0)
            waiting.cancel()
            await asyncio.sleep(0)
            assert subscription.is_closed
            assert not waiting.done(), "cancellation must wait for actual retained cleanup"
            with pytest.raises(uqa.NotificationError) as failure:
                engine.subscribe_notifications(["jobs"], options=options)
            assert failure.value.code == "NOTIFICATION_CAPACITY"
        finally:
            release.set()
        await worker
        with pytest.raises(asyncio.CancelledError):
            await asyncio.wait_for(waiting, 5)
        with engine.subscribe_notifications(["jobs"], options=options):
            pass
        engine.close()
    asyncio.run(scenario())


def test_abandoned_async_receiver_does_not_root_a_closed_loop_or_listener():
    engine = uqa.Engine()
    options = direct_options(max_active_subscriptions=1)
    loop = asyncio.new_event_loop()
    loop.set_exception_handler(lambda loop, context: None)
    async def abandon():
        subscription = await engine.subscribe_notifications_async(["jobs"], options=options)
        waiting = asyncio.create_task(subscription.__anext__())
        await asyncio.sleep(0)
        return weakref.ref(waiting)
    waiting = loop.run_until_complete(abandon())
    if hasattr(loop, "shutdown_default_executor"):
        loop.run_until_complete(loop.shutdown_default_executor())
    loop.close()
    del loop
    gc.collect()
    assert waiting() is None
    deadline = time.monotonic() + 5
    while True:
        try:
            replacement = engine.subscribe_notifications(["jobs"], options=options)
            break
        except uqa.NotificationError as error:
            assert error.code == "NOTIFICATION_CAPACITY"
            assert time.monotonic() < deadline
            time.sleep(0.001)
    replacement.close()
    engine.close()


FIXTURE = json.loads((Path(__file__).resolve().parents[2] / "crates/uqa-client/tests/fixtures/notifications-v1.json").read_text())


class Peer:
    def __init__(self, connection, number):
        self.connection = connection
        self.request_id = f"request_{number}"
        self.epoch = FIXTURE["stream_id"][:-2] + f"{number:02d}"
        self.frames = queue.Queue()
        self.closed = threading.Event()

    def send(self, data):
        self.frames.put(data.encode() if isinstance(data, str) else data)

    def disconnect(self):
        self.frames.put(None)


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def do_POST(self):
        peer = Peer(self.connection, len(self.server.connections) + 1)
        self.server.connections.append(peer)
        try:
            assert self.path == "/v1/notifications/subscribe"
            assert self.headers["Authorization"] == "Bearer PRIVATE_NOTIFICATION_TOKEN"
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            assert body == json.loads(FIXTURE["valid_request"])
            self.send_response(self.server.status)
            self.send_header("Content-Type", "text/event-stream; charset=utf-8")
            self.send_header("Cache-Control", "no-store, no-transform")
            self.send_header("X-Request-ID", peer.request_id)
            self.send_header("Transfer-Encoding", "chunked")
            self.end_headers()
            self.wfile.flush()
            self.server.requests.put(peer)
            self.connection.settimeout(0.01)
            while not self.server.stopping.is_set():
                try:
                    frame = peer.frames.get(timeout=0.01)
                except queue.Empty:
                    try:
                        if self.connection.recv(1, socket.MSG_PEEK) == b"":
                            break
                    except socket.timeout:
                        pass
                    continue
                if frame is None:
                    break
                self.wfile.write(f"{len(frame):x}\r\n".encode() + frame + b"\r\n")
                self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass
        except BaseException as error:
            self.server.errors.append(error)
        finally:
            self.close_connection = True
            peer.closed.set()


@pytest.fixture
def server():
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server.requests = queue.Queue()
    server.connections = []
    server.errors = []
    server.stopping = threading.Event()
    server.status = 200
    server.engine = uqa.HttpEngine(f"http://127.0.0.1:{server.server_port}", "PRIVATE_NOTIFICATION_TOKEN")
    thread = threading.Thread(target=lambda: server.serve_forever(poll_interval=0.01), daemon=True)
    thread.start()
    try:
        yield server
    finally:
        server.stopping.set()
        server.shutdown()
        thread.join(timeout=5)
        server.server_close()
        assert not server.errors


def ready(peer):
    peer.send(FIXTURE["ready"].replace('"idle_timeout_ms":"401"', '"idle_timeout_ms":"10000"')
              .replace(FIXTURE["request_id"], peer.request_id)
              .replace(FIXTURE["stream_id"], peer.epoch))


def test_http_ready_shared_fixture_fragmentation_and_transport_close(server):
    with ThreadPoolExecutor(max_workers=1) as executor:
        opening = executor.submit(server.engine.subscribe_notifications, FIXTURE["channels"], options=http_options())
        peer = server.requests.get(timeout=5)
        assert not opening.done(), "creation must wait for ready"
        ready(peer)
        with opening.result(timeout=5) as subscription:
            for byte in FIXTURE["notification"].encode():
                peer.send(bytes([byte]))
            event = next(subscription)
            assert (event.kind, event.epoch, event.request_id, event.sequence) == ("notification", FIXTURE["stream_id"], FIXTURE["request_id"], 1)
            assert (event.process_id, event.channel, event.payload) == (-2147483648, "작업", FIXTURE["expected_payload"])
            assert FIXTURE["expected_payload"] not in repr(event)
        assert peer.closed.wait(5), "close must release the actual response"


def test_http_async_registration_cancel_closes_actual_pending_response(server):
    async def scenario():
        opening = asyncio.ensure_future(server.engine.subscribe_notifications_async(FIXTURE["channels"], options=http_options()))
        peer = await asyncio.get_running_loop().run_in_executor(None, lambda: server.requests.get(timeout=5))
        opening.cancel()
        with pytest.raises(asyncio.CancelledError):
            await asyncio.wait_for(opening, 5)
        assert await asyncio.get_running_loop().run_in_executor(None, peer.closed.wait, 5)
    asyncio.run(scenario())


def test_http_async_receive_cancel_and_single_worker_cleanup(server):
    async def scenario():
        loop = asyncio.get_running_loop()
        loop.set_default_executor(ThreadPoolExecutor(max_workers=1))
        opening = asyncio.ensure_future(server.engine.subscribe_notifications_async(FIXTURE["channels"], options=http_options()))
        deadline = loop.time() + 5
        while server.requests.empty():
            if opening.done():
                await opening
            assert loop.time() < deadline
            await asyncio.sleep(0.001)
        peer = server.requests.get_nowait()
        ready(peer)
        subscription = await asyncio.wait_for(opening, 5)
        waiting = asyncio.create_task(subscription.__anext__())
        await asyncio.sleep(0)
        waiting.cancel()
        with pytest.raises(asyncio.CancelledError):
            await asyncio.wait_for(waiting, 5)
        assert peer.closed.wait(5)
        await subscription.aclose()
    asyncio.run(scenario())


def test_http_unsupported_endpoint_is_typed_without_sql_fallback(server):
    server.status = 404
    with pytest.raises(uqa.NotificationError) as failure:
        server.engine.subscribe_notifications(FIXTURE["channels"], options=http_options())
    assert failure.value.code == "NOTIFICATION_UNSUPPORTED"
    assert "PRIVATE_NOTIFICATION_TOKEN" not in repr(failure.value)
    assert len(server.connections) == 1


def test_http_gap_reconnection_identity_and_terminal_error(server):
    retry = uqa.NotificationRetryOptions(max_attempts=2, episode_timeout_ms=5000,
                                        initial_backoff_ms=1, max_backoff_ms=5,
                                        max_retry_after_ms=50)
    with ThreadPoolExecutor(max_workers=1) as executor:
        opening = executor.submit(server.engine.subscribe_notifications, FIXTURE["channels"], options=http_options(retry=retry))
        original = server.requests.get(timeout=5)
        ready(original)
        with opening.result(timeout=5) as subscription:
            original.disconnect()
            replacement = server.requests.get(timeout=5)
            # The first epoch remains visible until Reconnected is consumed.
            assert subscription.epoch == original.epoch
            ready(replacement)
            replacement.send(FIXTURE["notification"].replace(FIXTURE["request_id"], replacement.request_id)
                             .replace(FIXTURE["stream_id"], replacement.epoch))
            gap = next(subscription)
            assert (gap.kind, gap.epoch, gap.cause) == ("resync_required", original.epoch, "NOTIFICATION_TRANSPORT")
            assert gap.sequence is None and gap.payload is None
            resumed = next(subscription)
            assert (resumed.kind, resumed.epoch, resumed.request_id) == ("reconnected", replacement.epoch, replacement.request_id)
            assert (subscription.epoch, subscription.request_id) == (replacement.epoch, replacement.request_id)
            event = next(subscription)
            assert (event.kind, event.sequence, event.epoch) == ("notification", 1, replacement.epoch)
            replacement.send('event: error\ndata: ' + json.dumps(dict(request_id=replacement.request_id,
                             stream_id=replacement.epoch, code="NOTIFICATION_AUTHORITY_REVOKED", retryable=True)) + '\n\n')
            with pytest.raises(uqa.NotificationError) as failure:
                next(subscription)
            assert failure.value.code == "NOTIFICATION_AUTHORITY_REVOKED"
            assert subscription.is_closed
        assert replacement.closed.wait(5)
        assert len(server.connections) == 2


def test_http_async_explicit_close_ends_pending_iteration_normally(server):
    async def scenario():
        loop = asyncio.get_running_loop()
        opening = asyncio.ensure_future(server.engine.subscribe_notifications_async(FIXTURE["channels"], options=http_options()))
        peer = await loop.run_in_executor(None, lambda: server.requests.get(timeout=5))
        ready(peer)
        subscription = await asyncio.wait_for(opening, 5)
        waiting = asyncio.create_task(subscription.__anext__())
        await asyncio.sleep(0)
        await asyncio.wait_for(subscription.aclose(), 5)
        with pytest.raises(StopAsyncIteration):
            await waiting
        assert peer.closed.wait(5)
    asyncio.run(scenario())


def test_http_retry_exhaustion_keeps_original_and_last_failure(server):
    retry = uqa.NotificationRetryOptions(max_attempts=1, episode_timeout_ms=5000,
                                        initial_backoff_ms=1, max_backoff_ms=5,
                                        max_retry_after_ms=50)
    with ThreadPoolExecutor(max_workers=1) as executor:
        opening = executor.submit(server.engine.subscribe_notifications, FIXTURE["channels"], options=http_options(retry=retry))
        original = server.requests.get(timeout=5)
        ready(original)
        with opening.result(timeout=5) as subscription:
            original.disconnect()
            replacement = server.requests.get(timeout=5)
            replacement.disconnect()
            assert next(subscription).kind == "resync_required"
            with pytest.raises(uqa.NotificationError) as failure:
                next(subscription)
            retained = failure.value.failure
            assert retained.original_failure.code == "NOTIFICATION_TRANSPORT"
            assert retained.last_attempt_failure.code == "NOTIFICATION_TRANSPORT"
        assert replacement.closed.wait(5)
