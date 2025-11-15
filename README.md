# Alisa — CLI Task Orchestrator

Alisa helps you quickly prepare a workspace for an AI agent: it creates the `.alisa` directory, stores task context, validates artifacts before execution, and prevents race conditions between multiple processes. Use it to standardize your task workflow and keep an automatic record of changes.

## Installation

1. Install [Rust](https://www.rust-lang.org/) and Cargo (stable 1.76+ works).
2. Build and install the CLI:  
   ```bash
   cargo install --path .
   ```
   or run it directly from the repository:  
   ```bash
   cargo run -- init
   ```

## Quick start

- `alisa init` — creates (or re-checks) `.alisa`, bootstrapping the manifest and helper files.
- `alisa init --dry-run` — shows what would be created/updated without touching the filesystem.
- `alisa init --check` — validates the existing structure and reports any issues.
- `alisa init --force` — recreates service databases (registry/audit/RAG) and other artifacts when you need a clean slate.

Exit codes:
- `0` — everything is ready;
- `1` — validation failed or another error occurred;
- `2` — incompatible schema version detected;
- `3` — the workspace is locked by another process;
- `130` — command interrupted (Ctrl+C).

## What `alisa init` creates

Inside `.alisa` you’ll find the configuration and indexes the agent relies on:
- `manifest.json` — workspace ID and schema versions;
- `state/project.toml` and `state/runtime.toml` — snapshots of user settings and derived parameters;
- `state/session/current.json` — the current state of tasks/runs;
- SQLite databases `state/registry.sqlite`, `audit/audit_index.sqlite`, `cache/rag/index.sqlite`;
- the helper `.gitignore`, plus directories like `cache/`, `tasks/`, `audit/`, etc.

In most cases running `alisa init` once gives you the full set of artifacts. There’s no need to edit them manually—the CLI repairs their contents whenever needed.

## Workspace locking

`alisa init` uses `.alisa/locks/workspace.lock` to prevent concurrent mutations. If you launch the command again while a previous run is still in progress, you’ll see `workspace is locked by another process`. Just wait for the first run to finish or stop it; when it exits it releases the lock.

## Prompts and interactive hints

When the CLI detects a corrupted file (for example, a broken JSON/TOML), it reports the issue and asks whether it should overwrite the artifact. In interactive mode press `Y` to repair the file or `n` to leave it untouched. If you don’t answer within about 30 seconds the operation is canceled.
