//! Слэш-команды харнесса.
//!
//! КОНТРАКТ (владелец: агент `agent`). Минимальный набор:
//! `/help` `/model [name]` `/think [on|off|auto]` `/clear` `/new` `/quit` `/tools`
//! `/prompts [name]` `/mermaid <file|код>` `/adr new <title>` `/spine lint <file>`
//! `/rubric list|run <name> <file>` `/bench list|run <name>`
//! `/kb <query>` `/web <query>` `/fetch <url>` `/sites`
//! `/handoff <harness> <repo>` `/control <repo>` `/score` (интерактив)
//! `/mcp list` `/save <file>` `/load <file>` `/worktree` (список).
//! Неизвестная команда — [`SlashOutcome::Unknown`]; не слэш — [`SlashOutcome::NotSlash`].

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use crate::error::{HarnessError, Result};
use crate::tool::ToolContext;

use super::AgentSession;
use super::prompts;

/// Исход слэш-команды.
#[derive(Debug, Clone)]
pub enum SlashOutcome {
    /// Команда обработана; текст для показа пользователю.
    Handled(String),
    /// `/model` без аргумента: UI предлагается открыть пикер моделей
    /// (TUI — модалка; выбор сводится к `/model <name>`).
    PickModel,
    /// `/new`: новая сессия — сессия уже ротирована исполнителем
    /// (история пуста, журнал — новый файл); UI очищает блоки диалога.
    NewSession,
    /// Выход из приложения.
    Quit,
    /// Неизвестная команда.
    Unknown(String),
    /// Ввод не является слэш-командой — передать модели.
    NotSlash,
}

/// Проверка, что ввод — слэш-команда.
#[must_use]
pub fn is_slash(input: &str) -> bool {
    input.trim_start().starts_with('/')
}

/// Выполняет слэш-команду.
///
/// Ошибки подкоманд (файл не найден, модель недоступна) возвращаются как
/// `Err`; проблемы использования (нет аргументов) — как `Handled` с подсказкой.
///
/// # Errors
/// Ошибка подкоманды (файл не найден, модель недоступна и т.п.).
pub async fn execute(
    input: &str,
    session: &mut AgentSession,
    ctx: &ToolContext,
) -> Result<SlashOutcome> {
    let trimmed = input.trim();
    if !is_slash(trimmed) {
        return Ok(SlashOutcome::NotSlash);
    }
    let mut parts = trimmed.splitn(2, |c: char| c.is_whitespace());
    let cmd = parts.next().unwrap_or_default();
    let rest = parts.next().unwrap_or_default().trim();

    match cmd {
        "/help" => Ok(SlashOutcome::Handled(help_text())),
        "/compact" => cmd_compact(session).await,
        "/model" => cmd_model(rest, session, ctx),
        "/think" => cmd_think(rest, session, ctx),
        "/clear" => {
            session.clear();
            Ok(SlashOutcome::Handled("контекст очищен".into()))
        }
        "/new" => {
            session.reset();
            Ok(SlashOutcome::NewSession)
        }
        "/quit" => Ok(SlashOutcome::Quit),
        "/tools" => Ok(SlashOutcome::Handled(tools_text(session))),
        "/prompts" => cmd_prompts(rest, ctx),
        "/mermaid" => cmd_mermaid(rest, ctx),
        "/adr" => cmd_adr(rest, ctx),
        "/spine" => cmd_spine(rest, ctx),
        "/rubric" => cmd_rubric(rest, ctx).await,
        "/bench" => cmd_bench(rest, ctx).await,
        "/kb" => cmd_kb(rest, ctx).await,
        "/web" => cmd_web(rest, ctx).await,
        "/fetch" => cmd_fetch(rest, ctx).await,
        "/sites" => Ok(SlashOutcome::Handled(sites_text(ctx))),
        "/handoff" => cmd_handoff(rest, ctx),
        "/control" => cmd_control(rest, ctx),
        "/score" => Ok(SlashOutcome::Handled(score_text(rest))),
        "/mcp" => cmd_mcp(rest, ctx).await,
        "/doctor" => Ok(SlashOutcome::Handled(crate::doctor::render(
            &crate::doctor::run_checks(&ctx.config),
        ))),
        "/skills" => cmd_skills(rest, ctx),
        "/skill" => cmd_skill(rest, session, ctx),
        "/plugins" => cmd_plugins(rest, ctx),
        "/agents" => Ok(SlashOutcome::Handled(cmd_agents(ctx))),
        "/worktree" => cmd_worktree_list(ctx).await,
        "/distill" => cmd_distill(rest, session, ctx).await,
        "/save" => cmd_save(rest, session, ctx),
        "/load" => cmd_load(rest, session, ctx),
        "/agentsmd" => cmd_agentsmd(rest, ctx),
        "/sessions" => cmd_sessions(ctx),
        "/resume" => cmd_resume(rest, session, ctx),
        other => Ok(SlashOutcome::Unknown(
            other.trim_start_matches('/').to_string(),
        )),
    }
}

/// Список команд с краткими описаниями (для /help и палитры TUI).
#[must_use]
pub fn catalog() -> Vec<(&'static str, &'static str)> {
    vec![
        ("/help", "справка по командам"),
        ("/compact", "сжать контекст сейчас (L1 + LLM-саммари L3)"),
        ("/model [name]", "сменить модель; без имени — пикер моделей"),
        ("/think [on|off]", "ризонинг-режим модели (thinking)"),
        ("/clear", "очистить контекст"),
        ("/new", "новая сессия: чистый диалог и журнал"),
        ("/quit", "выход"),
        ("/tools", "список инструментов"),
        ("/prompts [name]", "библиотека промптов / показать шаблон"),
        ("/mermaid <file|код>", "рендер mermaid в ASCII"),
        ("/adr new <title>", "новый ADR по шаблону"),
        ("/spine lint <file>", "линтер ARCHITECTURE-SPINE.md"),
        ("/rubric list|run", "рубрики: список / оценка файла"),
        ("/bench list|run", "бенчмарки: список / прогон"),
        ("/kb <query>", "поиск по локальной базе знаний"),
        ("/web <query>", "веб-поиск (архитектурные сайты)"),
        ("/fetch <url>", "загрузить страницу текстом"),
        ("/sites", "кураторские сайты архитектора"),
        ("/handoff <harness> <repo>", "сформировать handoff-пакет"),
        ("/control <repo>", "архитектурный контроль (fitness functions)"),
        ("/score", "Architecture Significance Score → маршрут Fast/Standard/Critical"),
        ("/mcp list", "MCP-серверы и инструменты"),
        ("/doctor", "диагностика окружения"),
        ("/export <word|excel> [путь]", "экран диалога в .docx/.xlsx"),
        ("/skills [query]", "поиск по библиотеке скиллов"),
        ("/skill <name>", "загрузить скилл в контекст"),
        ("/plugins", "плагины: скиллы + MCP"),
        ("/agents", "субагенты: спеки из плагинов + фоновые задачи"),
        ("/worktree", "изолированные git worktree агентов (список)"),
        ("/distill <name> [plugin]", "дистиллировать контекст сессии в скилл"),
        ("/save <file>", "сохранить транскрипт"),
        ("/load <file>", "включить файл в контекст"),
        ("/agentsmd <repo>", "сгенерировать/проверить AGENTS.md репозитория"),
        ("/sessions", "журналы прошлых сессий"),
        ("/resume <file|last>", "восстановить сессию из журнала"),
    ]
}

/// Текст `/help`: таблица из каталога команд.
fn help_text() -> String {
    let mut out = String::from("Команды:\n");
    for (name, desc) in catalog() {
        let _ = writeln!(out, "  {name:<28} {desc}");
    }
    out
}

/// Текст `/tools`: инструменты текущей сессии с первой строкой описания
/// (полный справочник — `docs/tools.md`).
fn tools_text(session: &AgentSession) -> String {
    let specs = session.tool_specs();
    if specs.is_empty() {
        return "инструменты не зарегистрированы".into();
    }
    let mut out = format!("Инструменты ({}):\n", specs.len());
    for spec in specs {
        let desc = spec
            .description
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(90)
            .collect::<String>();
        let _ = writeln!(out, "  {:<22} {desc}", spec.name);
    }
    out.push_str("\nСправочник с параметрами и примерами — docs/tools.md репозитория харнесса.");
    out
}

/// `/compact`: принудительная компактификация контекста (L1-маскирование
/// старых tool-результатов + L3-саммари), вне автоматических порогов.
async fn cmd_compact(session: &mut AgentSession) -> Result<SlashOutcome> {
    match session.compact_now().await {
        Ok((before, after, folded, truncated)) => {
            let text = if folded == 0 && truncated == 0 {
                format!("компактить нечего: история ~{before} ток. (грубая оценка)")
            } else {
                format!(
                    "компактификация: ~{before} → ~{after} ток.; свёрнуто сообщений: {folded}, \
                     усечено tool-результатов: {truncated}. Автопороги: L1 70% / L3 95% от \
                     min(бюджет, окно модели)."
                )
            };
            Ok(SlashOutcome::Handled(text))
        }
        Err(e) => Ok(SlashOutcome::Handled(format!(
            "компактификация не удалась (модель-саммаризатор): {e}"
        ))),
    }
}

/// `/model [name]`: без аргумента — пикер моделей (TUI; исход
/// [`SlashOutcome::PickModel`]), с аргументом — переключение через реестр.
fn cmd_model(rest: &str, session: &mut AgentSession, ctx: &ToolContext) -> Result<SlashOutcome> {
    if rest.is_empty() {
        if ctx.llm.is_some() {
            return Ok(SlashOutcome::PickModel);
        }
        return Ok(SlashOutcome::Handled(format!(
            "Текущая модель: {}\nРеестр моделей недоступен в этом контексте.",
            session.model_name()
        )));
    }
    let registry = ctx
        .llm
        .as_ref()
        .ok_or_else(|| HarnessError::Config("реестр LLM недоступен в этом контексте".into()))?;
    let provider = registry.get(rest)?;
    let model = provider.model().to_string();
    session.set_provider(provider);
    Ok(SlashOutcome::Handled(format!(
        "модель переключена: {rest} ({model})"
    )))
}

/// `/think [on|off|auto]`: переключатель ризонинг-режима активной модели.
/// В тело запроса сливается карта `thinking_on`/`thinking_off` из конфига
/// модели (DeepSeek V4/GLM: `thinking.type`; Kimi K3: `reasoning_effort`).
/// Без аргумента — статус; `auto` — вернуться к дефолту провайдера.
fn cmd_think(rest: &str, session: &mut AgentSession, ctx: &ToolContext) -> Result<SlashOutcome> {
    let name = session.provider().name().to_string();
    let mc = ctx.config.models.get(&name);
    let map_for = |on: bool| {
        let m = mc?;
        let slot = if on { &m.thinking_on } else { &m.thinking_off };
        slot.as_ref()
            .map(|v| serde_json::to_string(v).unwrap_or_default())
    };
    let state = match session.thinking() {
        Some(true) => "on",
        Some(false) => "off",
        None => "auto (дефолт провайдера)",
    };
    match rest {
        "" => {
            let support = match (mc.and_then(|m| m.thinking_on.as_ref()), mc.and_then(|m| m.thinking_off.as_ref())) {
                (Some(on), Some(off)) => format!(
                    "карты: on → {}, off → {}",
                    serde_json::to_string(on).unwrap_or_default(),
                    serde_json::to_string(off).unwrap_or_default()
                ),
                _ => "карты thinking_on/thinking_off не настроены — переключение недоступно".to_string(),
            };
            Ok(SlashOutcome::Handled(format!(
                "Ризонинг: {state} · модель {name} · {support}"
            )))
        }
        "on" | "off" => {
            let on = rest == "on";
            let Some(map) = map_for(on) else {
                return Ok(SlashOutcome::Handled(format!(
                    "для модели {name} не настроена карта thinking_{rest} в config.toml — \
                     переключатель не применён"
                )));
            };
            session.set_thinking(Some(on));
            Ok(SlashOutcome::Handled(format!(
                "ризонинг: {rest} ({name}); в тело запроса сливается {map}"
            )))
        }
        "auto" => {
            session.set_thinking(None);
            Ok(SlashOutcome::Handled(format!(
                "ризонинг: auto ({name}) — параметры thinking не шлются, дефолт провайдера"
            )))
        }
        _ => Ok(SlashOutcome::Handled("использование: /think [on|off|auto]".into())),
    }
}

/// `/prompts [name]`: список шаблонов или тело одного.
fn cmd_prompts(rest: &str, ctx: &ToolContext) -> Result<SlashOutcome> {
    let dir = ctx.config.paths.prompts_dir();
    let lib = prompts::load_library(&dir)?;
    if rest.is_empty() {
        if lib.is_empty() {
            return Ok(SlashOutcome::Handled(format!(
                "библиотека промптов пуста ({})",
                dir.display()
            )));
        }
        let mut out = format!("Библиотека промптов ({}):\n", dir.display());
        for tpl in &lib {
            let _ = writeln!(out, "  {:<24} {}", tpl.name, tpl.description);
        }
        return Ok(SlashOutcome::Handled(out));
    }
    match lib.iter().find(|t| t.name == rest) {
        Some(tpl) => Ok(SlashOutcome::Handled(tpl.body.clone())),
        None => Ok(SlashOutcome::Handled(format!("шаблон '{rest}' не найден"))),
    }
}

/// `/mermaid <file|код>`: inline-код (начинается с graph/flowchart/
/// sequenceDiagram) рендерится напрямую, иначе аргумент — путь к файлу.
fn cmd_mermaid(rest: &str, ctx: &ToolContext) -> Result<SlashOutcome> {
    if rest.is_empty() {
        return Ok(SlashOutcome::Handled(
            "использование: /mermaid <file|код>".into(),
        ));
    }
    let is_code = rest.starts_with("graph")
        || rest.starts_with("flowchart")
        || rest.starts_with("sequenceDiagram");
    let code = if is_code {
        rest.to_string()
    } else {
        let path = ctx.resolve(rest);
        // Каталог — это подсказка по использованию (блок-подсказка),
        // а не красная ошибка команды.
        match crate::mermaid::read_diagram_source(&path) {
            Ok(code) => code,
            Err(HarnessError::Mermaid(hint)) => return Ok(SlashOutcome::Handled(hint)),
            Err(e) => return Err(e),
        }
    };
    Ok(SlashOutcome::Handled(crate::mermaid::render(&code)?))
}

/// `/adr new <title>`: новый ADR в `docs/adr` рабочего каталога.
fn cmd_adr(rest: &str, ctx: &ToolContext) -> Result<SlashOutcome> {
    let title = rest.strip_prefix("new").map(str::trim).unwrap_or_default();
    if title.is_empty() {
        return Ok(SlashOutcome::Handled(
            "использование: /adr new <title>".into(),
        ));
    }
    let path = crate::control::adr_new(&ctx.resolve("docs/adr"), title)?;
    Ok(SlashOutcome::Handled(format!(
        "ADR создан: {}",
        path.display()
    )))
}

/// `/spine lint <file>`: линтер ARCHITECTURE-SPINE.md.
fn cmd_spine(rest: &str, ctx: &ToolContext) -> Result<SlashOutcome> {
    let parts: Vec<&str> = rest.split_whitespace().collect();
    let ["lint", file] = parts.as_slice() else {
        return Ok(SlashOutcome::Handled(
            "использование: /spine lint <file>".into(),
        ));
    };
    let issues = crate::control::lint_spine(&ctx.resolve(file))?;
    if issues.is_empty() {
        return Ok(SlashOutcome::Handled(format!("{file}: замечаний нет")));
    }
    let mut out = format!("{file}: {} замечаний\n", issues.len());
    for i in &issues {
        let _ = writeln!(
            out,
            "  [{}] {}:{} {}: {}",
            i.severity,
            i.file.display(),
            i.line,
            i.rule,
            i.message
        );
    }
    Ok(SlashOutcome::Handled(out))
}

/// `/rubric list|run <name> <file>`: список рубрик / оценка файла LLM-судьёй.
async fn cmd_rubric(rest: &str, ctx: &ToolContext) -> Result<SlashOutcome> {
    let parts: Vec<&str> = rest.split_whitespace().collect();
    match parts.as_slice() {
        ["list"] => {
            let dir = ctx.config.paths.rubrics_dir();
            let items = crate::rubric::list(&dir)?;
            if items.is_empty() {
                return Ok(SlashOutcome::Handled(format!(
                    "рубрик нет ({})",
                    dir.display()
                )));
            }
            let mut out = format!("Рубрики ({}):\n", dir.display());
            for r in &items {
                let _ = writeln!(
                    out,
                    "  {:<24} ({} критериев) {}",
                    r.name, r.criteria_count, r.description
                );
            }
            Ok(SlashOutcome::Handled(out))
        }
        ["run", name, file] => {
            let path = resolve_named(&ctx.config.paths.rubrics_dir(), name, "yaml");
            let rubric = crate::rubric::load(&path)?;
            let target_path = ctx.resolve(file);
            let target = std::fs::read_to_string(&target_path)
                .map_err(|e| HarnessError::io(&target_path, e))?;
            let provider = default_provider(ctx)?;
            let report = crate::rubric::evaluate(&rubric, &target, provider.as_ref()).await?;
            Ok(SlashOutcome::Handled(report.to_markdown()))
        }
        _ => Ok(SlashOutcome::Handled(
            "использование: /rubric list | /rubric run <name> <file>".into(),
        )),
    }
}

/// `/bench list|run <name>`: список бенчмарков / прогон на модели по умолчанию.
async fn cmd_bench(rest: &str, ctx: &ToolContext) -> Result<SlashOutcome> {
    let parts: Vec<&str> = rest.split_whitespace().collect();
    match parts.as_slice() {
        ["list"] => {
            let dir = ctx.config.paths.benchmarks_dir();
            let items = crate::bench::list(&dir)?;
            if items.is_empty() {
                return Ok(SlashOutcome::Handled(format!(
                    "бенчмарков нет ({})",
                    dir.display()
                )));
            }
            let mut out = format!("Бенчмарки ({}):\n", dir.display());
            for b in &items {
                let _ = writeln!(out, "  {:<24} [{}] {}", b.name, b.tags.join(", "), b.description);
            }
            Ok(SlashOutcome::Handled(out))
        }
        ["run", name] => {
            let path = resolve_named(&ctx.config.paths.benchmarks_dir(), name, "yaml");
            let bench = crate::bench::load(&path)?;
            let provider = default_provider(ctx)?;
            let report = crate::bench::run(
                &bench,
                provider.as_ref(),
                &ctx.config.paths.rubrics_dir(),
                &ctx.config.paths.reports_dir,
            )
            .await?;
            let verdict = if report.passed { "PASS" } else { "FAIL" };
            Ok(SlashOutcome::Handled(format!(
                "бенчмарк '{name}': {verdict}, взвешенный балл {:.2} (порог {:.2}); \
                 отчёты — в {}",
                report.rubric_report.weighted_total,
                bench.pass_threshold,
                ctx.config.paths.reports_dir.display()
            )))
        }
        _ => Ok(SlashOutcome::Handled(
            "использование: /bench list | /bench run <name>".into(),
        )),
    }
}

/// `/kb <query>`: поиск по локальной базе знаний.
async fn cmd_kb(rest: &str, ctx: &ToolContext) -> Result<SlashOutcome> {
    if rest.is_empty() {
        return Ok(SlashOutcome::Handled("использование: /kb <query>".into()));
    }
    let hits = crate::kb::search(
        &ctx.config.knowledge.dirs,
        &ctx.config.knowledge.extensions,
        rest,
        10,
    )
    .await?;
    if hits.is_empty() {
        return Ok(SlashOutcome::Handled("ничего не найдено".into()));
    }
    let mut out = String::new();
    for h in &hits {
        let _ = writeln!(out, "{}:{} (score {:.1})", h.path.display(), h.line, h.score);
        let _ = writeln!(out, "{}", h.snippet);
    }
    Ok(SlashOutcome::Handled(out))
}

/// `/web <query>`: поиск по кураторским архитектурным сайтам.
async fn cmd_web(rest: &str, ctx: &ToolContext) -> Result<SlashOutcome> {
    if rest.is_empty() {
        return Ok(SlashOutcome::Handled("использование: /web <query>".into()));
    }
    let results = crate::web::search_arch_sites(rest, &[], &ctx.config.web).await?;
    if results.is_empty() {
        return Ok(SlashOutcome::Handled("ничего не найдено".into()));
    }
    let mut out = String::new();
    for r in &results {
        let _ = writeln!(out, "{}\n  {}\n  {}", r.title, r.url, r.snippet);
    }
    Ok(SlashOutcome::Handled(out))
}

/// `/fetch <url>`: загрузка страницы текстом.
async fn cmd_fetch(rest: &str, ctx: &ToolContext) -> Result<SlashOutcome> {
    if rest.is_empty() {
        return Ok(SlashOutcome::Handled("использование: /fetch <url>".into()));
    }
    let text = crate::web::fetch(rest, &ctx.config.web).await?;
    Ok(SlashOutcome::Handled(text))
}

/// `/sites`: кураторский список сайтов архитектора.
fn sites_text(ctx: &ToolContext) -> String {
    let mut out = String::from("Кураторские сайты:\n");
    for s in crate::web::curated_sites(&ctx.config.web) {
        let _ = writeln!(out, "  {:<18} {} — {}", s.name, s.domain, s.description);
        let _ = writeln!(out, "  {:<18} {}", "", s.base_url);
    }
    out
}

/// `/handoff <harness> <repo> [task...]`: генерация handoff-пакета.
fn cmd_handoff(rest: &str, ctx: &ToolContext) -> Result<SlashOutcome> {
    let mut it = rest.split_whitespace();
    let (Some(harness), Some(repo)) = (it.next(), it.next()) else {
        return Ok(SlashOutcome::Handled(
            "использование: /handoff <harness> <repo> [task...]".into(),
        ));
    };
    let task_parts: Vec<&str> = it.collect();
    let task = if task_parts.is_empty() {
        "См. TASK.md: реализуй согласно архитектуре".to_string()
    } else {
        task_parts.join(" ")
    };
    let known = crate::harness::known();
    if !known.contains(&harness) {
        return Ok(SlashOutcome::Handled(format!(
            "неизвестный харнесс '{harness}'; известные: {}",
            known.join(", ")
        )));
    }
    let packet = crate::harness::generate_handoff(&ctx.resolve(repo), &task, &[], &ctx.config)?;
    Ok(SlashOutcome::Handled(format!(
        "handoff-пакет для '{harness}': {} файлов в {} (epic-context ~{} токенов)",
        packet.files.len(),
        packet.dir.display(),
        packet.epic_context_tokens
    )))
}

/// `/control <repo>`: fitness functions из `.arch-handoff/CONSTRAINTS.yaml`.
fn cmd_control(rest: &str, ctx: &ToolContext) -> Result<SlashOutcome> {
    if rest.is_empty() {
        return Ok(SlashOutcome::Handled("использование: /control <repo>".into()));
    }
    let repo = ctx.resolve(rest);
    let report = crate::control::check(&repo, &repo.join(".arch-handoff/CONSTRAINTS.yaml"))?;
    let verdict = if report.passed { "PASS" } else { "FAIL" };
    let mut out = format!("контроль {}: {verdict}\n{}\n", repo.display(), report.summary);
    for i in &report.issues {
        let _ = writeln!(
            out,
            "  [{}] {}:{} {}: {}",
            i.severity,
            i.file.display(),
            i.line,
            i.rule,
            i.message
        );
    }
    Ok(SlashOutcome::Handled(out))
}

/// `/score k=v...`: Architecture Significance Score по 15 триггерам.
/// Без аргументов — справка по триггерам.
fn score_text(rest: &str) -> String {
    let mut answers = BTreeMap::new();
    for pair in rest.split_whitespace() {
        if let Some((k, v)) = pair.split_once('=') {
            let fired = matches!(v, "true" | "1" | "yes" | "да");
            answers.insert(k.to_string(), fired);
        }
    }
    let mut out = String::new();
    if !answers.is_empty() {
        let sig = crate::control::significance_score(&answers);
        let _ = writeln!(
            out,
            "Оценка значимости: {} → маршрут {:?}",
            sig.score, sig.route
        );
        if sig.fired.is_empty() {
            let _ = writeln!(out, "Сработавших триггеров нет.");
        } else {
            let _ = writeln!(out, "Сработали: {}", sig.fired.join(", "));
        }
        out.push('\n');
    } else {
        let _ = writeln!(out, "использование: /score <триггер>=true ...\n");
    }
    let _ = writeln!(out, "Триггеры значимости ({}):", crate::control::SIGNIFICANCE_TRIGGERS.len());
    for t in crate::control::SIGNIFICANCE_TRIGGERS {
        let _ = writeln!(out, "  {t}");
    }
    out
}

/// `/mcp list`: серверы из `mcp.json` + их инструменты. Сбои (нет файла,
/// сервер не поднялся) — не фатальны: текст ошибки в `Handled`.
async fn cmd_mcp(rest: &str, ctx: &ToolContext) -> Result<SlashOutcome> {
    if rest != "list" {
        return Ok(SlashOutcome::Handled("использование: /mcp list".into()));
    }
    let servers = match crate::mcp::load_servers(&ctx.config.mcp.servers_file) {
        Ok(s) => s,
        Err(e) => return Ok(SlashOutcome::Handled(format!("mcp: {e}"))),
    };
    if servers.is_empty() {
        return Ok(SlashOutcome::Handled("MCP-серверы не настроены".into()));
    }
    let manager = match crate::mcp::McpManager::connect(&servers, ctx.config.mcp.timeout_secs).await
    {
        Ok(m) => m,
        Err(e) => return Ok(SlashOutcome::Handled(format!("mcp: {e}"))),
    };
    let names = manager.server_names();
    let tools = manager.tools().await;
    manager.shutdown().await;
    let mut out = format!("MCP-серверы: {}\n", names.join(", "));
    if tools.is_empty() {
        let _ = writeln!(out, "инструментов не обнаружено");
    } else {
        let _ = writeln!(out, "Инструменты ({}):", tools.len());
        for t in &tools {
            let _ = writeln!(out, "  {:<32} {}", t.name, t.description);
        }
    }
    Ok(SlashOutcome::Handled(out))
}

/// `/save <file>`: транскрипт сессии в markdown (роль: текст).
fn cmd_save(rest: &str, session: &AgentSession, ctx: &ToolContext) -> Result<SlashOutcome> {
    if rest.is_empty() {
        return Ok(SlashOutcome::Handled("использование: /save <file>".into()));
    }
    let mut md = String::from("# Транскрипт сессии\n\n");
    for m in session.messages() {
        let _ = writeln!(md, "## {}\n", m.role);
        if !m.content.is_empty() {
            let _ = writeln!(md, "{}\n", m.content);
        }
        for call in &m.tool_calls {
            let _ = writeln!(md, "- вызов `{}`: `{}`", call.name, call.arguments);
        }
        if !m.tool_calls.is_empty() {
            md.push('\n');
        }
    }
    let path = ctx.resolve(rest);
    std::fs::write(&path, &md).map_err(|e| HarnessError::io(&path, e))?;
    Ok(SlashOutcome::Handled(format!(
        "транскрипт сохранён: {} ({} сообщений)",
        path.display(),
        session.messages().len()
    )))
}

/// `/load <file>`: содержимое файла (≤16К символов) включается в контекст
/// как user-сообщение — без обращения к модели.
fn cmd_load(rest: &str, session: &mut AgentSession, ctx: &ToolContext) -> Result<SlashOutcome> {
    if rest.is_empty() {
        return Ok(SlashOutcome::Handled("использование: /load <file>".into()));
    }
    let path = ctx.resolve(rest);
    let raw = std::fs::read_to_string(&path).map_err(|e| HarnessError::io(&path, e))?;
    let content = truncate_chars(&raw, 16_384);
    let n = content.chars().count();
    session.inject_context(rest, &content);
    Ok(SlashOutcome::Handled(format!("включено: {n} символов")))
}

/// Провайдер по умолчанию из реестра LLM контекста.
fn default_provider(ctx: &ToolContext) -> Result<std::sync::Arc<dyn crate::llm::LlmProvider>> {
    ctx.llm
        .as_ref()
        .ok_or_else(|| HarnessError::Config("реестр LLM недоступен в этом контексте".into()))
        .map(|r| r.default())
}

/// Резолвит именованный ассет: `dir/name` или `dir/name.<ext>`.
fn resolve_named(dir: &std::path::Path, name: &str, ext: &str) -> PathBuf {
    let direct = dir.join(name);
    if direct.is_file() {
        direct
    } else {
        dir.join(format!("{name}.{ext}"))
    }
}

/// Усекает текст до `max` символов (char-safe) с пометкой об усечении.
fn truncate_chars(text: &str, max: usize) -> String {
    let out: String = text.chars().take(max).collect();
    if out.len() < text.len() {
        format!("{out}\n… [усечено до {max} символов]")
    } else {
        out
    }
}


/// `/skills [query]`: поиск по библиотеке скиллов (без query — список).
fn cmd_skills(rest: &str, ctx: &ToolContext) -> Result<SlashOutcome> {
    let plugins = crate::plugin::discover(&ctx.config.plugins.dirs);
    let total: usize = plugins.iter().map(|p| p.skills.len()).sum();
    if rest.is_empty() {
        let mut out = format!("Скиллов: {total} в {} плагинах\n", plugins.len());
        for p in &plugins {
            for s in &p.skills {
                let desc = s.description.lines().next().unwrap_or("");
                let _ = writeln!(out, "  {:<28} {:<14} {}", s.name, p.manifest.name, desc.chars().take(70).collect::<String>());
            }
        }
        return Ok(SlashOutcome::Handled(out));
    }
    let hits = crate::plugin::search(&plugins, rest, 10);
    if hits.is_empty() {
        return Ok(SlashOutcome::Handled(format!(
            "ничего не найдено (скиллов в индексе: {total})"
        )));
    }
    let mut out = String::new();
    for h in &hits {
        let _ = writeln!(out, "{} [{}] (score {:.1})", h.meta.name, h.meta.plugin, h.score);
        let _ = writeln!(out, "  {}", h.meta.description.lines().next().unwrap_or("").chars().take(100).collect::<String>());
        if !h.snippet.is_empty() {
            let _ = writeln!(out, "{}", h.snippet);
        }
    }
    out.push_str("\n/skill <name> — загрузить скилл в контекст");
    Ok(SlashOutcome::Handled(out))
}

/// `/skill <name>`: загружает скилл и включает его в контекст сессии.
fn cmd_skill(rest: &str, session: &mut AgentSession, ctx: &ToolContext) -> Result<SlashOutcome> {
    if rest.is_empty() {
        return Ok(SlashOutcome::Handled("использование: /skill <name>".into()));
    }
    let plugins = crate::plugin::discover(&ctx.config.plugins.dirs);
    let Some(meta) = crate::plugin::skill_by_name(&plugins, rest) else {
        return Ok(SlashOutcome::Handled(format!(
            "скилл '{rest}' не найден (см. /skills)"
        )));
    };
    let body = crate::plugin::load_skill(meta)?;
    let chars = body.chars().count();
    session.inject_context(&format!("скилл: {}", meta.name), &body);
    Ok(SlashOutcome::Handled(format!(
        "скилл '{}' включён в контекст ({chars} символов)",
        meta.name
    )))
}

/// `/plugins`: список плагинов (скиллы + MCP).
fn cmd_plugins(rest: &str, ctx: &ToolContext) -> Result<SlashOutcome> {
    let plugins = crate::plugin::discover(&ctx.config.plugins.dirs);
    if let Some(name) = (!rest.is_empty()).then_some(rest) {
        let Some(p) = plugins.iter().find(|p| p.manifest.name == name) else {
            return Ok(SlashOutcome::Handled(format!("плагин '{name}' не найден")));
        };
        let mut out = format!(
            "{} v{} — {}\n{}\nСкиллы ({}):\n",
            p.manifest.name,
            p.manifest.version,
            p.manifest.description,
            p.dir.display(),
            p.skills.len()
        );
        for s in &p.skills {
            let _ = writeln!(out, "  {:<28} {}", s.name, s.description.lines().next().unwrap_or("").chars().take(70).collect::<String>());
        }
        let servers = crate::plugin::mcp_servers(std::slice::from_ref(p));
        if !servers.is_empty() {
            let _ = writeln!(out, "MCP-серверы ({}):", servers.len());
            for s in &servers {
                let _ = writeln!(out, "  {:<24} {} {}", s.name, s.command, s.args.join(" "));
            }
        }
        return Ok(SlashOutcome::Handled(out));
    }
    let mut out = format!("Плагины ({}):\n", plugins.len());
    for p in &plugins {
        let _ = writeln!(
            out,
            "  {:<24} скиллов: {:<3} {}",
            p.manifest.name,
            p.skills.len(),
            p.manifest.description.lines().next().unwrap_or("").chars().take(50).collect::<String>()
        );
    }
    Ok(SlashOutcome::Handled(out))
}


/// `/agents`: спецификации субагентов из плагинов + статусы фоновых задач.
fn cmd_agents(ctx: &ToolContext) -> String {
    let specs = crate::subagent::available_specs(&ctx.config.plugins.dirs);
    let mut out = String::new();
    if specs.is_empty() {
        out.push_str("Субагентов в плагинах нет (agents/*.md). Доступен встроенный 'general'.\n");
    } else {
        let _ = writeln!(out, "Субагенты ({} + general):", specs.len());
        for s in &specs {
            let tools = if s.tools.is_empty() {
                "все инструменты".to_string()
            } else {
                s.tools.join(", ")
            };
            let _ = writeln!(
                out,
                "  {:<28} {:<22} {}",
                s.name,
                s.plugin,
                s.description.lines().next().unwrap_or("").chars().take(60).collect::<String>()
            );
            let _ = writeln!(out, "    tools: {tools}");
        }
    }
    if let Some(registry) = &ctx.subagents {
        let tasks = registry.list();
        let running = tasks
            .iter()
            .filter(|t| t.status == crate::subagent::TaskStatus::Running)
            .count();
        let _ = writeln!(
            out,
            "\nФоновые задачи ({running} работает, {} всего):",
            tasks.len()
        );
        out.push_str(&crate::subagent::render_tasks(&tasks));
    }
    out.push_str("\nЗапуск — через агента: «запусти <имя> на <задачу>» (инструмент subagent_run).");
    out
}

/// `/distill <name> [plugin]`: дистиллирует транскрипт текущей сессии
/// в SKILL.md библиотеки скиллов (моделью, активной в чате).
async fn cmd_distill(rest: &str, session: &AgentSession, ctx: &ToolContext) -> Result<SlashOutcome> {
    let mut parts = rest.split_whitespace();
    let Some(name) = parts.next() else {
        return Ok(SlashOutcome::Handled(
            "использование: /distill <skill-name> [plugin] — дистиллировать контекст сессии в скилл библиотеки"
                .into(),
        ));
    };
    let plugin = parts.next().unwrap_or("");
    // Транскрипт собирается с ХВОСТА: свежий контекст ценнее старого.
    let mut transcript = String::new();
    for m in session.messages().iter().rev() {
        if m.content.trim().is_empty() {
            continue;
        }
        let chunk = format!(
            "[{}] {}\n\n",
            m.role.as_str(),
            m.content.chars().take(1500).collect::<String>()
        );
        if transcript.chars().count() + chunk.chars().count() > 24_000 {
            break;
        }
        transcript.insert_str(0, &chunk);
    }
    if transcript.trim().is_empty() {
        return Ok(SlashOutcome::Handled(
            "контекст сессии пуст — нечего дистиллировать".into(),
        ));
    }
    let Some(root) = ctx.config.plugins.dirs.first() else {
        return Ok(SlashOutcome::Handled(
            "не настроены каталоги плагинов ([plugins] dirs) — некуда писать скилл".into(),
        ));
    };
    let provider = session.provider();
    match crate::distill::distill_to_skill(&transcript, name, plugin, &provider, root).await {
        Ok(outcome) => Ok(SlashOutcome::Handled(format!(
            "скилл '{}' дистиллирован из контекста сессии → {} ({} символов)\n\
             Найдётся через /skills {} — загрузка в контекст: /skill {}",
            outcome.skill_name,
            outcome.path.display(),
            outcome.chars,
            outcome.skill_name,
            outcome.skill_name
        ))),
        Err(e) => Ok(SlashOutcome::Handled(format!("дистилляция не удалась: {e}"))),
    }
}


/// `/sessions`: список журналов прошлых сессий (новые первыми).
fn cmd_sessions(ctx: &ToolContext) -> Result<SlashOutcome> {
    let logs = crate::agent::list_session_logs(&ctx.config.paths.sessions_dir);
    if logs.is_empty() {
        return Ok(SlashOutcome::Handled(
            "журналов сессий нет (каталог пуст или ещё не создан)".into(),
        ));
    }
    let mut out = String::from("Сессии (новые первыми); /resume <имя|last> — восстановить:\n");
    for (i, l) in logs.iter().take(15).enumerate() {
        let name = l.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let _ = writeln!(
            out,
            "  {:<2} {:<26} {:<19} сообщений: {:<3} {}",
            i + 1,
            name,
            l.modified,
            l.messages,
            l.first_user_line
        );
    }
    Ok(SlashOutcome::Handled(out))
}

/// `/resume <file|last>`: восстанавливает историю из журнала прошлой сессии.
fn cmd_resume(rest: &str, session: &mut AgentSession, ctx: &ToolContext) -> Result<SlashOutcome> {
    if rest.is_empty() {
        return Ok(SlashOutcome::Handled(
            "использование: /resume <имя-файла|last> (список — /sessions)".into(),
        ));
    }
    let logs = crate::agent::list_session_logs(&ctx.config.paths.sessions_dir);
    let current = session.log_path().map(std::path::Path::to_path_buf);
    let target = if rest == "last" {
        // «last» — новейший журнал, КРОМЕ журнала текущей сессии.
        logs.iter().find(|l| Some(&l.path) != current.as_ref())
    } else {
        let want = if rest.ends_with(".jsonl") {
            rest.to_string()
        } else {
            format!("{rest}.jsonl")
        };
        logs.iter().find(|l| {
            l.path
                .file_name()
                .map(|n| n.to_string_lossy() == want)
                .unwrap_or(false)
        })
    };
    let Some(info) = target else {
        return Ok(SlashOutcome::Handled(format!(
            "журнал '{rest}' не найден (см. /sessions)"
        )));
    };
    let restored = session.restore_from_log(&info.path)?;
    Ok(SlashOutcome::Handled(format!(
        "сессия восстановлена из {}: {} сообщений ({}). Контекст продолжается с этого места.",
        info.path.display(),
        restored,
        info.first_user_line
    )))
}


/// `/worktree`: список изолированных worktree текущего репозитория.
async fn cmd_worktree_list(ctx: &ToolContext) -> Result<SlashOutcome> {
    match crate::worktree::list(&ctx.cwd).await {
        Ok(infos) => Ok(SlashOutcome::Handled(format!(
            "worktree в {}:\n{}",
            ctx.cwd.display(),
            crate::worktree::render_list(&infos)
        ))),
        Err(e) => Ok(SlashOutcome::Handled(format!("worktree: {e}"))),
    }
}

/// `/agentsmd <repo>`: refresh + lint AGENTS.md репозитория.
fn cmd_agentsmd(rest: &str, ctx: &ToolContext) -> Result<SlashOutcome> {
    if rest.is_empty() {
        return Ok(SlashOutcome::Handled(
            "использование: /agentsmd <repo> — генерация + проверка AGENTS.md".into(),
        ));
    }
    let repo = ctx.resolve(rest);
    let report = crate::agentsmd::generate(&repo)?;
    let mut out = format!(
        "AGENTS.md: {} ({}), инвариантов: {}\n",
        report.path.display(),
        report.action,
        report.invariants
    );
    let issues = crate::agentsmd::lint(&repo)?;
    if issues.is_empty() {
        out.push_str("линт: нарушений нет");
    } else {
        out.push_str("линт:\n");
        for i in &issues {
            let _ = writeln!(out, "  [{}] {} — {}", i.severity, i.rule, i.message);
        }
    }
    Ok(SlashOutcome::Handled(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::Arc;

    use crate::config::Config;
    use crate::llm::{ChatMessage, ChatRequest, LlmProvider};
    use crate::tool::ToolRegistry;

    /// Минимальный провайдер для конструирования сессии в тестах слэш-команд.
    #[derive(Debug)]
    struct StubLlm;

    #[async_trait::async_trait]
    impl LlmProvider for StubLlm {
        fn name(&self) -> &str {
            "stub"
        }
        fn model(&self) -> &str {
            "stub-1"
        }
        async fn complete(&self, _req: ChatRequest) -> Result<ChatMessage> {
            Ok(ChatMessage::assistant("ок", Vec::new()))
        }
    }

    /// Сессия + контекст в tempdir (журналы и ассеты — изолированы).
    fn make_fixture(dir: &Path) -> (AgentSession, ToolContext) {
        let mut cfg = Config::default();
        cfg.paths.sessions_dir = dir.join("sessions");
        cfg.paths.assets_dir = dir.join("assets");
        cfg.agent.stream = false;
        // Изоляция тестов: плагинные хуки реальной библиотеки не подхватываем.
        cfg.plugins.include_hooks = false;
        let config = Arc::new(cfg);
        let session = AgentSession::new(
            config.clone(),
            Arc::new(StubLlm),
            ToolRegistry::new(),
            ToolContext::new(dir.to_path_buf(), config.clone()),
            "sys".into(),
        );
        let ctx = ToolContext::new(dir.to_path_buf(), config);
        (session, ctx)
    }

    #[tokio::test]
    async fn new_starts_fresh_session_and_rotates_journal() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (mut s, ctx) = make_fixture(tmp.path());
        let old_log = s.log_path().map(|p| p.to_path_buf()).expect("журнал открыт");
        // Немного истории, чтобы было что очищать.
        s.send("привет", None).await.expect("ход заглушки");
        assert!(!s.messages().is_empty());

        match execute("/new", &mut s, &ctx).await.expect("ok") {
            SlashOutcome::NewSession => {}
            other => panic!("ожидался NewSession, получено {other:?}"),
        }
        assert!(s.messages().is_empty(), "история очищена");
        let new_log = s.log_path().map(|p| p.to_path_buf()).expect("новый журнал");
        assert_ne!(old_log, new_log, "журнал ротирован");
        assert!(old_log.is_file(), "старый журнал остаётся для /sessions");
        let old = std::fs::read_to_string(&old_log).expect("чтение старого журнала");
        assert!(old.contains("session_end"), "финальная отметка старого: {old}");
        let new = std::fs::read_to_string(&new_log).expect("чтение нового журнала");
        assert!(new.contains("\"system\""), "новый начат с системного: {new}");

        // Два /new подряд (та же секунда) — разные файлы, без перемешивания.
        execute("/new", &mut s, &ctx).await.expect("ok");
        let third = s.log_path().map(|p| p.to_path_buf()).expect("третий журнал");
        assert_ne!(new_log, third, "суффикс разводит коллизию одной секунды");
        assert!(third.is_file());
    }

    #[tokio::test]
    async fn not_slash_and_empty_input_pass_through() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (mut s, ctx) = make_fixture(tmp.path());
        assert!(matches!(
            execute("обычный вопрос", &mut s, &ctx).await.expect("ok"),
            SlashOutcome::NotSlash
        ));
        assert!(matches!(
            execute("   ", &mut s, &ctx).await.expect("ok"),
            SlashOutcome::NotSlash
        ));
        assert!(is_slash("  /help"));
        assert!(!is_slash("привет"));
        assert!(!is_slash(""));
    }

    #[tokio::test]
    async fn unknown_command_reports_name() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (mut s, ctx) = make_fixture(tmp.path());
        match execute("/bogus арг", &mut s, &ctx).await.expect("ok") {
            SlashOutcome::Unknown(name) => assert_eq!(name, "bogus"),
            other => panic!("ожидался Unknown, получено {other:?}"),
        }
    }

    #[tokio::test]
    async fn mermaid_on_directory_returns_hint_not_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (mut s, ctx) = make_fixture(tmp.path());
        let dir = tmp.path().join("diagrams");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("flow.mmd"), "flowchart LR\nA-->B\n").expect("mmd");
        let cmd = format!("/mermaid {}", dir.display());
        match execute(&cmd, &mut s, &ctx).await.expect("ok") {
            SlashOutcome::Handled(text) => {
                assert!(text.contains("каталог"), "подсказка: {text}");
                assert!(text.contains("flow.mmd"), "файл подсказан: {text}");
            }
            other => panic!("ожидался Handled, получено {other:?}"),
        }
    }

    /// Провайдер-заглушка с именем из `[models]` (для тестов `/think`).
    #[derive(Debug)]
    struct NamedStub;

    #[async_trait::async_trait]
    impl LlmProvider for NamedStub {
        fn name(&self) -> &str {
            "deepseek"
        }
        fn model(&self) -> &str {
            "deepseek-v4-flash"
        }
        async fn complete(&self, _req: ChatRequest) -> Result<ChatMessage> {
            Ok(ChatMessage::assistant("ок", Vec::new()))
        }
    }

    #[tokio::test]
    async fn think_without_configured_maps_is_refused_politely() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (mut s, ctx) = make_fixture(tmp.path());
        // Провайдер фикстуры называется 'stub' — такого ключа нет в [models].
        match execute("/think on", &mut s, &ctx).await.expect("ok") {
            SlashOutcome::Handled(text) => {
                assert!(text.contains("не настроена карта"), "отказ: {text}");
            }
            other => panic!("ожидался Handled, получено {other:?}"),
        }
        assert_eq!(s.thinking(), None, "состояние не изменилось");
    }

    #[tokio::test]
    async fn think_on_off_auto_roundtrip_for_configured_model() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut cfg = Config::default();
        cfg.paths.sessions_dir = tmp.path().join("sessions");
        let config = Arc::new(cfg);
        let mut s = AgentSession::new(
            config.clone(),
            Arc::new(NamedStub),
            ToolRegistry::new(),
            ToolContext::new(tmp.path().to_path_buf(), config.clone()),
            "sys".into(),
        );
        let ctx = ToolContext::new(tmp.path().to_path_buf(), config);
        // Статус по умолчанию: auto + видны обе карты.
        match execute("/think", &mut s, &ctx).await.expect("ok") {
            SlashOutcome::Handled(text) => {
                assert!(text.contains("auto"), "статус: {text}");
                assert!(text.contains("enabled") && text.contains("disabled"), "карты: {text}");
            }
            other => panic!("ожидался Handled, получено {other:?}"),
        }
        // on → Some(true) + карта enabled в отчёте.
        match execute("/think on", &mut s, &ctx).await.expect("ok") {
            SlashOutcome::Handled(text) => assert!(text.contains("\"enabled\""), "{text}"),
            other => panic!("ожидался Handled, получено {other:?}"),
        }
        assert_eq!(s.thinking(), Some(true));
        // off → Some(false); auto → None.
        let _ = execute("/think off", &mut s, &ctx).await.expect("ok");
        assert_eq!(s.thinking(), Some(false));
        let _ = execute("/think auto", &mut s, &ctx).await.expect("ok");
        assert_eq!(s.thinking(), None);
        // Мусорный аргумент — подсказка использования.
        match execute("/think maybe", &mut s, &ctx).await.expect("ok") {
            SlashOutcome::Handled(text) => assert!(text.contains("использование"), "{text}"),
            other => panic!("ожидался Handled, получено {other:?}"),
        }
    }

    #[tokio::test]
    async fn bare_model_returns_pick_model_when_registry_present() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (mut s, ctx) = make_fixture(tmp.path());
        let registry = Arc::new(
            crate::llm::LlmRegistry::from_config(&ctx.config).expect("реестр конструируется"),
        );
        let ctx = ctx.with_llm(registry);
        match execute("/model", &mut s, &ctx).await.expect("ok") {
            SlashOutcome::PickModel => {}
            other => panic!("ожидался PickModel, получено {other:?}"),
        }
    }

    #[tokio::test]
    async fn compact_command_reports_fold_stats() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (mut s, ctx) = make_fixture(tmp.path());
        // Пустая история — честное «нечего компактить».
        match execute("/compact", &mut s, &ctx).await.expect("ok") {
            SlashOutcome::Handled(text) => assert!(text.contains("нечего"), "{text}"),
            other => panic!("ожидался Handled, получено {other:?}"),
        }
        // С историей — свёртка и статистика.
        s.restore_from_log(&{
            let p = tmp.path().join("old-session-1.jsonl");
            std::fs::write(
                &p,
                "{\"kind\":\"user\",\"content\":\"первая задача про саги и платежи\"}\n\
                 {\"kind\":\"assistant\",\"content\":\"разбор вариантов саги: оркестрация vs хореография\"}\n\
                 {\"kind\":\"user\",\"content\":\"актуальная задача\"}\n",
            )
            .expect("write");
            p
        })
        .expect("restore");
        match execute("/compact", &mut s, &ctx).await.expect("ok") {
            SlashOutcome::Handled(text) => {
                assert!(text.contains("→"), "статистика: {text}");
                assert!(text.contains("свёрнуто"), "{text}");
            }
            other => panic!("ожидался Handled, получено {other:?}"),
        }
    }

    #[test]
    fn catalog_names_are_unique_and_slash_prefixed() {
        let cat = catalog();
        let mut names: Vec<&str> = cat.iter().map(|(n, _)| *n).collect();
        assert!(names.iter().all(|n| n.starts_with('/')));
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), cat.len(), "имена команд должны быть уникальны");
    }

    #[tokio::test]
    async fn help_lists_all_catalog_entries() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (mut s, ctx) = make_fixture(tmp.path());
        match execute("/help", &mut s, &ctx).await.expect("ok") {
            SlashOutcome::Handled(text) => {
                for (name, _) in catalog() {
                    assert!(text.contains(name), "в /help нет {name}");
                }
            }
            other => panic!("ожидался Handled, получено {other:?}"),
        }
    }

    #[tokio::test]
    async fn quit_clear_and_tools() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (mut s, ctx) = make_fixture(tmp.path());
        assert!(matches!(
            execute("/quit", &mut s, &ctx).await.expect("ok"),
            SlashOutcome::Quit
        ));

        s.inject_context("t", "c");
        assert_eq!(s.messages().len(), 1);
        match execute("/clear", &mut s, &ctx).await.expect("ok") {
            SlashOutcome::Handled(text) => assert!(text.contains("очищен")),
            other => panic!("ожидался Handled, получено {other:?}"),
        }
        assert!(s.messages().is_empty());

        match execute("/tools", &mut s, &ctx).await.expect("ok") {
            SlashOutcome::Handled(text) => assert!(text.contains("инструмент")),
            other => panic!("ожидался Handled, получено {other:?}"),
        }
    }

    #[tokio::test]
    async fn model_without_registry_is_reported() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (mut s, ctx) = make_fixture(tmp.path());
        match execute("/model", &mut s, &ctx).await.expect("ok") {
            SlashOutcome::Handled(text) => {
                assert!(text.contains("Текущая модель: stub-1"));
                assert!(text.contains("недоступен"));
            }
            other => panic!("ожидался Handled, получено {other:?}"),
        }
        // Переключение без реестра — ошибка подкоманды.
        assert!(execute("/model deepseek", &mut s, &ctx).await.is_err());
    }

    #[tokio::test]
    async fn prompts_list_and_show() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("assets/prompts");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("review.md"), "# Ревью\n\nТело ревью {{f}}").expect("write");

        let (mut s, ctx) = make_fixture(tmp.path());
        match execute("/prompts", &mut s, &ctx).await.expect("ok") {
            SlashOutcome::Handled(text) => {
                assert!(text.contains("review"));
                assert!(text.contains("Ревью"));
            }
            other => panic!("ожидался Handled, получено {other:?}"),
        }
        match execute("/prompts review", &mut s, &ctx).await.expect("ok") {
            SlashOutcome::Handled(text) => assert!(text.contains("Тело ревью")),
            other => panic!("ожидался Handled, получено {other:?}"),
        }
        match execute("/prompts нет", &mut s, &ctx).await.expect("ok") {
            SlashOutcome::Handled(text) => assert!(text.contains("не найден")),
            other => panic!("ожидался Handled, получено {other:?}"),
        }
    }

    #[tokio::test]
    async fn score_routes_by_answers_and_lists_triggers() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (mut s, ctx) = make_fixture(tmp.path());
        match execute("/score new_component=true new_vendor=true", &mut s, &ctx)
            .await
            .expect("ok")
        {
            SlashOutcome::Handled(text) => {
                assert!(text.contains("Standard"), "маршрут: {text}");
                assert!(text.contains("new_component"));
                assert!(text.contains("trust_zone_change"), "справка по триггерам");
            }
            other => panic!("ожидался Handled, получено {other:?}"),
        }
        match execute("/score", &mut s, &ctx).await.expect("ok") {
            SlashOutcome::Handled(text) => assert!(text.contains("использование")),
            other => panic!("ожидался Handled, получено {other:?}"),
        }
    }

    #[tokio::test]
    async fn save_writes_markdown_and_load_injects_context() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (mut s, ctx) = make_fixture(tmp.path());
        s.inject_context("spec.md", "текст спецификации");

        match execute("/save transcript.md", &mut s, &ctx).await.expect("ok") {
            SlashOutcome::Handled(text) => assert!(text.contains("1 сообщений")),
            other => panic!("ожидался Handled, получено {other:?}"),
        }
        let saved = std::fs::read_to_string(tmp.path().join("transcript.md")).expect("read");
        assert!(saved.contains("## user"));
        assert!(saved.contains("текст спецификации"));

        let before = s.messages().len();
        match execute("/load transcript.md", &mut s, &ctx).await.expect("ok") {
            SlashOutcome::Handled(text) => assert!(text.contains("включено:")),
            other => panic!("ожидался Handled, получено {other:?}"),
        }
        assert_eq!(s.messages().len(), before + 1);
        let last = &s.messages()[s.messages().len() - 1];
        assert_eq!(last.role, crate::llm::Role::User);
        assert!(last.content.contains("transcript.md"));
        assert!(last.content.contains("## user"));

        // Несуществующий файл — ошибка подкоманды.
        assert!(execute("/load ghost.md", &mut s, &ctx).await.is_err());
    }
}
