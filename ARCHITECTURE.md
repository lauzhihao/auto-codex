# Architecture

This repository is moving from a single Python wrapper around Codex CLI to a Rust core with per-CLI adapters.

## Goals

- Keep the current account-selection behavior and local-first workflow.
- Ship a single cross-platform binary for the wrapper itself.
- Support multiple AI CLIs without forcing them into one fake auth model.
- Let each CLI declare the capabilities it actually supports.

## Non-goals

- Do not assume every CLI supports live usage refresh.
- Do not assume every CLI can switch accounts by replacing one credentials file.
- Do not remove the existing Python implementation until the Rust path reaches feature parity for Codex.

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

### Adapter

Each CLI gets its own adapter. Adapters are responsible for translating the core's generic actions into tool-specific behavior.

Examples:

- discover the active identity for the CLI
- import known credentials into local state
- refresh live usage, if the CLI exposes a reliable source
- switch account, profile, or provider
- run login
- launch or resume the underlying CLI

## Capability model

Each adapter must explicitly declare which features it supports.

- `import_known`
- `read_current_identity`
- `switch_account`
- `login`
- `launch`
- `resume`
- `live_usage`

If an adapter does not support `live_usage`, the core must degrade gracefully instead of pretending automatic account scoring works.

## Initial rollout

Phase 1 is Codex-only in Rust.

- keep current Python implementation as the production path
- build the Rust CLI skeleton
- port domain models and selection policy first
- add a `CodexAdapter`
- migrate commands incrementally after tests exist

Phase 2 adds new adapters one by one.

- `OpenCodeAdapter` is the first candidate after Codex because its auth/config surface is comparatively explicit
- `ClaudeCodeAdapter` and `GeminiCliAdapter` should only move past proof-of-concept after identity switching and usage semantics are validated

## Current repository mapping

The Python implementation currently mixes all layers together:

- installer and shell integration live in `install.sh`
- core policy and CLI-specific Codex behavior both live in `codex-autoswitch.py`

The Rust migration should separate these concerns from the start.

