# Image file cache implementation plan

Goal: Track upstream main, retain all images while replacing outbound inline bytes with stable ChatGPT file references, install the result including managed app servers.

Architecture: Reuse the existing ChatGPT file upload API. Cache by backend/account/content hash on disk; concurrently materialize every unique image in a request. Keep canonical local history unchanged, and replace outbound image representation deterministically so repeated requests retain their prefix. No sliding image eviction.

Spec: User's explicit goal in this thread, including one concurrent upload per image and end-to-end installation.

## Steps
- [x] Fetch and integrate upstream main while retaining existing local image fixes.
- [ ] Implement request-time image materialization for ChatGPT authentication only, persistent account-scoped content cache, concurrent upload and stable file IDs.
- [ ] Verify duplicate reuse, concurrent distinct uploads, unchanged image detail/order and both HTTP/WebSocket request paths with focused tests.
- [ ] Build CLI and companion binaries; verify a real two-image request and replay with no additional uploads.
- [ ] Install versioned binaries and update managed daemon packages, checking running executable paths and reported versions.

## Implementation notes
- The current CLI is 0.165.0+imagefix.1; managed daemons include 0.166.0 and local builds. The current agent itself runs under the admin daemon, so restart that service last.
- Existing API upload protocol has been live-tested with two images and 18-image batches; file references preserve image content and simultaneous multi-image model input.
- Keep tests focused as requested. No broad workspace test suite or unrelated refactoring.

## Additional authorized scope
- Add session-scoped `/cp name`, `/usecp name`, `/rmcp name`, `/rmallcp`. Switching model context must preserve TUI scrollback. Implement through existing command/history persistence extension points with minimal upstream divergence.
- Upstream merged: ac91c14443c11c4279a9305099354e443c6fa7d4 (merge 3f0d9a93fc). Baseline `cargo check -p codex-core --lib` passed.
- `/usecp name!` freezes the base for each subsequent user turn, normal intra-turn tool history; idle Ctrl+C restores main context with feedback list and durable branch references. Versioned checkpoint/branch metadata and local codexctl cp/link integration required.

## Verification and delivery notes
- Added default-on `[features] image_file_uploads`; config schema regenerated and docs/config.md plus example-config.md updated.
- Image destination/auth review: enforce compatible resolved ChatGPT backend, forward workspace routing, refresh once after upload 401. Original inline request remains canonical.
- Checkpoint review fixes: abort lifecycle exclusion, restore dynamic tool/MCP context consistently, capture branch after pending feedback and terminal status.
- Local codexctl updated in ~/.local/share/codexctl (not a Git repository); originals backed up under backups/checkpoints-20261102. 32 Python tests passed. `cp` aliases copy; `checkpoint` provides inspection; copy/link carries versioned sidecar state.
- First focused Rust test attempts exposed concurrent checkpoint compilation errors; no passing result claimed. Final focused tests follow the integrated compile fixes.
- Image implementation committed as 3edb380a40. Isolated test of the production cache module passed all concurrency/dedup/persistence/account/order assertions.
- Focused checkpoint tests compiled and ran: one passed, two failed on synthetic fixtures lacking normalized `unknown` content kinds. Fixtures corrected without weakening assertions. Per user request, stop further debug-test rebuilds and proceed to release build plus minimal runtime smoke.
- Checkpoint scoped review now reports no remaining concrete defects after transaction/abort/stale-completion fixes.
