# AGENTS.md — Spine

Guidance for AI agents (and humans) working on this repository.
**Reading/exploring the repo instead? See `AGENTS-READERS.md`.**

**Spine** is a *domain agent harness for solution architects*: a thin,
Rust-built terminal agent with architecture-specific tooling — ADRs,
architecture-spine invariants, rubrics with an evidence-bound LLM judge,
fitness functions, handoff packages for coding harnesses, a skills/plugins
library, background sub-agents, and governance. One binary, `arch`: TUI +
CLI + library. See `README.md` (bilingual RU/EN) for the full feature tour.

> **Research project** — not intended for production use; built to study
> domain agent harnesses.

## Install & run (one-minute setup)

```bash
cargo build --release          # binary: target/release/arch
ln -sf "$PWD/target/release/arch" ~/.local/bin/arch   # one-word launch: `arch`
arch init                      # config + assets into ~/.arch-harness and
                               # ~/.config/arch-harness/config.toml

arch                           # interactive TUI (default command)
arch run -q "draft an ADR for saga adoption" > adr.md   # strict headless
arch doctor                    # environment check (keys, dirs, plugins, MCP)
```

API keys come from the environment or key files — never from the config
(values are never stored there):

```bash
export DEEPSEEK_API_KEY=...    # deepseek (v4-flash, default), deepseek-pro (v4-pro)
export ZHIPU_API_KEY=...       # glm (glm-5.2 + budget 4.7/air/flash)
export KIMI_API_KEY=...        # kimi (k3, coding surface) or file ~/.kimi_api_key
```

No-LLM smoke: `arch mermaid examples/mermaid/flow.mmd`,
`arch control score --trigger new_component=true`, `arch doctor`.

## Commands for development

- Build: `cargo build` / fast check: `cargo check`
- Tests: `cargo test` (live-LLM tests are `#[ignore]`d — they need keys and network)
- Lint: `cargo clippy --all-targets` (pedantic warnings are tolerated for now)
- Release: `cargo build --release`
- README screenshots regenerate from code: `ARCH_GEN_SHOTS=1 cargo test gen_readme_screenshots`

## Architecture map

| Area | Files | Role |
|---|---|---|
| Agent loop | `src/agent.rs`, `src/agent/{slash,prompts}.rs` | turn loop, tool dispatch, compaction (L1/prune/L3), session journal (append-only JSONL), slash commands |
| LLM | `src/llm.rs`, `src/llm/openai_compat.rs`, `{deepseek,kimi,glm}.rs` | OpenAI-compatible client (SSE streaming, retries, stream-break recovery, `reasoning_content` echo, thinking maps) |
| Tools | `src/tool.rs`, `src/tools/{bash,fs,ask}.rs`, `src/tools.rs` | registry, policy gate (R0–R5), bash with env-scrub + orthogonal outcome markers, file ops with fuzzy edit |
| Domain tools | `src/{rubric,bench,control,model,agentsmd,evidence,metrics,delta,worktree,subagent,ralph,distill,harness,kb,web,mcp,mermaid,plugin}.rs` | architect-specific tooling (see README) |
| TUI | `src/tui.rs`, `src/tui/{app,render,text,theme}.rs` | ratatui Tokyo Night; ask-modal, model picker, tabs, fullscreen viewer |
| Config | `src/config.rs` | `~/.config/arch-harness/config.toml` (all personal paths live HERE, never in code) |
| Assets | `assets/`, `src/assets.rs` | embedded prompts/rubrics/benchmarks/plugins, deployed by `arch init` |
| Entry | `src/main.rs` | CLI (clap) + wiring |

## Conventions (enforced)

- **No `unsafe`**, no `unwrap`/`expect` outside tests. Doc comments are in
  Russian (`///`); user-facing text is Russian; code/identifiers English.
- Errors: `HarnessError`/`Result` (thiserror) in the library; `anyhow` with
  `.context()` at the CLI edge. A tool failure is `ToolOutput::err`, never a panic.
- Tests are deterministic and self-contained: `tempfile` + `Config::default()`
  with overridden `paths.*`; no network, no real home dir, no real plugin
  libraries (fixtures set `plugins.include_hooks = false`).
- **Secrets**: never print, log, or commit key material; keys resolve lazily
  via `api_key_env`/`api_key_file`; tool output and journals pass through the
  redactor (`src/secrets.rs`); spawned commands get a scrubbed environment
  (`[bash] env_scrub`).
- **No personal paths in the repo** — machine-specific directories
  (knowledge bases, plugin libraries) belong to the user config only
  (see README “Configuring personal paths”). This is checked before every push.
- Orthogonal outcomes are reported independently (exit code, signal, timeout,
  truncation — separate markers, never nested).
- Swallowed errors (`let _ = …`) carry a comment naming what is ignored and
  why it is safe. Numeric limits are named `MAX_*` constants with docs.

## How to extend

- **New agent tool**: implement `Tool` (`spec` + `call`), register in
  `tools::domain_tools`, add a doc row in `docs/tools.md`, add tests.
- **New model**: add `[models.<name>]` to the config (base_url, model,
  api_key_env/api_key_file, `thinking_on/off` maps, `context_limit`).
  Any OpenAI-compatible endpoint works out of the box.
- **New skill/plugin**: a directory under a `[plugins] dirs` entry —
  `plugin.json` + `skills/<name>/SKILL.md` (+ optional `mcp.json`,
  `agents/*.md`, `hooks/hooks.json`). The plugin is the only install unit;
  skills never install separately.
- **New slash command**: `src/agent/slash.rs` — `execute()` arm + `catalog()`
  entry + a test; update `docs/slash_commands.md`.

## Definition of done for a change

1. `cargo test` green (incl. new tests for the change) and
   `cargo build --release` clean.
2. Docs touched by the change updated (`docs/*.md`, README when user-facing).
3. No secrets or personal paths added (run a grep gate before pushing).
4. Session journal facts: user-visible behavior changes are reflected in
   `docs/architecture.md` when the loop contract moves.
