#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Async ownership over the same native subscription used by synchronous Python."""

from __future__ import annotations

import asyncio

from ._uqa import _invalid_notification as _invalid


def _complete(weak_future, event, error):
    """Settle on the owning loop without rooting an abandoned consumer task."""
    future = weak_future()
    if future is not None and not future.done():
        if error is None:
            future.set_result(event)
        else:
            future.set_exception(error)


async def _settle(future):
    """Finish owned work despite repeated cancellation of the awaiting task."""
    while not future.done():
        try:
            await asyncio.shield(future)
        except asyncio.CancelledError:
            continue
        except BaseException:
            break
    # Retrieve the result even on failure so no worker exception is abandoned.
    try:
        return future.result()
    except BaseException:
        return None


class AsyncNotificationSubscription:
    """One async consumer; cancellation waits for actual receive and close work."""

    def __init__(self, subscription):
        self._subscription = subscription
        self._pending = None
        self._close = None
        self._loop = asyncio.get_running_loop()

    @property
    def epoch(self):
        return self._subscription.epoch

    @property
    def request_id(self):
        return self._subscription.request_id

    @property
    def is_closed(self):
        return self._subscription.is_closed

    def __aiter__(self):
        return self

    async def __anext__(self):
        if asyncio.get_running_loop() is not self._loop or self._pending is not None:
            raise _invalid()
        if self._close is not None:
            raise StopAsyncIteration
        future = self._subscription._next_event_future(self._loop)
        self._pending = future
        try:
            event = await asyncio.shield(future)
        except GeneratorExit:
            # An abandoned coroutine cannot await a closed loop; the native
            # owner's drop path retains and completes provider cleanup.
            self._subscription._stop_delivery()
            raise
        except BaseException:
            self._subscription._stop_delivery()
            await _settle(future)
            await self._close_owned()
            raise
        finally:
            self._pending = None
        if event is None or self._close is not None:
            await self.aclose()
            raise StopAsyncIteration
        return event

    async def _close_owned(self):
        self._subscription._stop_delivery()
        if self._close is None:
            self._close = self._loop.run_in_executor(None, self._subscription.close)
        await _settle(self._close)

    async def aclose(self):
        if asyncio.get_running_loop() is not self._loop:
            raise _invalid()
        self._subscription._stop_delivery()
        if self._close is None:
            self._close = self._loop.run_in_executor(None, self._subscription.close)
        try:
            await asyncio.shield(self._close)
            if self._pending is not None:
                await _settle(self._pending)
        except asyncio.CancelledError:
            await _settle(self._close)
            if self._pending is not None:
                await _settle(self._pending)
            raise

    async def __aenter__(self):
        return self

    async def __aexit__(self, exc_type, exc, traceback):
        await self.aclose()
        return False

    def __repr__(self):
        return "AsyncNotificationSubscription(...)"


class _AsyncRegistration:
    def __init__(self, registration):
        self._registration = registration
        self._started = False
        self._subscription = None

    async def _start(self):
        if self._started:
            raise _invalid()
        self._started = True
        loop = asyncio.get_running_loop()
        future = loop.run_in_executor(None, self._registration.run)
        try:
            subscription = await asyncio.shield(future)
        except asyncio.CancelledError:
            self._registration.cancel()
            subscription = await _settle(future)
            if subscription is not None:
                subscription._stop_delivery()
                await _settle(loop.run_in_executor(None, subscription.close))
            raise
        self._subscription = AsyncNotificationSubscription(subscription)
        return self._subscription

    def __await__(self):
        return self._start().__await__()

    async def __aenter__(self):
        return await self._start()

    async def __aexit__(self, exc_type, exc, traceback):
        if self._subscription is not None:
            await self._subscription.aclose()
        return False
