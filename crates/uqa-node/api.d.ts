import type * as Native from "./index";

export * from "./index";

// The public handles expose the native instance methods and require a factory.
export declare class BackupJob { private constructor(); }
export interface BackupJob extends Native.BackupJob {}

export declare class BackupListing { private constructor(); }
export interface BackupListing extends Native.BackupListing {}

export declare class BackupRepository { private constructor(); }
export interface BackupRepository extends Native.BackupRepository {}

export declare class RestoreJob { private constructor(); }
export interface RestoreJob extends Native.RestoreJob {}
