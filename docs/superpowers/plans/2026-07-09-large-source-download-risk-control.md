# Large Source Download Risk Control Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** Add configurable large-source download protection under the submission risk-control settings so huge sources do not repeatedly trigger Bilibili `playurl` 412 wind-control.

**Architecture:** Extend `SubmissionRiskControlConfig` with download-stage protection fields, expose them through config API request/response/update paths, and apply them in `workflow.rs` only when a source exceeds the configured large-source threshold. Add a scoped `playurl` limiter and audio-only low-qn path so M4A-only sources avoid 8K playurl probing.

**Tech Stack:** Rust, SeaORM, Tokio, serde config, SvelteKit settings UI, existing `cargo test -p bili_sync` tests.

---

### Task 1: Config/API surface

**Files:**
- Modify: `crates/bili_sync/src/config/item.rs`
- Modify: `crates/bili_sync/src/api/request.rs`
- Modify: `crates/bili_sync/src/api/response.rs`
- Modify: `crates/bili_sync/src/api/handler.rs`

- [x] **Step 1: Write failing tests for serde defaults and update mapping**

Add unit tests proving omitted JSON receives safe defaults and update params persist new fields into `submission_risk_control`.

- [x] **Step 2: Implement fields and API mapping**

Add these fields with defaults: `enable_large_source_download_limit=true`, `large_source_download_threshold=500`, `large_source_max_videos_per_round=10`, `large_source_max_pages_per_round=50`, `large_source_concurrent_video=1`, `large_source_concurrent_page=1`, `large_source_playurl_limit=1`, `large_source_playurl_duration_ms=1000`, `audio_only_use_low_qn_for_playurl=true`.

- [x] **Step 3: Verify config tests**

Run `cargo test -p bili_sync large_source -- --nocapture` and confirm new tests pass.

### Task 2: Workflow large-source policy

**Files:**
- Modify: `crates/bili_sync/src/workflow.rs`

- [x] **Step 1: Write failing tests for policy and budget slicing**

Add tests for large-source detection, concurrency override, and video/page budget slicing.

- [x] **Step 2: Implement policy helpers**

Add a small `LargeSourceDownloadPolicy` helper that computes effective video/page concurrency and selected download queue.

- [x] **Step 3: Wire policy into download stage**

Use the policy in `download_unprocessed_videos` before building futures and pass effective page concurrency into `DownloadPageArgs`.

### Task 3: Playurl limiter and audio-only low qn

**Files:**
- Modify: `crates/bili_sync/src/bilibili/video.rs`
- Modify: `crates/bili_sync/src/bilibili/mod.rs`
- Modify: `crates/bili_sync/src/workflow.rs`

- [x] **Step 1: Write failing tests for qn selection**

Add tests proving audio-only M4A mode with the new flag uses `qn=16` instead of the configured 8K-to-360P range.

- [x] **Step 2: Implement scoped playurl limiter**

Add a Tokio task-local limiter that delays actual playurl sends while the large-source policy is active.

- [x] **Step 3: Apply audio-only qn override**

In `fetch_page_video`, when `audio_only && audio_only_use_low_qn_for_playurl`, call playurl with `max_qn=min_qn=16`.


### Task 4: Frontend settings UI

**Files:**
- Modify: `web/src/lib/types.ts`
- Modify: `web/src/routes/settings/+page.svelte`
- Modify: `web/src/lib/utils/glossary.ts`

- [x] **Step 1: Add API types**

Expose the large-source download protection fields in `ConfigResponse` and `UpdateConfigRequest`.

- [x] **Step 2: Add settings controls**

Add controls under the existing wind-control settings drawer for threshold, per-round budgets, video/page concurrency, playurl rate window, and audio-only low-qn probing.

- [x] **Step 3: Add glossary terms**

Add search/help glossary terms for the new large-source download protection settings.

### Task 5: Validation and PR update

**Files:**
- All modified files above

- [x] **Step 1: Format and targeted tests**

Run `rustfmt --edition 2021 --check` for modified Rust files and targeted `cargo test -p bili_sync` filters.

- [x] **Step 2: Full crate check**

Run `cargo check -p bili_sync`; accept only the known existing `web/build` warning.

- [x] **Step 3: Commit and push**

Commit in Chinese and push `fix/risk-control-download-resume` so PR #203 updates.
