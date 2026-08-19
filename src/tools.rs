//! Ядерные инструменты: bash и файловые операции.
//!
//! КОНТРАКТ (владелец: агент `tools`):
//! - [`bash`] — выполнение shell-команд с таймаутом и лимитом вывода;
//! - [`fs`] — read/write/edit/glob/grep;
//! - [`ask`] — интерактивный выбор вариантов пользователем (`propose_options`);
//! - [`core_registry`] — реестр ядерных инструментов;
//! - [`full_registry`] — ядро + доменные инструменты (`mermaid::tools()`,
//!   `rubric::tools()`, `web::tools()`, `kb::tools()`, `control::tools()`,
//!   `model::tools()`, `trace::tools()`, `harness::tools()`, `fleet`).

use std::sync::Arc;

use crate::config::Config;
use crate::tool::{Tool, ToolRegistry};

pub mod ask;
pub mod bash;
pub mod fs;

/// Реестр ядерных инструментов: bash, файлы, glob/grep, `propose_options`.
#[must_use]
pub fn core_registry() -> ToolRegistry {
    ToolRegistry::new()
        .with(Arc::new(bash::BashTool))
        .with(Arc::new(fs::ReadFileTool))
        .with(Arc::new(fs::WriteFileTool))
        .with(Arc::new(fs::EditFileTool))
        .with(Arc::new(fs::GlobTool))
        .with(Arc::new(fs::GrepTool))
        .with(Arc::new(ask::ProposeOptionsTool))
}

/// Полный реестр: ядро + специализированные инструменты архитектора.
/// Политика автономии — из `Config::policy` (R-уровни).
#[must_use]
pub fn full_registry(cfg: &Config) -> ToolRegistry {
    let mut reg = core_registry();
    for tool in domain_tools(cfg) {
        reg.register(tool);
    }
    let policy = crate::policy::Policy::parse(&cfg.policy.autonomy).unwrap_or_default();
    reg.with_policy(policy)
}

fn domain_tools(cfg: &Config) -> Vec<Arc<dyn Tool>> {
    let mut out: Vec<Arc<dyn Tool>> = Vec::new();
    out.extend(crate::mermaid::tools());
    out.extend(crate::rubric::tools());
    out.extend(crate::web::tools());
    out.extend(crate::kb::tools());
    out.extend(crate::control::tools());
    out.extend(crate::model::tools());
    out.extend(crate::trace::tools());
    out.extend(crate::harness::tools(cfg));
    out.extend(crate::plugin::tools(cfg));
    out.extend(crate::agentsmd::tools(cfg));
    out.extend(crate::subagent::tools(cfg));
    out.extend(crate::ralph::tools(cfg));
    out.push(Arc::new(crate::worktree::WorktreeNewTool));
    out.push(Arc::new(crate::fleet::FleetAuditTool));
    out.extend(crate::distill::tools(cfg));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_registry_contains_core_interaction_and_domain_tools() {
        let cfg = Config::default();
        let names = full_registry(&cfg).names();
        for expected in [
            "bash",
            "read_file",
            "propose_options",
            "subagent_run",
            "subagent_list",
            "subagent_result",
            "ralph_run",
            "worktree_new",
            "fleet_audit",
            "skill_distill",
            "skill_search",
            "mermaid_render",
            "model_query",
            "trace_check",
        ] {
            assert!(
                names.iter().any(|n| n == expected),
                "нет инструмента {expected}"
            );
        }
    }
}
