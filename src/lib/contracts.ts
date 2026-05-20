// Shared API contracts from DCP-SA/dcp-contracts (git submodule).
//
// This module re-exports type aliases for backend wire-format types so the
// desktop app can reference them in one place when calling api.dcp.sa.
//
// IMPORTANT — scope note:
// The Tauri frontend does NOT call api.dcp.sa directly. All backend calls go
// through Rust commands in src-tauri/src/lib.rs, which reshape responses into
// local DTOs (ProviderDashboard, ProviderMetrics, JobEntry, etc.). Those
// Rust-side shapes do NOT match the contracts 1:1 (see drift notes below),
// so the hand-rolled TypeScript interfaces in src/lib/api.ts intentionally
// remain in place — they mirror Rust structs, not the wire format.
//
// The Python daemon (src-tauri/dcp_daemon.py) IS a direct wire-format
// consumer (HeartbeatRequest, GpuInfo, etc.) but its 8K-line size puts the
// pydantic migration in a separate follow-up PR.
//
// Drift between hand-rolled (api.ts) and contracts:
//   - GpuInfo:        completely different shape (local Tauri probe vs wire
//                     heartbeat field — different domain, both legitimate).
//   - ProviderMetrics vs ProviderStats: contracts adds uptime_hours_last_7d
//                     and avg_job_duration_minutes.
//   - ProviderDashboard vs ProviderProfile: ProviderProfile has ~30 fields;
//                     ProviderDashboard is a Tauri-aggregated subset, and
//                     last_heartbeat nullability differs.
//   - JobEntry vs ProviderRecentJob: field naming diverges
//                     (created_at vs submitted_at; JobEntry adds token
//                     counts; ProviderRecentJob adds dc1_fee_halala).
//
// Adopt contracts types in NEW code paths that talk wire-format directly.
// Do not silently swap existing call sites — open a targeted PR per drift.

import type { components } from "../../contracts/packages/ts/src/index";

type Schemas = components["schemas"];

// Provider lifecycle (wire format)
export type WireProviderRegisterRequest = Schemas["ProviderRegisterRequest"];
export type WireProviderRegisterResponse = Schemas["ProviderRegisterResponse"];
export type WireHeartbeatRequest = Schemas["HeartbeatRequest"];
export type WireHeartbeatLegacyRequest = Schemas["HeartbeatLegacyRequest"];
export type WireHeartbeatResponse = Schemas["HeartbeatResponse"];
export type WirePendingTask = Schemas["PendingTask"];

// GPU + model status (wire format — distinct from local Tauri GpuInfo)
export type WireGpuInfo = Schemas["GpuInfo"];
export type WireModelStatus = Schemas["ModelStatus"];

// Provider /me + metrics (wire format)
export type WireProviderProfile = Schemas["ProviderProfile"];
export type WireProviderMeResponse = Schemas["ProviderMeResponse"];
export type WireProviderStats = Schemas["ProviderStats"];
export type WireProviderRecentJob = Schemas["ProviderRecentJob"];
export type WireProviderActiveJob = Schemas["ProviderActiveJob"];
export type WireProviderMeMetricsResponse = Schemas["ProviderMeMetricsResponse"];

// Jobs (wire format)
export type WireJob = Schemas["Job"];
export type WireJobStatusResponse = Schemas["JobStatusResponse"];

// Errors (wire format — schema-level error envelope)
export type WireError = Schemas["Error"];
export type WireV1Error = Schemas["V1Error"];

// HTTP responses (status-code-mapped wrappers) live under
// `components["responses"]` — pull them directly from the re-export below
// if needed (Unauthorized, BadRequest, NotFound, Forbidden, etc.).

// Re-export the full OpenAPI shape for advanced callers.
export type { components, paths, operations } from "../../contracts/packages/ts/src/index";
