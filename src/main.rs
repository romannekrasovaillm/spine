//! Тонкая точка входа: парсинг аргументов → вызов lib → код возврата.

use std::io::{IsTerminal, Read, Write as _};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use arch_harness::agent::AgentSession;
use arch_harness::config::Config;
use arch_harness::llm::LlmRegistry;
use arch_harness::tool::ToolContext;

/// Доменный харнесс solution-архитектора.
#[derive(Parser)]
#[command(name = "arch", version, about, long_about = None)]
struct Cli {
    /// Путь к config.toml (иначе ./arch-harness.toml или ~/.config/arch-harness/config.toml).
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Интерактивный TUI (действие по умолчанию).
    Tui,
    /// Инициализация ~/.arch-harness: конфиг, ассеты, примеры.
    Init,
    /// Headless-прогон агента: `arch run "задача"` или `cat spec.md | arch run -`.
    Run {
        /// Промпт; `-` или отсутствие значения при пайпе — читать stdin.
        prompt: Option<String>,
        /// Модель (имя из [models]).
        #[arg(long)]
        model: Option<String>,
        /// Без стриминга (печатать только финальный ответ).
        #[arg(long)]
        no_stream: bool,
        /// Строгий headless-контракт (как `dsh --profile headless`):
        /// stdout — только финальный ответ ассистента, прогресс молчит;
        /// при успехе stderr пуст, при сбое — причина в stderr и exit 1.
        /// Для скриптов и пайпов: `arch run -q "…" > answer.md`.
        #[arg(long, short = 'q')]
        quiet: bool,
        /// Ризонинг-режим: on|off (в запросы сливается карта thinking_on/off
        /// из конфига модели; без флага — дефолт провайдера).
        #[arg(long, value_name = "on|off")]
        think: Option<String>,
    },
    /// Список настроенных моделей.
    Models,
    /// Библиотека промптов: список или показ шаблона.
    Prompts {
        /// Имя шаблона (без — список).
        name: Option<String>,
    },
    /// Рендер mermaid-файла в Unicode/ASCII-арт.
    Mermaid {
        /// Файл с диаграммой (`-` — stdin).
        file: String,
    },
    /// Рубрики архитектурного контроля.
    Rubric {
        #[command(subcommand)]
        cmd: RubricCmd,
    },
    /// Архитектурные бенчмарки.
    Bench {
        #[command(subcommand)]
        cmd: BenchCmd,
    },
    /// Поиск по локальной базе знаний.
    Kb {
        /// Запрос.
        query: String,
        /// Максимум результатов.
        #[arg(long, default_value_t = 8)]
        limit: usize,
    },
    /// Веб: поиск и фетч по архитектурным сайтам.
    Web {
        #[command(subcommand)]
        cmd: WebCmd,
    },
    /// MCP-серверы: список и вызовы.
    Mcp {
        #[command(subcommand)]
        cmd: McpCmd,
    },
    /// Сформировать handoff-пакет для кодового харнесса.
    Handoff {
        /// Имя харнесса (claude-code, qwen-code, openclaw, hermes, theseus, codewhale).
        harness: String,
        /// Путь к репозиторию.
        #[arg(long)]
        repo: PathBuf,
        /// Формулировка задачи.
        #[arg(long)]
        task: String,
        /// Файлы спек/спайна/ADR для включения.
        #[arg(long)]
        spec: Vec<PathBuf>,
    },
    /// Прогнать кодовый харнесс по handoff-пакету.
    HarnessRun {
        /// Имя харнесса.
        harness: String,
        /// Путь к репозиторию (с .arch-handoff/).
        #[arg(long)]
        repo: PathBuf,
        /// Задача (иначе — из .arch-handoff/TASK.md).
        #[arg(long)]
        task: Option<String>,
    },
    /// Список известных кодовых харнессов.
    Harnesses,
    /// Архитектурный контроль.
    Control {
        #[command(subcommand)]
        cmd: ControlCmd,
    },
    /// Библиотека скиллов: список, поиск, показ.
    Skills {
        #[command(subcommand)]
        cmd: SkillsCmd,
    },
    /// Плагины (скиллы + MCP в одном пакете).
    Plugins {
        #[command(subcommand)]
        cmd: PluginsCmd,
    },
    /// Политика автономии (R-уровни): показать/проверить класс риска команды.
    Policy {
        /// Проверить команду: как её классифицирует политика.
        #[arg(long)]
        check: Option<String>,
    },
    /// Evidence Bundle — аудиторский след как условие выпуска.
    Evidence {
        #[command(subcommand)]
        cmd: EvidenceCmd,
    },
    /// Операционные метрики харнесса (из журналов сессий и отчётов).
    Metrics,
    /// Диагностика окружения: ключи, каталоги, плагины, харнессы, MCP.
    Doctor,
    /// Экспорт журнала сессии в Word/Excel.
    Export {
        /// Формат: word (docx) или excel (xlsx).
        format: String,
        /// Путь к журналу сессии (session-*.jsonl).
        session: PathBuf,
        /// Куда писать файл (.docx/.xlsx).
        out: PathBuf,
    },
    /// Дельта-спецификации (propose → apply → archive).
    Delta {
        #[command(subcommand)]
        cmd: DeltaCmd,
    },
    /// AGENTS.md для репозиториев команд: генерация из архитектурных артефактов.
    AgentsMd {
        #[command(subcommand)]
        cmd: AgentsMdCmd,
    },
    /// Планировщик md-задач.
    Cron {
        #[command(subcommand)]
        cmd: CronCmd,
    },
    /// Worktree-фабрика: изоляция агентной работы в git worktree (review/accept/drop).
    Worktree {
        #[command(subcommand)]
        cmd: WorktreeCmd,
    },
}

/// Подкоманды `arch worktree`.
#[derive(Subcommand)]
enum WorktreeCmd {
    /// Создать изолированный worktree (ветка arch/<name>).
    New {
        /// Имя (kebab-case [a-z0-9-]).
        name: String,
        /// Репозиторий (по умолчанию — текущий каталог).
        #[arg(long)]
        repo: Option<PathBuf>,
        /// Базовая ветка/коммит (по умолчанию HEAD).
        #[arg(long)]
        base: Option<String>,
    },
    /// Список worktree фабрики.
    List {
        /// Репозиторий (по умолчанию — текущий каталог).
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// Diff ветки worktree против HEAD (review).
    Diff {
        /// Имя worktree.
        name: String,
        /// Репозиторий (по умолчанию — текущий каталог).
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// Принять: merge в текущую ветку + уборка worktree.
    Accept {
        /// Имя worktree.
        name: String,
        /// Репозиторий (по умолчанию — текущий каталог).
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// Удалить worktree без merge (только чистое).
    Drop {
        /// Имя worktree.
        name: String,
        /// Репозиторий (по умолчанию — текущий каталог).
        #[arg(long)]
        repo: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum SkillsCmd {
    /// Список всех скиллов библиотеки.
    List,
    /// Поиск по скиллам.
    Search {
        /// Запрос.
        query: String,
        /// Максимум результатов.
        #[arg(long, default_value_t = 8)]
        limit: usize,
    },
    /// Показать полный текст скилла.
    Show {
        /// Точное имя скилла.
        name: String,
    },
}

#[derive(Subcommand)]
enum PluginsCmd {
    /// Список плагинов.
    List,
    /// Подробности плагина (манифест, скиллы, MCP-серверы).
    Show {
        /// Имя плагина.
        name: String,
    },
}

#[derive(Subcommand)]
enum RubricCmd {
    /// Список якорных рубрик.
    List,
    /// Оценить файл по рубрике (LLM-судья).
    Run {
        /// Рубрика (имя файла в assets/rubrics или путь).
        rubric: String,
        /// Целевой документ (md/txt).
        target: PathBuf,
        /// Модель-судья.
        #[arg(long)]
        model: Option<String>,
        /// Сначала сгенерировать динамическую рубрику под предмет.
        #[arg(long)]
        dynamic_subject: Option<String>,
    },
}

#[derive(Subcommand)]
enum BenchCmd {
    /// Список бенчмарков.
    List,
    /// Прогнать бенчмарк.
    Run {
        /// Имя файла бенчмарка в assets/benchmarks (или путь).
        name: String,
        /// Испытуемая модель.
        #[arg(long)]
        model: Option<String>,
    },
}

#[derive(Subcommand)]
enum WebCmd {
    /// Поиск в вебе.
    Search {
        /// Запрос.
        query: String,
        /// Ограничить кураторскими архитектурными сайтами.
        #[arg(long)]
        arch: bool,
    },
    /// Загрузить страницу текстом.
    Fetch {
        /// URL.
        url: String,
    },
    /// Кураторский список сайтов архитектора.
    Sites,
}

#[derive(Subcommand)]
enum McpCmd {
    /// Список серверов и их инструментов.
    List,
    /// Вызвать MCP-инструмент.
    Call {
        /// Составное имя server__tool.
        name: String,
        /// Аргументы JSON.
        #[arg(default_value = "{}")]
        args: String,
    },
}

#[derive(Subcommand)]
enum ControlCmd {
    /// Fitness-контроль репозитория по CONSTRAINTS.yaml.
    Check {
        /// Репозиторий.
        repo: PathBuf,
        /// Файл ограничений (по умолчанию <repo>/.arch-handoff/CONSTRAINTS.yaml).
        #[arg(long)]
        constraints: Option<PathBuf>,
    },
    /// Линтер ARCHITECTURE-SPINE.md.
    Spine {
        /// Путь к spine-файлу.
        file: PathBuf,
    },
    /// Сенсоры спецификаций (required-sections, upstream-coverage).
    Sensors {
        /// Каталог спецификаций.
        dir: PathBuf,
    },
    /// Architecture Significance Score: `--trigger new_component=true ...`
    Score {
        /// Триггеры вида имя=true/false.
        #[arg(long)]
        trigger: Vec<String>,
    },
    /// Новый ADR.
    Adr {
        /// Заголовок решения.
        title: String,
        /// Каталог ADR (по умолчанию ./docs/adr).
        #[arg(long)]
        dir: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum EvidenceCmd {
    /// Собрать bundle (EVIDENCE.yaml) по каталогу изменения.
    Pack {
        /// Каталог изменения.
        dir: PathBuf,
        /// Маршрут: fast|standard|critical.
        #[arg(long, default_value = "standard")]
        route: String,
    },
    /// Проверить bundle: полнота + целостность хэшей.
    Verify {
        /// Каталог изменения.
        dir: PathBuf,
    },
}

#[derive(Subcommand)]
enum DeltaCmd {
    /// Новая дельта (каркас changes/<name>/DELTA.md).
    New {
        /// Имя изменения (kebab-case).
        name: String,
        /// Репозиторий (по умолчанию — текущий каталог).
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// Список дельт (предложенные/архивные).
    List {
        /// Репозиторий.
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// Валидация структуры дельты.
    Validate {
        /// Имя дельты.
        name: String,
        /// Репозиторий.
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// Архивировать дельту после apply (вливание в живую истину).
    Archive {
        /// Имя дельты.
        name: String,
        /// Репозиторий.
        #[arg(long)]
        repo: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum AgentsMdCmd {
    /// Сгенерировать или обновить AGENTS.md (рукописная зона сохраняется).
    Refresh {
        /// Репозиторий.
        repo: PathBuf,
    },
    /// Проверить AGENTS.md: свежесть (дрейф источников), ссылки, заглушки.
    Lint {
        /// Репозиторий.
        repo: PathBuf,
    },
    /// Прогнать линтер по реестру репозиториев (файл: путь на строку).
    LintAll {
        /// Файл реестра (по умолчанию ~/.arch-harness/repos.txt).
        #[arg(long)]
        registry: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum CronCmd {
    /// Список задач расписания.
    List,
    /// Запустить задачу по имени сейчас.
    Run {
        /// Имя задачи.
        name: String,
    },
    /// Проверить и запустить дюжные задачи (для системного cron).
    Tick,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let cfg = Arc::new(Config::load(cli.config.as_deref()).context("загрузка конфигурации")?);

    match cli.cmd {
        None | Some(Cmd::Tui) => arch_harness::tui::run(cfg).await?,
        Some(Cmd::Init) => cmd_init(&cfg)?,
        Some(Cmd::Run {
            prompt,
            model,
            no_stream,
            quiet,
            think,
        }) => cmd_run(&cfg, prompt, model, !no_stream && !quiet, think).await?,
        Some(Cmd::Models) => {
            let registry = LlmRegistry::from_config(&cfg)?;
            println!("Модели (по умолчанию: {}):", registry.default_name());
            for name in registry.names() {
                let p = registry.get(&name)?;
                println!("  {name:<20} {} ({})", p.model(), p.name());
            }
        }
        Some(Cmd::Prompts { name }) => cmd_prompts(&cfg, name)?,
        Some(Cmd::Mermaid { file }) => {
            // Каталог — понятная подсказка со списком *.mmd, а не «os error 21».
            let input = if file != "-" && std::path::Path::new(&file).is_dir() {
                arch_harness::mermaid::read_diagram_source(std::path::Path::new(&file))?
            } else {
                read_file_or_stdin(&file)?
            };
            let art = arch_harness::mermaid::render(&input)?;
            println!("{art}");
        }
        Some(Cmd::Rubric { cmd }) => cmd_rubric(&cfg, cmd).await?,
        Some(Cmd::Bench { cmd }) => cmd_bench(&cfg, cmd).await?,
        Some(Cmd::Kb { query, limit }) => {
            let hits = arch_harness::kb::search(
                &cfg.knowledge.dirs,
                &cfg.knowledge.extensions,
                &query,
                limit,
            )
            .await?;
            for hit in &hits {
                println!("── {}:{} (score {:.1})", hit.path.display(), hit.line, hit.score);
                println!("{}", hit.snippet);
            }
            if hits.is_empty() {
                println!("Ничего не найдено.");
            }
        }
        Some(Cmd::Web { cmd }) => cmd_web(&cfg, cmd).await?,
        Some(Cmd::Mcp { cmd }) => cmd_mcp(&cfg, cmd).await?,
        Some(Cmd::Handoff {
            harness,
            repo,
            task,
            spec,
        }) => {
            if !cfg.harnesses.contains_key(&harness) {
                anyhow::bail!(
                    "неизвестный харнесс '{harness}'. Известные: {:?}",
                    arch_harness::harness::known()
                );
            }
            let packet = arch_harness::harness::generate_handoff(&repo, &task, &spec, &cfg)?;
            println!("Handoff-пакет: {}", packet.dir.display());
            for f in &packet.files {
                println!("  {}", f.display());
            }
            println!("epic-context ≈ {} токенов", packet.epic_context_tokens);
        }
        Some(Cmd::HarnessRun {
            harness,
            repo,
            task,
        }) => {
            let hcfg = cfg
                .harnesses
                .get(&harness)
                .with_context(|| format!("харнесс '{harness}' не настроен"))?;
            let task_text = match task {
                Some(t) => t,
                None => std::fs::read_to_string(repo.join(".arch-handoff/TASK.md"))
                    .context("нет --task и не найден .arch-handoff/TASK.md")?,
            };
            let run = arch_harness::harness::run_harness(&harness, hcfg, &repo, &task_text).await?;
            println!("── stdout (exit {:?}, {:.1}s) ──", run.exit_code, run.duration_secs);
            println!("{}", run.stdout);
            if !run.stderr.is_empty() {
                eprintln!("── stderr ──\n{}", run.stderr);
            }
        }
        Some(Cmd::Harnesses) => {
            println!("Известные кодовые харнессы:");
            for name in arch_harness::harness::known() {
                let status = match cfg.harnesses.get(name) {
                    Some(h) => format!("{} ({:?})", h.binary, h.prompt_mode),
                    None => "не настроен".into(),
                };
                let installed = which(&cfg.harnesses.get(name).map(|h| h.binary.as_str()).unwrap_or(name));
                println!("  {name:<14} {status:<40} {installed}");
            }
        }
        Some(Cmd::Control { cmd }) => cmd_control(cmd)?,
        Some(Cmd::Skills { cmd }) => cmd_skills(&cfg, cmd)?,
        Some(Cmd::Plugins { cmd }) => cmd_plugins(&cfg, cmd)?,
        Some(Cmd::Policy { check }) => cmd_policy(&cfg, check)?,
        Some(Cmd::Evidence { cmd }) => cmd_evidence(cmd)?,
        Some(Cmd::Metrics) => {
            let mut m = arch_harness::metrics::collect(&cfg.paths.sessions_dir, &cfg.paths.reports_dir)?;
            // Architecture drift по реестру AGENTS.md (repos.txt), если он ведётся.
            let registry = arch_harness::config::Config::home_dir().join("repos.txt");
            if registry.is_file() {
                if let Ok(report) = arch_harness::agentsmd::lint_registry(&registry) {
                    m.agentsmd_total = report.len();
                    m.agentsmd_stale = report
                        .iter()
                        .filter(|(_, issues)| {
                            issues.iter().any(|i| {
                                i.rule.contains("stale") || i.severity == "error"
                            })
                        })
                        .count();
                }
            }
            println!("{}", m.to_markdown());
        }
        Some(Cmd::Doctor) => {
            let checks = arch_harness::doctor::run_checks(&cfg);
            print!("{}", arch_harness::doctor::render(&checks));
            if arch_harness::doctor::exit_code(&checks) != 0 {
                std::process::exit(1);
            }
        }
        Some(Cmd::Export {
            format,
            session,
            out,
        }) => {
            let Some(fmt) = arch_harness::export::ExportFormat::parse(&format) else {
                return Err(anyhow::anyhow!(
                    "неизвестный формат «{format}» (ожидалось word|excel)"
                ));
            };
            let n = arch_harness::export::export_journal(&session, fmt, &out)?;
            println!("экспортировано {n} строк → {}", out.display());
        }
        Some(Cmd::Delta { cmd }) => cmd_delta(cmd)?,
        Some(Cmd::AgentsMd { cmd }) => cmd_agents_md(&cfg, cmd)?,
        Some(Cmd::Cron { cmd }) => cmd_cron(&cfg, cmd).await?,
        Some(Cmd::Worktree { cmd }) => cmd_worktree(&cfg, cmd).await?,
    }
    Ok(())
}

/// `arch worktree`: изоляция агентной работы (создание, review, accept, drop).
async fn cmd_worktree(cfg: &Arc<Config>, cmd: WorktreeCmd) -> Result<()> {
    let cwd = std::env::current_dir().context("cwd")?;
    let repo_of = |repo: Option<PathBuf>| repo.unwrap_or_else(|| cwd.clone());
    match cmd {
        WorktreeCmd::New { name, repo, base } => {
            let path = arch_harness::worktree::create(cfg, &repo_of(repo), &name, base.as_deref()).await?;
            println!("worktree создан: {}", path.display());
            println!("review: arch worktree diff {name} · accept: arch worktree accept {name} · drop: arch worktree drop {name}");
        }
        WorktreeCmd::List { repo } => {
            let infos = arch_harness::worktree::list(&repo_of(repo)).await?;
            print!("{}", arch_harness::worktree::render_list(&infos));
        }
        WorktreeCmd::Diff { name, repo } => {
            println!("{}", arch_harness::worktree::diff(&repo_of(repo), &name).await?);
        }
        WorktreeCmd::Accept { name, repo } => {
            println!("{}", arch_harness::worktree::accept(cfg, &repo_of(repo), &name).await?);
        }
        WorktreeCmd::Drop { name, repo } => {
            println!("{}", arch_harness::worktree::drop(cfg, &repo_of(repo), &name).await?);
        }
    }
    Ok(())
}

/// `arch init`: конфиг + ассеты в ~/.arch-harness.
fn cmd_init(cfg: &Config) -> Result<()> {
    let home = Config::home_dir();
    std::fs::create_dir_all(&home).context("создание домашнего каталога")?;
    let written = arch_harness::assets::write_defaults(&home)?;
    let cfg_path = cfg.save_default()?;
    println!("Инициализация завершена:");
    println!("  конфиг:  {}", cfg_path.display());
    println!("  домашний каталог: {}", home.display());
    for f in &written {
        println!("  ассет:   {}", f.display());
    }
    Ok(())
}

/// `arch run`: headless агент.
///
/// Строгий режим (`--quiet`, как `dsh --profile headless` у DeepSeek
/// Harness) = без стриминга: stdout несёт ТОЛЬКО финальный ответ ассистента
/// (пригоден для пайпов), события хода молчат; пустая задача отклоняется
/// до запуска; сбой — причина в stderr и ненулевой код выхода.
async fn cmd_run(
    cfg: &Arc<Config>,
    prompt: Option<String>,
    model: Option<String>,
    stream: bool,
    think: Option<String>,
) -> Result<()> {
    let input = match prompt.as_deref() {
        Some("-") | None if !std::io::stdin().is_terminal() => {
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf).context("чтение stdin")?;
            buf
        }
        Some(p) => p.to_string(),
        None => anyhow::bail!("нет промпта: передайте аргумент или пайп в stdin"),
    };
    if input.trim().is_empty() {
        anyhow::bail!("пустая задача: передайте непустой промпт аргументом или пайпом в stdin");
    }
    let thinking = match think.as_deref() {
        Some("on") => Some(true),
        Some("off") => Some(false),
        Some(other) => anyhow::bail!("--think: ожидается on|off, получено '{other}'"),
        None => None,
    };

    let registry = Arc::new(LlmRegistry::from_config(cfg)?);
    let provider = match &model {
        Some(name) => registry.get(name)?,
        None => registry.default(),
    };
    let tools = arch_harness::tools::full_registry(cfg);
    let cwd = std::env::current_dir().context("cwd")?;
    let tool_ctx = ToolContext::new(cwd, cfg.clone())
        .with_llm(registry.clone())
        .with_provider(provider.clone())
        .with_subagents(arch_harness::subagent::SubagentRegistry::new());
    let system = default_system_prompt(cfg);
    let mut session = AgentSession::new(cfg.clone(), provider, tools, tool_ctx, system);
    session.set_thinking(thinking);

    let reply = if stream {
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let printer = tokio::spawn(async move {
            use arch_harness::agent::AgentEvent;
            while let Some(ev) = rx.recv().await {
                // StdoutLock не Send — лочим на каждое событие, не через await.
                let mut out = std::io::stdout().lock();
                match ev {
                    AgentEvent::Delta(text) => {
                        let _ = out.write_all(text.as_bytes());
                        let _ = out.flush();
                    }
                    AgentEvent::ToolStart { name, .. } => {
                        let _ = writeln!(out, "\n\x1b[2m▶ tool: {name}\x1b[0m");
                    }
                    AgentEvent::ToolEnd {
                        name,
                        is_error,
                        summary,
                        ..
                    } => {
                        let mark = if is_error { "✗" } else { "✓" };
                        let _ = writeln!(out, "\x1b[2m{mark} {name}: {summary}\x1b[0m");
                    }
                    AgentEvent::Note(text) => {
                        let _ = writeln!(out, "\x1b[2m» {text}\x1b[0m");
                    }
                    AgentEvent::TurnDone => {
                        let _ = writeln!(out);
                    }
                }
            }
        });
        let r = session.send(&input, Some(tx)).await?;
        let _ = printer.await;
        r
    } else {
        session.send(&input, None).await?
    };
    if !stream {
        // Печать через writeln с игнорированием BrokenPipe: `arch run -q … | head`
        // обрывает stdout — для пайпа это норма, а не повод для паники println!.
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{reply}");
        let _ = out.flush();
    }
    Ok(())
}

/// Системный промпт по умолчанию: из библиотеки промптов или встроенный.
fn default_system_prompt(cfg: &Config) -> String {
    let dir = cfg.paths.prompts_dir();
    if let Ok(lib) = arch_harness::agent::prompts::load_library(&dir) {
        if let Some(tpl) = lib.iter().find(|t| t.name == "architect") {
            return tpl.body.clone();
        }
    }
    "Ты — solution-архитектор в корпоративном контуре банка. Помогаешь проектировать \
     решения, ведёшь ADR и architecture-spine, оцениваешь архитектуру по рубрикам, \
     готовишь handoff-пакеты кодовым агентам. Отвечай по-русски, точно и по делу."
        .into()
}

/// `arch prompts`.
fn cmd_prompts(cfg: &Config, name: Option<String>) -> Result<()> {
    let lib = arch_harness::agent::prompts::load_library(&cfg.paths.prompts_dir())?;
    match name {
        None => {
            println!("Библиотека промптов ({}):", cfg.paths.prompts_dir().display());
            for tpl in &lib {
                println!("  {:<24} {}", tpl.name, tpl.description);
            }
        }
        Some(n) => {
            let tpl = lib
                .iter()
                .find(|t| t.name == n)
                .with_context(|| format!("шаблон '{n}' не найден"))?;
            println!("{}", tpl.body);
        }
    }
    Ok(())
}

async fn cmd_rubric(cfg: &Arc<Config>, cmd: RubricCmd) -> Result<()> {
    match cmd {
        RubricCmd::List => {
            let list = arch_harness::rubric::list(&cfg.paths.rubrics_dir())?;
            for r in &list {
                println!("  {:<32} {} ({} критериев)", r.name, r.description, r.criteria_count);
            }
        }
        RubricCmd::Run {
            rubric,
            target,
            model,
            dynamic_subject,
        } => {
            let registry = Arc::new(LlmRegistry::from_config(cfg)?);
            let judge = match &model {
                Some(name) => registry.get(name)?,
                None => registry.default(),
            };
            let text = std::fs::read_to_string(&target)
                .with_context(|| format!("чтение {}", target.display()))?;
            let rub = match dynamic_subject {
                Some(subject) => {
                    let anchor_path = resolve_asset(&cfg.paths.rubrics_dir(), &rubric, "yaml");
                    let anchor = arch_harness::rubric::load(&anchor_path).ok();
                    arch_harness::rubric::generate_dynamic(&subject, anchor.as_ref(), judge.as_ref())
                        .await?
                }
                None => {
                    let path = resolve_asset(&cfg.paths.rubrics_dir(), &rubric, "yaml");
                    arch_harness::rubric::load(&path)?
                }
            };
            let report = arch_harness::rubric::evaluate(&rub, &text, judge.as_ref()).await?;
            println!("{}", report.to_markdown());
            let out = cfg
                .paths
                .reports_dir
                .join(format!("rubric-{}-{}.md", rub.name, timestamp()));
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            std::fs::write(&out, report.to_markdown())?;
            eprintln!("Отчёт: {}", out.display());
        }
    }
    Ok(())
}

async fn cmd_bench(cfg: &Arc<Config>, cmd: BenchCmd) -> Result<()> {
    match cmd {
        BenchCmd::List => {
            for b in arch_harness::bench::list(&cfg.paths.benchmarks_dir())? {
                println!("  {:<32} {} [{}]", b.name, b.description, b.tags.join(", "));
            }
        }
        BenchCmd::Run { name, model } => {
            let path = resolve_asset(&cfg.paths.benchmarks_dir(), &name, "yaml");
            let bench = arch_harness::bench::load(&path)?;
            let registry = LlmRegistry::from_config(cfg)?;
            let provider = match &model {
                Some(m) => registry.get(m)?,
                None => registry.default(),
            };
            let report =
                arch_harness::bench::run(&bench, provider.as_ref(), &cfg.paths.rubrics_dir(), &cfg.paths.reports_dir)
                    .await?;
            println!(
                "Бенчмарк '{}': {:.2} (порог {:.2}) — {}",
                report.bench_name,
                report.rubric_report.weighted_total,
                bench.pass_threshold,
                if report.passed { "PASS" } else { "FAIL" }
            );
        }
    }
    Ok(())
}

async fn cmd_web(cfg: &Config, cmd: WebCmd) -> Result<()> {
    match cmd {
        WebCmd::Search { query, arch } => {
            let results = if arch {
                arch_harness::web::search_arch_sites(&query, &[], &cfg.web).await?
            } else {
                arch_harness::web::search(&query, &cfg.web).await?
            };
            for r in &results {
                println!("• {}\n  {}\n  {}\n", r.title, r.url, r.snippet);
            }
            if results.is_empty() {
                println!("Ничего не найдено.");
            }
        }
        WebCmd::Fetch { url } => {
            let text = arch_harness::web::fetch(&url, &cfg.web).await?;
            println!("{text}");
        }
        WebCmd::Sites => {
            println!("Кураторские сайты архитектора:");
            for s in arch_harness::web::curated_sites(&cfg.web) {
                println!("  {:<16} {:<40} {}", s.name, s.base_url, s.description);
            }
        }
    }
    Ok(())
}

async fn cmd_mcp(cfg: &Config, cmd: McpCmd) -> Result<()> {
    let mut servers = arch_harness::mcp::load_servers(&cfg.mcp.servers_file)
        .with_context(|| format!("чтение {}", cfg.mcp.servers_file.display()))?;
    // Плагины тоже несут MCP-серверы (стандарт: plugin.json mcpServers / .mcp.json).
    if cfg.plugins.include_mcp {
        let plugins = arch_harness::plugin::discover(&cfg.plugins.dirs);
        servers.extend(arch_harness::plugin::mcp_servers(&plugins));
    }
    let manager = Arc::new(arch_harness::mcp::McpManager::connect(&servers, cfg.mcp.timeout_secs).await?);
    match cmd {
        McpCmd::List => {
            println!("Серверы: {}", manager.server_names().join(", "));
            for spec in manager.tools().await {
                println!("  {:<40} {}", spec.name, spec.description);
            }
        }
        McpCmd::Call { name, args } => {
            let args: serde_json::Value = serde_json::from_str(&args).context("невалидный JSON аргументов")?;
            let out = manager.call(&name, args).await?;
            println!("{}", out.content);
        }
    }
    manager.shutdown().await;
    Ok(())
}

fn cmd_control(cmd: ControlCmd) -> Result<()> {
    match cmd {
        ControlCmd::Check { repo, constraints } => {
            let c = constraints.unwrap_or_else(|| repo.join(".arch-handoff/CONSTRAINTS.yaml"));
            let report = arch_harness::control::check(&repo, &c)?;
            println!("{}", report.summary);
            for i in &report.issues {
                println!("  [{}] {}:{} {} — {}", i.severity, i.file.display(), i.line, i.rule, i.message);
            }
            println!("Итог: {}", if report.passed { "PASS" } else { "FAIL" });
            if !report.passed {
                std::process::exit(1);
            }
        }
        ControlCmd::Spine { file } => {
            let issues = arch_harness::control::lint_spine(&file)?;
            if issues.is_empty() {
                println!("spine: нарушений нет");
            }
            for i in &issues {
                println!("[{}] {}:{} {} — {}", i.severity, i.file.display(), i.line, i.rule, i.message);
            }
        }
        ControlCmd::Sensors { dir } => {
            for r in arch_harness::control::sensors_check(&dir)? {
                println!(
                    "  [{}] {} {} — {}",
                    if r.passed { "PASS" } else { "FAIL" },
                    r.sensor,
                    r.file.display(),
                    r.details
                );
            }
        }
        ControlCmd::Score { trigger } => {
            let mut answers = std::collections::BTreeMap::new();
            for t in &trigger {
                let (k, v) = t
                    .split_once('=')
                    .with_context(|| format!("триггер '{t}' не вида имя=true"))?;
                answers.insert(k.to_string(), v == "true");
            }
            let s = arch_harness::control::significance_score(&answers);
            println!(
                "Score: {} ({} триггеров) → маршрут {:?}",
                s.score,
                s.fired.join(", "),
                s.route
            );
        }
        ControlCmd::Adr { title, dir } => {
            let dir = dir.unwrap_or_else(|| PathBuf::from("docs/adr"));
            let path = arch_harness::control::adr_new(&dir, &title)?;
            println!("ADR создан: {}", path.display());
        }
    }
    Ok(())
}

/// `arch skills`: библиотека скиллов.
fn cmd_skills(cfg: &Config, cmd: SkillsCmd) -> Result<()> {
    let plugins = arch_harness::plugin::discover(&cfg.plugins.dirs);
    match cmd {
        SkillsCmd::List => {
            let total: usize = plugins.iter().map(|p| p.skills.len()).sum();
            println!("Скиллов: {total} в {} плагинах", plugins.len());
            for p in &plugins {
                for s in &p.skills {
                    println!("  {:<28} {:<14} {}", s.name, p.manifest.name, first_line(&s.description, 80));
                }
            }
        }
        SkillsCmd::Search { query, limit } => {
            let hits = arch_harness::plugin::search(&plugins, &query, limit);
            if hits.is_empty() {
                println!("Ничего не найдено (скиллов в индексе: {}).", plugins.iter().map(|p| p.skills.len()).sum::<usize>());
            }
            for h in &hits {
                println!("── {} [{}] (score {:.1})", h.meta.name, h.meta.plugin, h.score);
                println!("   {}", first_line(&h.meta.description, 100));
                if !h.snippet.is_empty() {
                    println!("{}", h.snippet);
                }
            }
        }
        SkillsCmd::Show { name } => {
            let meta = arch_harness::plugin::skill_by_name(&plugins, &name)
                .with_context(|| format!("скилл '{name}' не найден (см. `arch skills list`)"))?;
            println!("{}", arch_harness::plugin::load_skill(meta)?);
        }
    }
    Ok(())
}

/// `arch plugins`: пакеты скиллов + MCP.
fn cmd_plugins(cfg: &Config, cmd: PluginsCmd) -> Result<()> {
    let plugins = arch_harness::plugin::discover(&cfg.plugins.dirs);
    match cmd {
        PluginsCmd::List => {
            println!("Плагины ({}):", plugins.len());
            for p in &plugins {
                let mcp_count = if cfg.plugins.include_mcp {
                    arch_harness::plugin::mcp_servers(std::slice::from_ref(p)).len()
                } else {
                    0
                };
                println!(
                    "  {:<24} v{:<8} скиллов: {:<3} mcp: {:<2} {}",
                    p.manifest.name,
                    p.manifest.version,
                    p.skills.len(),
                    mcp_count,
                    first_line(&p.manifest.description, 60)
                );
            }
        }
        PluginsCmd::Show { name } => {
            let p = plugins
                .iter()
                .find(|p| p.manifest.name == name)
                .with_context(|| format!("плагин '{name}' не найден"))?;
            println!("{} v{} — {}", p.manifest.name, p.manifest.version, p.manifest.description);
            println!("Каталог: {}", p.dir.display());
            if !p.manifest.keywords.is_empty() {
                println!("Ключевые слова: {}", p.manifest.keywords.join(", "));
            }
            println!("Скиллы ({}):", p.skills.len());
            for s in &p.skills {
                println!("  {:<28} {}", s.name, first_line(&s.description, 70));
            }
            let servers = arch_harness::plugin::mcp_servers(std::slice::from_ref(p));
            if !servers.is_empty() {
                println!("MCP-серверы ({}):", servers.len());
                for s in &servers {
                    println!("  {:<24} {} {}", s.name, s.command, s.args.join(" "));
                }
            }
        }
    }
    Ok(())
}

/// Первая строка текста, усечённая до `max` символов.
fn first_line(text: &str, max: usize) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    let cut: String = line.chars().take(max).collect();
    if line.chars().count() > max {
        format!("{cut}…")
    } else {
        cut
    }
}

/// `arch policy`: уровень автономии и классификация команды.
fn cmd_policy(cfg: &Config, check: Option<String>) -> Result<()> {
    let policy = arch_harness::policy::Policy::parse(&cfg.policy.autonomy)?;
    match check {
        None => {
            println!("Уровень автономии: R{} (из config [policy] autonomy)", policy.level);
            println!("  R0 — только чтения авто; R2 — + изменения (дефолт); R4 — деструктив с подтверждением; R5 — полная (красный флаг аудита)");
        }
        Some(cmd) => {
            use arch_harness::policy::{PolicyDecision, classify_bash};
            let class = classify_bash(&cmd);
            let decision = policy.check("bash", &serde_json::json!({"command": cmd}));
            let verdict = match &decision {
                PolicyDecision::Allow => "ALLOW",
                PolicyDecision::RequireConfirm(_) => "REQUIRE-CONFIRM",
                PolicyDecision::Deny(_) => "DENY",
            };
            println!("команда: {cmd}\nкласс риска: {class:?}\nрешение (R{}): {verdict}", policy.level);
            match &decision {
                PolicyDecision::RequireConfirm(m) | PolicyDecision::Deny(m) => println!("причина: {m}"),
                _ => {}
            }
        }
    }
    Ok(())
}

/// `arch evidence`: Evidence Bundle.
fn cmd_evidence(cmd: EvidenceCmd) -> Result<()> {
    match cmd {
        EvidenceCmd::Pack { dir, route } => {
            let route = match route.to_lowercase().as_str() {
                "fast" => arch_harness::control::Route::Fast,
                "critical" => arch_harness::control::Route::Critical,
                _ => arch_harness::control::Route::Standard,
            };
            let (bundle, verdict) = arch_harness::evidence::pack(&dir, route)?;
            println!("{}", verdict.summary);
            for item in &bundle.items {
                println!("  + {:<20} {} ({} б)", item.key, item.path, item.size);
            }
            for miss in &verdict.missing {
                println!("  ✗ ОТСУТСТВУЕТ: {miss}");
            }
            println!("Манифест: {}", dir.join("EVIDENCE.yaml").display());
            if !verdict.passed {
                std::process::exit(1);
            }
        }
        EvidenceCmd::Verify { dir } => {
            let v = arch_harness::evidence::verify(&dir)?;
            println!("{}", v.summary);
            for m in &v.missing {
                println!("  ✗ ОТСУТСТВУЕТ: {m}");
            }
            for t in &v.tampered {
                println!("  ✗ ИЗМЕНЁН: {t}");
            }
            println!("Итог: {}", if v.passed { "PASS — выпуск разрешён" } else { "FAIL — выпуск заблокирован" });
            if !v.passed {
                std::process::exit(1);
            }
        }
    }
    Ok(())
}

/// `arch delta`: дельта-спецификации.
fn cmd_delta(cmd: DeltaCmd) -> Result<()> {
    let cwd = || std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    match cmd {
        DeltaCmd::New { name, repo } => {
            let path = arch_harness::delta::new(&repo.unwrap_or_else(cwd), &name)?;
            println!("Дельта создана: {}", path.display());
        }
        DeltaCmd::List { repo } => {
            let list = arch_harness::delta::list(&repo.unwrap_or_else(cwd));
            if list.is_empty() {
                println!("Дельт нет (changes/ пуст или отсутствует).");
            }
            for d in &list {
                println!("  {:<30} {:?}", d.name, d.status);
            }
        }
        DeltaCmd::Validate { name, repo } => {
            let issues = arch_harness::delta::validate(&repo.unwrap_or_else(cwd), &name)?;
            if issues.is_empty() {
                println!("дельта '{name}': нарушений нет");
            }
            let mut failed = false;
            for i in &issues {
                println!("[{}] {}:{} {} — {}", i.severity, i.file.display(), i.line, i.rule, i.message);
                failed |= i.severity == "error";
            }
            if failed {
                std::process::exit(1);
            }
        }
        DeltaCmd::Archive { name, repo } => {
            let path = arch_harness::delta::archive(&repo.unwrap_or_else(cwd), &name)?;
            println!("Дельта заархивирована: {}", path.display());
        }
    }
    Ok(())
}

/// `arch agents-md`: AGENTS.md как канал архитектурного контроля.
fn cmd_agents_md(cfg: &Config, cmd: AgentsMdCmd) -> Result<()> {
    match cmd {
        AgentsMdCmd::Refresh { repo } => {
            let report = arch_harness::agentsmd::generate(&repo)?;
            println!(
                "AGENTS.md: {} ({}) — инвариантов: {}, fitness: {}",
                report.path.display(),
                report.action,
                report.invariants,
                if report.has_constraints { "да" } else { "нет" }
            );
        }
        AgentsMdCmd::Lint { repo } => {
            let issues = arch_harness::agentsmd::lint(&repo)?;
            if issues.is_empty() {
                println!("AGENTS.md свежий, нарушений нет");
            }
            let mut failed = false;
            for i in &issues {
                println!("[{}] {}:{} {} — {}", i.severity, i.file.display(), i.line, i.rule, i.message);
                failed |= i.severity == "error";
            }
            if failed {
                std::process::exit(1);
            }
        }
        AgentsMdCmd::LintAll { registry } => {
            let registry = registry.unwrap_or_else(|| Config::home_dir().join("repos.txt"));
            let results = arch_harness::agentsmd::lint_registry(&registry)?;
            let mut failed = false;
            for (repo, issues) in &results {
                let errors = issues.iter().filter(|i| i.severity == "error").count();
                let status = if issues.is_empty() {
                    "OK".to_string()
                } else {
                    format!("{} проблем ({} error)", issues.len(), errors)
                };
                println!("{:<50} {}", repo.display(), status);
                failed |= errors > 0;
            }
            let _ = cfg;
            if failed {
                std::process::exit(1);
            }
        }
    }
    Ok(())
}

async fn cmd_cron(cfg: &Arc<Config>, cmd: CronCmd) -> Result<()> {
    let tab = arch_harness::cron::load(&cfg.cron.file)?;
    match cmd {
        CronCmd::List => {
            for j in &tab.jobs {
                println!("  {:<24} {:<16} {}", j.name, j.schedule, j.task_md.display());
            }
        }
        CronCmd::Run { name } => {
            let job = tab
                .jobs
                .iter()
                .find(|j| j.name == name)
                .with_context(|| format!("задача '{name}' не найдена"))?;
            let registry = LlmRegistry::from_config(cfg)?;
            let provider = match &job.model {
                Some(m) => registry.get(m)?,
                None => registry.default(),
            };
            let tools = arch_harness::tools::full_registry(cfg);
            let out_dir = job
                .out
                .clone()
                .unwrap_or_else(|| cfg.paths.reports_dir.join("cron"));
            let path = arch_harness::cron::run_job(job, provider.as_ref(), &tools, &out_dir).await?;
            println!("Отчёт: {}", path.display());
        }
        CronCmd::Tick => {
            // «Дюжные» задачи между прошлым тиком и сейчас; метка — в state-файле.
            let state_file = Config::home_dir().join("cron-last-tick");
            let now = chrono::Local::now();
            let last = std::fs::read_to_string(&state_file)
                .ok()
                .and_then(|s| {
                    chrono::DateTime::parse_from_rfc3339(s.trim())
                        .ok()
                        .map(|dt| dt.with_timezone(&chrono::Local))
                })
                .unwrap_or_else(|| now - chrono::Duration::hours(24));
            let registry = LlmRegistry::from_config(cfg)?;
            let provider = registry.default();
            let tools = arch_harness::tools::full_registry(cfg);
            let reports_dir = cfg
                .cron
                .out_dir
                .clone()
                .unwrap_or_else(|| cfg.paths.reports_dir.join("cron"));
            let reports = arch_harness::cron::run_due(
                &tab,
                last,
                now,
                provider.as_ref(),
                &tools,
                &reports_dir,
            )
            .await?;
            std::fs::write(&state_file, now.to_rfc3339()).context("запись метки тика")?;
            if reports.is_empty() {
                println!("Дюжных задач нет.");
            }
            for path in &reports {
                println!("Отчёт: {}", path.display());
            }
        }
    }
    Ok(())
}

/// Читает файл или stdin (`-`).
fn read_file_or_stdin(file: &str) -> Result<String> {
    if file == "-" {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf).context("чтение stdin")?;
        Ok(buf)
    } else {
        std::fs::read_to_string(file).with_context(|| format!("чтение {file}"))
    }
}

/// Резолвит имя ассета: точный путь, либо `<dir>/<name>`, либо `<dir>/<name>.<ext>`.
fn resolve_asset(dir: &std::path::Path, name: &str, ext: &str) -> PathBuf {
    let as_path = PathBuf::from(name);
    if as_path.is_file() {
        return as_path;
    }
    let in_dir = dir.join(name);
    if in_dir.is_file() {
        return in_dir;
    }
    dir.join(format!("{name}.{ext}"))
}

/// Есть ли бинарь в PATH.
fn which(binary: &str) -> String {
    std::process::Command::new("which")
        .arg(binary)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "MISSING".into())
}

/// Метка времени для имён отчётов.
fn timestamp() -> String {
    chrono::Local::now().format("%Y%m%d-%H%M%S").to_string()
}
