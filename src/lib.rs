//! # arch-harness — доменный харнесс solution-архитектора
//!
//! Тонкий агентный харнесс для архитектора в корпоративном контуре:
//! TUI (ratatui), mermaid→ASCII рендер, якорные/динамические рубрики
//! архитектурного контроля, специализированные бенчмарки, MCP-интеграции,
//! веб-доступ к доменным знаниям, локальная база знаний, handoff-пакеты
//! кодовым харнессам (Claude Code, Qwen Code, OpenClaw, Hermes, Theseus,
//! CodeWhale), fitness functions и линтер architecture-spine, крон md-задач.
//!
//! Происхождение идей — `docs/SOURCE_BRIEF.md` (разбор SDD-харнессов и
//! корпоративных агентных фреймворков, август 2026).
//!
//! ```no_run
//! use arch_harness::config::Config;
//! let cfg = Config::load(None).expect("config");
//! assert!(cfg.models.contains_key("deepseek"));
//! ```

pub mod agent;
pub mod agentsmd;
pub mod assets;
pub mod bench;
pub mod clipboard;
pub mod config;
pub mod control;
pub mod cron;
pub mod delta;
pub mod detectors;
pub mod distill;
pub mod doctor;
pub mod error;
pub mod evidence;
pub mod export;
pub mod harness;
pub mod hooks;
pub mod kb;
pub mod llm;
pub mod matchers;
pub mod mcp;
pub mod mermaid;
pub mod metrics;
pub mod model;
pub mod plugin;
pub mod policy;
pub mod ralph;
pub mod retry;
pub mod rubric;
pub mod secrets;
pub mod subagent;
pub mod tool;
pub mod tools;
pub mod tui;
pub mod web;
pub mod worktree;

pub use config::Config;
pub use error::{HarnessError, Result};
