//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

"use strict";

const { NotificationError, NotificationEvent, protocol, cancelled, timeout } = require("./notification-error.js");
const { subscriptionRequest, httpOptions, invalid } = require("./notification-request.js");
const { NotificationQueue } = require("./notification-queue.js");
const inspect = Symbol.for("nodejs.util.inspect.custom");

function exhausted(episode, last) {
  return new NotificationError(last.code, { originalFailure: episode.original,
    lastAttemptFailure: last, reconnectAttempts: episode.attempts });
}
function sleep(delay, signal) {
  return new Promise((resolve, reject) => {
    if (signal.aborted) { reject(cancelled()); return; }
    let timer;
    const abort = () => { clearTimeout(timer); reject(cancelled()); };
    signal.addEventListener("abort", abort, { once: true });
    timer = setTimeout(() => { signal.removeEventListener("abort", abort); resolve(); }, delay);
  });
}
function signalOption(signal) {
  if (signal == null) return null;
  if (typeof signal.aborted !== "boolean" || typeof signal.addEventListener !== "function" ||
      typeof signal.removeEventListener !== "function") throw invalid();
  return signal;
}

class SubscriptionState {
  #connection;
  #queue;
  #controller = new AbortController();
  #external;
  #abort;
  #resolveReady;
  #rejectReady;
  #waiter = null;
  #receiving = false;
  #ready = false;
  #terminal = false;
  #error = null;
  #closed = false;
  epoch = null;
  requestId = null;
  ready;
  done;
  constructor(connection, signal) {
    this.#connection = connection;
    this.#queue = new NotificationQueue(connection.options.maxQueuedEvents, connection.options.maxQueuedBytes);
    this.ready = new Promise((resolve, reject) => { this.#resolveReady = resolve; this.#rejectReady = reject; });
    this.#external = signal;
    this.#abort = () => { this.#queue.clear(); this.#controller.abort(); };
    signal?.addEventListener("abort", this.#abort, { once: true });
    if (signal?.aborted) this.#controller.abort();
    this.done = this.#run().then((error) => this.#finish(error), (error) => this.#finish(error));
  }
  #wake() { const wake = this.#waiter; this.#waiter = null; wake?.(); }
  #push(event) {
    if (this.#controller.signal.aborted) throw cancelled();
    this.#queue.push(event);
    this.#wake();
  }
  #finish(error) {
    if (!(error instanceof NotificationError)) error = new NotificationError("NOTIFICATION_SOURCE_UNAVAILABLE");
    this.#error = error;
    this.#terminal = true;
    if (this.#closed || ["NOTIFICATION_CANCELLED", "NOTIFICATION_AUTHENTICATION", "NOTIFICATION_AUTHORITY_REVOKED"].includes(error.code)) this.#queue.clear();
    if (!this.#ready) this.#rejectReady(error);
    this.#external?.removeEventListener("abort", this.#abort);
    this.#external = null;
    this.#connection = null;
    this.#resolveReady = this.#rejectReady = null;
    this.#wake();
  }
  async #run() {
    const connection = this.#connection;
    const { runAttempt, randomInt, now } = connection.runtime;
    const policy = connection.options.retry;
    const signal = this.#controller.signal;
    let previousEpoch = null;
    let episode = null;
    for (;;) {
      let ready = null;
      let failure;
      try {
        await runAttempt(connection, signal, episode?.deadline, (event) => {
          if (event.epoch === previousEpoch) throw protocol("identity");
          if (!this.#ready) {
            this.epoch = event.epoch;
            this.requestId = event.requestId;
            this.#ready = true;
            this.#resolveReady();
          } else this.#push(new NotificationEvent({ kind: "reconnected", epoch: event.epoch, requestId: event.requestId }));
          ready = event;
          episode = null;
        }, (event) => this.#push(event));
      } catch (error) { failure = error; }
      if (signal.aborted) return cancelled();
      if (policy === null || !failure.retryable) return episode === null ? failure : exhausted(episode, failure);
      if (ready !== null) {
        previousEpoch = ready.epoch;
        this.#push(new NotificationEvent({ kind: "resync_required", epoch: ready.epoch,
          requestId: ready.requestId, cause: failure.code }));
        episode = { original: failure, deadline: now() + policy.episodeTimeoutMs, attempts: 0 };
      }
      if (episode === null) return failure;
      if (episode.attempts === policy.maxAttempts) return exhausted(episode, failure);
      const upper = Math.min(policy.initialBackoffMs * 2 ** Math.min(episode.attempts, 31), policy.maxBackoffMs);
      let wait;
      try { wait = Math.max(randomInt(Math.ceil(upper / 2), upper + 1), failure.retryAfterMs ?? 0); }
      catch { return exhausted(episode, new NotificationError("NOTIFICATION_SOURCE_UNAVAILABLE")); }
      if (now() + wait >= episode.deadline) return exhausted(episode, timeout("reconnect"));
      await sleep(wait, signal);
      if (now() >= episode.deadline) return exhausted(episode, timeout("reconnect"));
      episode.attempts += 1;
    }
  }
  get isClosed() { return this.#closed || this.#terminal || this.#controller.signal.aborted; }
  async nextEvent() {
    if (this.#receiving) throw invalid();
    this.#receiving = true;
    try {
      for (;;) {
        if (this.#closed) { await this.done; return null; }
        if (this.#controller.signal.aborted && !this.#terminal) { await this.done; continue; }
        const event = this.#queue.take();
        if (event !== null) {
          if (event.kind === "reconnected") { this.epoch = event.epoch; this.requestId = event.requestId; }
          return event;
        }
        if (this.#terminal) { this.#queue.clear(); throw this.#error; }
        await new Promise((resolve) => { this.#waiter = resolve; });
      }
    } finally { this.#receiving = false; }
  }
  stop() { this.#closed = true; this.#queue.clear(); this.#controller.abort(); }
  async close() { this.stop(); await this.done; }
  [inspect]() { return "NotificationSubscriptionState(...)"; }
}

const finalizers = new FinalizationRegistry((state) => state.stop());
class HttpNotificationSubscription {
  #state;
  constructor(state) { this.#state = state; finalizers.register(this, state, this); }
  get epoch() { return this.#state.epoch; }
  get requestId() { return this.#state.requestId; }
  get isClosed() { return this.#state.isClosed; }
  async nextEvent() {
    try { return await this.#state.nextEvent(); }
    finally { void this.#state; } // A pending receive retains its facade owner.
  }
  async next() {
    const value = await this.nextEvent();
    return value === null ? { done: true, value: undefined } : { done: false, value };
  }
  [Symbol.asyncIterator]() { return this; }
  async close() { await this.#state.close(); finalizers.unregister(this); }
  async return() { await this.close(); return { done: true, value: undefined }; }
  async throw(error) { await this.close(); throw error; }
  [inspect]() { return "HttpNotificationSubscription(" + (this.isClosed ? "closed" : "open") + ")"; }
}

async function subscribe(origin, token, channels, sourceOptions, runtime) {
  const signal = signalOption(sourceOptions?.signal);
  if (signal?.aborted) throw cancelled();
  const options = httpOptions(sourceOptions);
  const request = subscriptionRequest(channels, options.maxChannels);
  const state = new SubscriptionState({ origin, token, request, options, runtime }, signal);
  await state.ready;
  if (signal?.aborted) { await state.close(); throw cancelled(); }
  return new HttpNotificationSubscription(state);
}

module.exports = { subscribe, HttpNotificationSubscription };
