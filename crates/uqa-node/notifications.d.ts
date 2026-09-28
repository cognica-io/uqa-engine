export type NotificationFailureCode =
  | "NOTIFICATION_INVALID_REQUEST" | "NOTIFICATION_AUTHENTICATION"
  | "NOTIFICATION_AUTHORITY_REVOKED" | "NOTIFICATION_UNSUPPORTED"
  | "NOTIFICATION_CAPACITY" | "NOTIFICATION_BACKPRESSURE" | "NOTIFICATION_PROTOCOL"
  | "NOTIFICATION_SOURCE_UNAVAILABLE" | "NOTIFICATION_TRANSPORT" | "NOTIFICATION_TIMEOUT"
  | "NOTIFICATION_SERVER_DRAINING" | "NOTIFICATION_CANCELLED" | "NOTIFICATION_SEQUENCE_EXHAUSTED";

export declare class NotificationError extends Error {
  private constructor();
  readonly code: NotificationFailureCode;
  readonly httpStatus: number | null;
  readonly requestId: string | null;
  readonly originalFailure: NotificationError | null;
  readonly lastAttemptFailure: NotificationError | null;
  readonly reconnectAttempts: number | null;
  readonly diagnostic: unknown;
  readonly timeoutStage: "connection" | "readiness" | "idle" | "reconnect" | null;
  readonly retryable: boolean;
  readonly retryAfterMs: number | null;
}

export type NotificationEvent = Readonly<{ epoch: string; requestId: string | null } & (
  | { kind: "notification"; sequence: bigint; processId: number; channel: string; payload: string; cause: null }
  | { kind: "resync_required"; sequence: null; processId: null; channel: null; payload: null; cause: NotificationFailureCode }
  | { kind: "reconnected"; sequence: null; processId: null; channel: null; payload: null; cause: null }
)>;

export interface NotificationRetryOptions {
  maxAttempts: number;
  episodeTimeoutMs: number;
  initialBackoffMs: number;
  maxBackoffMs: number;
  maxRetryAfterMs: number;
}

export interface HttpNotificationOptions {
  maxChannels: number;
  maxQueuedEvents: number;
  maxQueuedBytes: number;
  maxTransportChunkBytes: number;
  connectTimeoutMs: number;
  readyTimeoutMs: number;
  maxIdleTimeoutMs: number;
  retry?: NotificationRetryOptions | null;
  signal?: AbortSignal;
}

export interface NotificationSubscriptionOptions {
  maxActiveSubscriptions: number;
  maxChannels: number;
  maxQueuedNotifications: number;
  maxQueuedBytes: number;
  maxRegistryEntriesPerPoll: number;
  signal?: AbortSignal;
}

export declare class NotificationSubscription implements AsyncIterableIterator<NotificationEvent> {
  private constructor();
  readonly epoch: string;
  readonly requestId: string | null;
  readonly isClosed: boolean;
  nextEvent(): Promise<NotificationEvent | null>;
  next(): Promise<IteratorResult<NotificationEvent>>;
  [Symbol.asyncIterator](): this;
  close(): Promise<void>;
  return(): Promise<IteratorResult<NotificationEvent>>;
  throw(error?: unknown): Promise<IteratorResult<NotificationEvent>>;
}

export declare class HttpNotificationSubscription implements AsyncIterableIterator<NotificationEvent> {
  private constructor();
  readonly epoch: string;
  readonly requestId: string;
  readonly isClosed: boolean;
  nextEvent(): Promise<NotificationEvent | null>;
  next(): Promise<IteratorResult<NotificationEvent>>;
  [Symbol.asyncIterator](): this;
  close(): Promise<void>;
  return(): Promise<IteratorResult<NotificationEvent>>;
  throw(error?: unknown): Promise<IteratorResult<NotificationEvent>>;
}
