//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

// Adapt the original Emscripten instance's retained listener, without a worker copy.
import { NotificationError } from "./notification-core.mjs";

const runtimes = new WeakMap();
function listeners(module) {
  let entries = runtimes.get(module);
  if (entries === undefined) {
    entries = new Map();
    runtimes.set(module, entries);
    module.uqaNotificationWake = (id) => entries.get(id)?.deref()?.wake();
  }
  return entries;
}

function failure(value) {
  return value?.failure ? new NotificationError(value.failure.code, { diagnostic: value.failure.diagnostic }) : null;
}

export class WASMNotificationHandle {
  #module;
  #call;
  #id;
  #identity;
  #pending = null;
  #failure = null;
  constructor(module, call, registration) {
    const error = failure(registration);
    if (error) throw error;
    this.#module = module;
    this.#call = call;
    this.#id = registration.id;
    this.#identity = { epoch: registration.epoch, requestId: registration.requestId };
    listeners(module).set(this.#id, new WeakRef(this));
  }
  async run() { return this.#identity; }
  get failureCode() { return this.#failure?.code ?? null; }
  diagnostic() { return this.#failure?.diagnostic ?? null; }
  get isClosed() { return this.#id === null || this.#invoke("notificationStatus").closed; }
  #invoke(method) { return this.#call(this.#module, 0, method, { id: this.#id }); }
  #finish(value = null) {
    const pending = this.#pending;
    this.#pending = null;
    if (this.#failure) pending?.reject(this.#failure);
    else pending?.resolve(value);
  }
  wake() {
    if (this.#pending === null || this.#id === null) return;
    try {
      const result = this.#invoke("notificationNext");
      this.#failure ??= failure(result);
      if (result.pending) return;
      const event = result.event ?? null;
      if (event?.sequence != null) event.sequence = BigInt(event.sequence);
      this.#finish(event);
    } catch (error) {
      this.#failure ??= error instanceof NotificationError ? error : new NotificationError("NOTIFICATION_SOURCE_UNAVAILABLE", { diagnostic: error });
      this.#finish();
    }
  }
  nextEvent() {
    if (this.#pending !== null) return Promise.reject(new NotificationError("NOTIFICATION_INVALID_REQUEST"));
    if (this.#failure) return Promise.reject(this.#failure);
    if (this.#id === null) return Promise.resolve(null);
    return new Promise((resolve, reject) => {
      this.#pending = { resolve, reject };
      this.wake();
    });
  }
  stop() {
    if (this.#id === null) return;
    const status = this.#invoke("notificationStop");
    this.#failure ??= failure(status);
    this.#finish();
  }
  async close() {
    if (this.#id === null) return;
    this.stop();
    const status = this.#invoke("notificationClose");
    this.#failure ??= failure(status);
    listeners(this.#module).delete(this.#id);
    this.#id = null;
  }
}
