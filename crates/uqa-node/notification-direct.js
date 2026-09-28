//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

"use strict";

const { NotificationError, NotificationEvent, knownKind } = require("./notification-error.js");
const { positive, invalid } = require("./notification-request.js");
const { encode, decode } = require("./notification-bytes.js");
const { validUnicode } = require("./http-json.js");
const inspect = Symbol.for("nodejs.util.inspect.custom");
const cancelled = () => new NotificationError("NOTIFICATION_CANCELLED");

function nativeFailure(error, handle) {
  if (error instanceof NotificationError) return error;
  const code = knownKind(error?.message) ? error.message : "NOTIFICATION_SOURCE_UNAVAILABLE";
  return new NotificationError(code, { diagnostic: handle?.diagnostic() ?? null });
}
function optionsAndChannels(channels, options) {
  if (options === null || typeof options !== "object") throw invalid();
  const limits = {};
  for (const field of ["maxActiveSubscriptions", "maxChannels", "maxQueuedNotifications", "maxQueuedBytes", "maxRegistryEntriesPerPoll"]) limits[field] = positive(options[field]);
  if (!Array.isArray(channels) || channels.length === 0 || channels.length > limits.maxChannels) throw invalid();
  const output = []; const seen = new Set();
  for (let index = 0, count = channels.length; index < count; index += 1) {
    const channel = channels[index];
    if (typeof channel !== "string" || channel.length === 0 || channel.length > 63 || !validUnicode(channel) || channel.includes("\0") || seen.has(channel)) throw invalid();
    const bytes = encode(channel);
    if (bytes.length > 63) throw invalid();
    const owned = decode(bytes);
    output.push(owned); seen.add(owned);
  }
  return { channels: output, limits };
}

class NativeOwner {
  #handle;
  #signal;
  #onAbort;
  #closing = null;
  #receiving = false;
  #terminal = null;
  #explicitClose = false;
  aborted = false;
  closed = false;
  constructor(handle, signal) {
    this.#handle = handle;
    this.#signal = signal;
    this.#onAbort = () => {
      this.aborted = true;
      this.#handle.stop();
      void this.close(false).catch(() => {});
    };
    signal?.addEventListener("abort", this.#onAbort, { once: true });
    if (signal?.aborted) this.#onAbort();
  }
  async ready() {
    try {
      const identity = await this.#handle.run();
      if (this.aborted) throw cancelled();
      return identity;
    } catch (error) {
      const failure = this.aborted ? cancelled() : nativeFailure(error, this.#handle);
      await this.close(false);
      throw failure;
    }
  }
  get isClosed() { return this.closed || this.#handle.isClosed; }
  async #end(explicit) {
    await this.close(explicit);
    if (this.#terminal) throw this.#terminal;
    return null;
  }
  async nextEvent() {
    if (this.#receiving) throw invalid();
    this.#receiving = true;
    try {
      if (this.#terminal) throw this.#terminal;
      if (this.#explicitClose) return await this.#end(true);
      if (this.aborted) { await this.close(false); throw cancelled(); }
      let raw;
      try { raw = await this.#handle.nextEvent(); }
      catch (error) {
        this.#terminal = nativeFailure(error, this.#handle);
        await this.close(false);
        throw this.#terminal;
      }
      if (this.aborted && !this.#explicitClose) { await this.close(false); throw cancelled(); }
      if (this.#explicitClose || raw == null) return await this.#end(this.#explicitClose);
      return new NotificationEvent(raw);
    } finally { this.#receiving = false; }
  }
  close(explicit = true) {
    this.#explicitClose ||= explicit;
    this.closed = true;
    this.#handle.stop();
    if (this.#closing === null) {
      let closing;
      try { closing = this.#handle.close(); }
      catch (error) { return Promise.reject(nativeFailure(error, this.#handle)); }
      this.#closing = closing.then(() => {
        if (this.#handle.failureCode !== null && this.#handle.failureCode !== undefined) {
          this.#terminal ??= new NotificationError(this.#handle.failureCode, { diagnostic: this.#handle.diagnostic() });
        }
      }).catch((error) => {
        this.#closing = null;
        throw nativeFailure(error, this.#handle);
      }).finally(() => {
        this.#signal?.removeEventListener("abort", this.#onAbort);
        this.#signal = null;
      });
    }
    return this.#closing;
  }
  [inspect]() { return "NativeNotificationOwner(...)"; }
}

const finalizers = new FinalizationRegistry((owner) => { void owner.close().catch(() => {}); });
class NotificationSubscription {
  #owner;
  #epoch;
  #requestId;
  constructor(owner, identity) {
    this.#owner = owner;
    this.#epoch = identity.epoch;
    this.#requestId = identity.requestId ?? null;
    finalizers.register(this, owner, this);
  }
  get epoch() { return this.#epoch; }
  get requestId() { return this.#requestId; }
  get isClosed() { return this.#owner.isClosed; }
  async nextEvent() {
    const event = await this.#owner.nextEvent();
    if (event?.kind === "reconnected") { this.#epoch = event.epoch; this.#requestId = event.requestId; }
    return event;
  }
  async next() { const value = await this.nextEvent(); return value === null ? { done: true, value: undefined } : { done: false, value }; }
  [Symbol.asyncIterator]() { return this; }
  async close() { await this.#owner.close(); finalizers.unregister(this); }
  async return() { await this.close(); return { done: true, value: undefined }; }
  async throw(error) { await this.close(); throw error; }
  [inspect]() { return "NotificationSubscription(" + (this.isClosed ? "closed" : "open") + ")"; }
}

async function subscribeDirect(channels, options, createHandle) {
  const signal = options?.signal;
  if (signal != null && (typeof signal.aborted !== "boolean" || typeof signal.addEventListener !== "function" || typeof signal.removeEventListener !== "function")) throw invalid();
  if (signal?.aborted) throw cancelled();
  const request = optionsAndChannels(channels, options);
  let handle;
  try { handle = createHandle(request.channels, request.limits); }
  catch (error) { throw nativeFailure(error); }
  const owner = new NativeOwner(handle, signal);
  return new NotificationSubscription(owner, await owner.ready());
}

module.exports = { subscribeDirect, NotificationSubscription };
