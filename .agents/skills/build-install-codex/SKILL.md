---
name: build-install-codex
description: Build this Codex checkout and install its CLI and managed app-server daemons locally. Use when asked to rebuild, replace, or update local Codex binaries and services from source.
---

# Build and install local Codex

Use the requested checkout and commit. Inspect `git status` first; preserve unrelated changes and do not silently change the source version. Check the workspace package version in `codex-rs/Cargo.toml` and the built `codex --version`: package metadata alone does not change the compiled client version. An unversioned `0.0.0` client can receive a different ChatGPT model catalog. Resolve that explicitly before replacing a working install.

## Build and package

Use the repository's canonical builder, checking `python3 scripts/build_codex_package.py --help` for the current options. For an optimized local package, use `--cargo-profile release`, a new unique `--package-dir`, and an appropriate host `--target`. On this Mac the target is `aarch64-apple-darwin`; `/opt/homebrew/bin/rg` can be supplied with `--rg-bin`. The builder includes the CLI, `codex-code-mode-host`, `codex-exec-server`, and required package resources. It can use existing binaries via `--entrypoint-bin`, `--code-mode-host-bin`, and `--exec-server-bin` if they were built from the requested source. For V8 download failures, inspect `scripts/codex_package/v8.py` and use matching local artifacts via both `RUSTY_V8_ARCHIVE` and `RUSTY_V8_SRC_BINDING_PATH` when available; do not reuse mismatched versions.

Put each package in a new directory, for example `~/.local/lib/codex-imagefix/releases/<compiled-version>-<short-commit>`. Record the source commit, compiled version, package location, and SHA-256 of all three executables. Verify the new executable starts and reports the intended version before switching anything. Follow the user's testing preferences; building, package and installation verification are distinct from running tests.

## Install

Inspect `command -v codex`, symlinks, wrapper aliases, relevant `CODEX_HOME`s, and daemon status before replacement. On this machine the CLI link has been `/opt/homebrew/bin/codex`, and known homes include `~/.codex`, `~/.codex-a`, `~/.codex-admin`, `~/.codexq`, and `~/Downloads/qwen-local/config/codex`. Discover additional requested homes rather than assuming all use one config. Preserve the previous CLI target and managed releases for rollback; atomically replace the CLI symlink only after the package is ready. Do not overwrite a signed desktop app or unrelated npm-bundled server when installing the local CLI.

For each intended managed home, use the new packaged binary with `CODEX_HOME=<home> codex app-server daemon update --from-cli --yes`. Inspect the installed version and state with `app-server daemon version`; check that the resolved managed executable and both companion binaries match the package hashes. If the daemon was running, verify the live PID and executable path as well. Preserve intentional stopped state unless the user asks to start it. A CLI process already running must be restarted to use the new frontend.

If the daemon hosting the current turn must be updated, do it **last**. Run the update and verification from an independent detached process using a Python interpreter compatible with the script; record output to a durable log before restarting the daemon. Reconnect and inspect the log, daemon status, live PID and hashes. Do not claim that an update was completed merely because a detached process was launched. If an update fails, report which homes succeeded and retain the previous package for recovery.

Report the source commit, compiled version, installed CLI location, each updated home and live verification. Identify any omitted or failed services. Do not claim that bundled desktop or unrelated server processes were updated unless they were actually replaced and verified.
