# Architecture

This repository currently ships a Rust core with a concrete Codex adapter.

## Goals

- Keep the current account-selection behavior and local-first workflow.
- Ship a single cross-platform binary for the wrapper itself.
- Keep the adapter boundary explicit enough that another CLI can be added later.
- Avoid abstract capability traits until a second real adapter needs them.

## Non-goals

- Do not assume every CLI supports live usage refresh.
- Do not assume every CLI can switch accounts by replacing one credentials file.

## Layers

### Core

The core owns behavior that should be identical across CLIs:

- command parsing
- state storage
- account records and usage snapshots
- account ranking
- "keep current account if still usable" policy
- shared output formatting

The core must not know about `~/.codex/auth.json`, `~/.claude/.credentials.json`, or any other CLI-specific paths.

### Codex Adapter

The current implementation targets Codex only. `CodexAdapter` translates the core workflow into Codex-specific behavior.

Examples:

- discover the active identity for the CLI
- import known credentials into local state
- refresh live usage, if the CLI exposes a reliable source
- switch account, profile, or provider
- run login
- launch or resume the underlying CLI

## Adapter Boundary Decision

Short term, scodex is a single-CLI launcher for Codex. The previous `CliAdapter` trait only exposed `id()` and `capabilities()`, while every real call used `CodexAdapter` directly. Keeping that trait made the code look more generic than it was.

The current code therefore uses the concrete `CodexAdapter` directly and does not keep a placeholder capability model. When a second adapter is implemented, introduce a trait around real call sites such as `launch`, `import_known`, `refresh_all`, and `read_live_identity`.

Until then, capability behavior is documented by the concrete Codex implementation and tests.

## Current rollout

Phase 1 is complete: Codex support now runs on the Rust implementation.

- keep the Codex path stable in Rust
- preserve local-state compatibility for existing users
- continue tightening tests around install, update, deploy, and account selection flows

Phase 2 can add new adapters one by one after the trait boundary is justified by a second implementation.

- `OpenCodeAdapter` is the first candidate after Codex because its auth/config surface is comparatively explicit
- `ClaudeCodeAdapter` and `GeminiCliAdapter` should only move past proof-of-concept after identity switching and usage semantics are validated

## Repository mapping

- installer and shell integration live in `install.sh` and `install.ps1`
- core policy lives under `src/core`
- CLI-specific Codex behavior lives under `src/adapters/codex/`
- top-level command parsing and help live in `src/cli.rs`
