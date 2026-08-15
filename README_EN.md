# Spine

**Spine** is a *domain agent harness for solution architects* — a thin,
Rust-built agent that lives in your terminal and speaks the language of
architecture work: ADRs, architecture-spine invariants, rubrics, fitness
functions, handoff packages for coding agents. One binary, `arch`: TUI,
CLI, and library. Русское описание: [README.md](README.md).

> **Research project.** Spine was built to study what makes domain agent
> harnesses tick — architectural tooling, rubric-based control, governance,
> plugin-packaged skills, autonomy policies. **It is not intended for
> production use** — research and experimentation only.

<p align="center">
  <img src="docs/screenshots/02-chat-mermaid.png" alt="Spine: agent chat with a live mermaid diagram rendered as Unicode art" width="88%">
</p>

<p align="center">
  <img src="docs/screenshots/01-splash.png" alt="Tokyo Night splash" width="32%">
  <img src="docs/screenshots/03-model-picker.png" alt="Model picker (/model)" width="32%">
  <img src="docs/screenshots/04-rubric.png" alt="Anchor rubric with an LLM judge" width="32%">
</p>

<p align="center"><sub>Screenshots are renders of the real TUI (generated from code by a test, not mockups).</sub></p>

## Why a *domain* harness

General-purpose coding agents know git and files; they don't know what an
architecture decision record is, when a change is *significant*, or how a
bank's architecture committee reviews a solution. Spine is deliberately
**thin** — core tools (bash, files) plus a small set of architect-specific
tools. The heavy lifting is artifact discipline, not code:

- **Significance routing (Fast/Standard/Critical)** — a 15-trigger
  Architecture Significance Score decides how much process a change gets.
- **Architecture-spine** — a backbone of invariants with `Binds`/`Prevents`/`Rule`
  fields: only what independent implementers could get *incompatibly wrong*.
- **ADR discipline** — decisions are recorded *before* implementation, with
  alternatives, negative consequences, and reversibility assessment.
- **Evidence over opinion** — rubric scoring by an LLM judge only with quoted
  evidence; fitness functions as machine-checkable claims about the repo.

## Feature tour

- **Beautiful TUI** (ratatui, Tokyo Night): markdown chat, mermaid diagrams
  rendered as Unicode art in a side panel, model picker, slash commands,
  mouse scrolling, fullscreen diagram viewer, docx/xlsx screen export.
- **Models**: DeepSeek V4 (flash/pro), GLM-5.2 family, Kimi K3 — switchable
  mid-session (`/model`), per-request reasoning toggle (`/think on|off`),
  stream-break auto-retry, `reasoning_content` echo for thinking+tools.
- **Skills library & plugins** ([agent-plugins.org](https://agent-plugins.org)
  layout): 50+ built-in architecture skills — ADR authoring, NFR design,
  adversarial review, resilience/integration patterns distilled from
  microservices.io, Azure patterns, AWS Builders' Library, agentic-AI guides,
  and `arch-office` (docx/pptx/xlsx generators for board-ready deliverables).
  `skill_search` + `/skills`, skill distillation from articles (`/distill`).
- **Background sub-agents & Ralph loops**: fresh-context executors with
  least-privilege tool whitelists, plus multi-round fresh-agent cycles with
  bounded structured handoffs (`ralph_run`).
- **Architecture control**: anchor & dynamic rubrics, benchmarks, fitness
  functions, spine linter, AGENTS.md generator/drift linter for team repos.
- **Governance**: R0–R5 autonomy levels, env scrubbing for spawned commands,
  secret redaction in tool output, immutable JSONL session journals,
  transformation KPIs (approval-theater detection, architecture drift,
  cost per validated outcome).
- **Handoff to coding harnesses** (Claude Code, Qwen Code, OpenClaw, Hermes,
  Theseus, CodeWhale): epic-context packages with invariants, acceptance
  criteria, and a headless JSON result contract. Git **worktree factory**
  isolates agent work (`arch worktree new|diff|accept|drop`).
- **MCP** servers (aggregated from plugins), curated architecture websites
  for search/fetch, local knowledge base, cron-scheduled markdown tasks.

## Quick start

```bash
cargo build --release          # binary: target/release/arch
ln -sf "$PWD/target/release/arch" ~/.local/bin/arch   # one-word launch: `arch`
arch init                      # config + assets into ~/.arch-harness and
                               # ~/.config/arch-harness/config.toml

# API keys via environment only (config stores *names* of the variables):
export DEEPSEEK_API_KEY=...    # deepseek (v4-flash, default), deepseek-pro (v4-pro)
export ZHIPU_API_KEY=...       # glm (glm-5.2 + budget 4.7/air/flash)
export KIMI_API_KEY=...        # kimi (k3, coding surface) or file ~/.kimi_api_key

arch                           # interactive TUI (one word)
arch run -q "draft an ADR for saga adoption" > adr.md   # strict headless
```

No-LLM smoke: `arch mermaid examples/mermaid/flow.mmd`,
`arch control score --trigger new_component=true`, `arch doctor`.

## Documentation

- `docs/architecture.md` — system design, contracts, how to extend.
- `docs/tools.md` — full tool reference (30+ tools with parameters).
- `docs/slash_commands.md`, `docs/models.md`, `docs/plugins_and_skills.md`,
  `docs/rubrics_and_benchmarks.md`, `docs/harness_integrations.md`,
  `docs/governance.md`, `docs/mcp.md`, `docs/cron_and_md_pipes.md`,
  `docs/web_kb.md`, `docs/agents_md.md`, `docs/control.md` — mostly in Russian.

## License

MIT — see [LICENSE](LICENSE).
