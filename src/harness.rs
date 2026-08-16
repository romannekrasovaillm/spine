//! Адаптеры кодовых харнессов и handoff-пакеты (передача архитектуры в код).
//!
//! КОНТРАКТ (владелец: агент `harness`):
//! - известные харнессы: claude-code, qwen-code, openclaw, hermes, theseus,
//!   codewhale ([`known`]); конфиги — из `Config::harnesses`;
//! - [`generate_handoff`] — каталог `<repo>/.arch-handoff/`: TASK.md (задача),
//!   ARCHITECTURE.md (свод спек/спайна), adr/ (копии ADR), CONSTRAINTS.yaml
//!   (fitness-правила), RUBRIC.yaml (якорная рубрика приёмки), MANIFEST.json
//!   (мета: дата, модель, источники) + компактный epic-context (800–1500
//!   токенов, по смыслу);
//! - [`run_harness`] — запуск бинаря харнесса (PromptMode positional/flag/stdin)
//!   в каталоге repo, таймаут, захват stdout/stderr → HarnessRun;
//! - [`tools`] — инструменты `handoff_create` и `harness_run` для агентного
//!   цикла (прогон пакета харнессом — только через `harness_run`, не bash).

use std::fmt::Write as _;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::config::{CodingHarnessConfig, Config, PromptMode};
use crate::error::{HarnessError, Result};
use crate::llm::ToolSpec;
use crate::tool::{Tool, ToolContext, ToolOutput};

/// Имя каталога handoff-пакета в корне репозитория.
const HANDOFF_DIR: &str = ".arch-handoff";

/// Максимальный размер epic-context (ARCHITECTURE.md), символов
/// (~1500 токенов при грубой оценке 4 символа ≈ 1 токен).
const EPIC_CONTEXT_MAX_CHARS: usize = 6000;

/// Дефолтный набор fitness-правил (схема `control::check`):
/// пишется только при отсутствии пользовательского CONSTRAINTS.yaml.
const DEFAULT_CONSTRAINTS: &str = "\
# Fitness-правила для `arch control check` (схема control::check).
# Создано генератором handoff; при повторной генерации файл НЕ затирается.
rules:
  - name: no-unwrap-in-src
    type: must_not_contain
    glob: \"src/**\"
    pattern: 'unwrap\\('
    severity: warn
  - name: no-dbg-macro
    type: must_not_contain
    glob: \"src/**\"
    pattern: 'dbg!'
    severity: error
  - name: readme-exists
    type: file_exists
    path: README.md
    severity: warn
  - name: cargo-check-passes
    type: command_succeeds
    command: 'cargo check'
    timeout_secs: 120
    severity: error
";

/// Имена известных кодовых харнессов.
pub fn known() -> Vec<&'static str> {
    vec![
        "claude-code",
        "qwen-code",
        "openclaw",
        "hermes",
        "theseus",
        "codewhale",
    ]
}

/// Итог генерации handoff-пакета.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffPacket {
    /// Каталог `.arch-handoff/`.
    pub dir: PathBuf,
    /// Файлы пакета (включая сохранённые пользовательские CONSTRAINTS.yaml/RUBRIC.yaml).
    pub files: Vec<PathBuf>,
    /// Оценка размера epic-context в токенах.
    pub epic_context_tokens: usize,
}

/// Метаданные пакета (`MANIFEST.json`).
#[derive(Serialize)]
struct Manifest<'a> {
    /// Дата создания, ISO 8601 (UTC).
    created_at: String,
    /// Формулировка задачи.
    task: &'a str,
    /// Модель по умолчанию из конфига.
    model: &'a str,
    /// Файлы-источники спецификаций.
    sources: Vec<String>,
    /// Размер epic-context, символов.
    epic_context_chars: usize,
    /// Оценка размера epic-context, токенов (~chars/4).
    epic_context_tokens: usize,
}

/// Генерирует handoff-пакет в репозиторий.
///
/// Создаёт `<repo>/.arch-handoff/` с TASK.md, ARCHITECTURE.md, MANIFEST.json,
/// adr/ (копии ADR) и, при отсутствии, CONSTRAINTS.yaml и RUBRIC.yaml.
/// Перезаписываются только TASK.md, ARCHITECTURE.md и MANIFEST.json —
/// пользовательские правки CONSTRAINTS.yaml/RUBRIC.yaml сохраняются.
///
/// # Errors
/// Репозиторий недоступен, спека не читается, ошибка записи.
pub fn generate_handoff(
    repo: &Path,
    task: &str,
    spec_files: &[PathBuf],
    cfg: &Config,
) -> Result<HandoffPacket> {
    if !repo.is_dir() {
        return Err(HarnessError::Harness(format!(
            "репозиторий недоступен: {}",
            repo.display()
        )));
    }
    let dir = repo.join(HANDOFF_DIR);
    let adr_dir = dir.join("adr");
    std::fs::create_dir_all(&adr_dir).map_err(|e| HarnessError::io(&adr_dir, e))?;

    // TASK.md — всегда перезаписывается (задача новая на каждый прогон).
    let task_path = dir.join("TASK.md");
    std::fs::write(&task_path, render_task_md(task)).map_err(|e| HarnessError::io(&task_path, e))?;

    // ARCHITECTURE.md — всегда перезаписывается (компиляция актуальных спек).
    let arch_md = compile_epic_context(spec_files)?;
    let arch_path = dir.join("ARCHITECTURE.md");
    std::fs::write(&arch_path, &arch_md).map_err(|e| HarnessError::io(&arch_path, e))?;
    let epic_chars = arch_md.chars().count();
    let epic_tokens = epic_chars / 4;

    // CONSTRAINTS.yaml — только при отсутствии (не затирать пользовательские правила).
    let constraints_path = dir.join("CONSTRAINTS.yaml");
    if !constraints_path.exists() {
        std::fs::write(&constraints_path, DEFAULT_CONSTRAINTS)
            .map_err(|e| HarnessError::io(&constraints_path, e))?;
    }

    // RUBRIC.yaml — только при отсутствии и только если есть якорная рубрика.
    let rubric_path = dir.join("RUBRIC.yaml");
    if !rubric_path.exists() {
        let anchor = cfg.paths.rubrics_dir().join("handoff_quality.yaml");
        if anchor.is_file() {
            std::fs::copy(&anchor, &rubric_path).map_err(|e| HarnessError::io(&rubric_path, e))?;
        }
    }

    // adr/ — копии ADR-файлов; существующие копии не затираем.
    let mut adr_copies = Vec::new();
    for spec in spec_files {
        if is_adr_file(spec) {
            let Some(name) = spec.file_name() else { continue };
            let dest = adr_dir.join(name);
            if !dest.exists() {
                std::fs::copy(spec, &dest).map_err(|e| HarnessError::io(&dest, e))?;
            }
            adr_copies.push(dest);
        }
    }

    // MANIFEST.json — всегда перезаписывается.
    let manifest = Manifest {
        created_at: Utc::now().to_rfc3339(),
        task,
        model: &cfg.default_model,
        sources: spec_files
            .iter()
            .map(|p| p.display().to_string())
            .collect(),
        epic_context_chars: epic_chars,
        epic_context_tokens: epic_tokens,
    };
    let manifest_path = dir.join("MANIFEST.json");
    let manifest_text = serde_json::to_string_pretty(&manifest)?;
    std::fs::write(&manifest_path, format!("{manifest_text}\n"))
        .map_err(|e| HarnessError::io(&manifest_path, e))?;

    let mut files = vec![task_path, arch_path, manifest_path];
    if constraints_path.exists() {
        files.push(constraints_path);
    }
    if rubric_path.exists() {
        files.push(rubric_path);
    }
    files.extend(adr_copies);

    Ok(HandoffPacket {
        dir,
        files,
        epic_context_tokens: epic_tokens,
    })
}

/// Рендерит TASK.md: задача + контракт результата (headless JSON-статус).
fn render_task_md(task: &str) -> String {
    let mut s = String::with_capacity(task.len() + 1024);
    s.push_str("# Задача для кодового харнесса\n\n");
    s.push_str(task.trim());
    s.push_str("\n\n## Контракт результата\n\n");
    s.push_str("Финальный ответ обязан завершаться JSON-объектом (после него — ни символа):\n\n");
    s.push_str("```json\n{\"status\": \"complete|partial|blocked\", \"assumptions\": [], \"open_questions\": [], \"conflicts_with_prior_decisions\": []}\n```\n\n");
    s.push_str("- `status`: `complete` — выполнено полностью; `partial` — частично; `blocked` — заблокировано.\n");
    s.push_str("- `assumptions`: допущения, принятые при реализации.\n");
    s.push_str("- `open_questions`: вопросы к архитектору.\n");
    s.push_str(
        "- `conflicts_with_prior_decisions`: расхождения с принятыми ранее решениями (ADR, spine).\n\n",
    );
    s.push_str("Архитектурный контекст — `ARCHITECTURE.md`, ограничения — `CONSTRAINTS.yaml`, рубрика приёмки — `RUBRIC.yaml` (при наличии).\n");
    s
}

/// Компилирует epic-context из спецификаций: заголовок с датой и источниками,
/// далее — сжатые рендеры спек; итог усечён до [`EPIC_CONTEXT_MAX_CHARS`].
///
/// # Errors
/// Спека не читается.
fn compile_epic_context(spec_files: &[PathBuf]) -> Result<String> {
    let mut out = String::with_capacity(EPIC_CONTEXT_MAX_CHARS);
    out.push_str("# Архитектурный контекст (epic-context)\n\n");
    out.push_str(&format!("Собран: {}\n\n", Utc::now().to_rfc3339()));
    out.push_str("Источники:\n");
    for f in spec_files {
        out.push_str(&format!("- {}\n", f.display()));
    }
    out.push('\n');
    for f in spec_files {
        let text = std::fs::read_to_string(f).map_err(|e| HarnessError::io(f, e))?;
        out.push_str(&format!("<!-- источник: {} -->\n\n", f.display()));
        out.push_str(render_spec(&text).trim_end());
        out.push_str("\n\n");
    }
    if out.chars().count() > EPIC_CONTEXT_MAX_CHARS {
        let notice = "\n\n> **Контекст усечён** до 6000 символов; полные тексты — в файлах-источниках (см. MANIFEST.json).\n";
        let keep = EPIC_CONTEXT_MAX_CHARS.saturating_sub(notice.chars().count());
        let truncated: String = out.chars().take(keep).collect();
        out = truncated;
        out.push_str(notice);
    }
    Ok(out)
}

/// Рендерит одну спецификацию: секции с полями Binds/Prevents/Rule (ADR-блоки
/// spine) — целиком, прочие секции — заголовок + первые два абзаца.
fn render_spec(text: &str) -> String {
    let mut preamble = String::new();
    let mut sections: Vec<(String, String)> = Vec::new();
    let mut cur: Option<(String, String)> = None;
    let mut in_fence = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
        }
        if !in_fence && line.starts_with('#') {
            if let Some(s) = cur.take() {
                sections.push(s);
            }
            cur = Some((line.trim_end().to_string(), String::new()));
        } else if let Some((_, body)) = cur.as_mut() {
            body.push_str(line);
            body.push('\n');
        } else {
            preamble.push_str(line);
            preamble.push('\n');
        }
    }
    if let Some(s) = cur.take() {
        sections.push(s);
    }

    let mut out = String::new();
    if !preamble.trim().is_empty() {
        out.push_str(&first_paragraphs(&preamble, 2));
        out.push_str("\n\n");
    }
    for (heading, body) in &sections {
        out.push_str(heading);
        out.push_str("\n\n");
        if is_adr_block(body) {
            out.push_str(body.trim());
        } else {
            out.push_str(&first_paragraphs(body, 2));
        }
        out.push_str("\n\n");
    }
    out
}

/// Признак ADR-блока spine: секция содержит поля Binds/Prevents/Rule.
fn is_adr_block(body: &str) -> bool {
    ["Binds:", "Prevents:", "Rule:"]
        .iter()
        .any(|m| body.contains(m))
}

/// Первые `n` абзацев текста (абзацы разделены пустыми строками).
fn first_paragraphs(text: &str, n: usize) -> String {
    let mut paras: Vec<String> = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            if !cur.is_empty() {
                paras.push(cur.join("\n"));
                cur.clear();
                if paras.len() >= n {
                    break;
                }
            }
        } else {
            cur.push(line);
        }
    }
    if paras.len() < n && !cur.is_empty() {
        paras.push(cur.join("\n"));
    }
    paras.join("\n\n")
}

/// Признак ADR-файла: md, чьё имя содержит `ADR` или путь содержит `/adr/`.
fn is_adr_file(path: &Path) -> bool {
    if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("md")) {
        return false;
    }
    let name_hit = path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.contains("ADR"));
    let path_hit = path.to_string_lossy().contains("/adr/");
    name_hit || path_hit
}

/// Итог прогона кодового харнесса.
#[derive(Debug, Clone)]
pub struct HarnessRun {
    /// Имя харнесса.
    pub harness: String,
    /// Код возврата.
    pub exit_code: Option<i32>,
    /// stdout.
    pub stdout: String,
    /// stderr.
    pub stderr: String,
    /// Длительность, секунды.
    pub duration_secs: f64,
}

/// Собирает argv и данные для stdin по режиму [`PromptMode`]:
/// - Positional: `args + [task]`;
/// - Flag: подстановка `{prompt}` в args, иначе `args + [task]`;
/// - Stdin: `args`, задача уходит в stdin.
fn build_argv(cfg: &CodingHarnessConfig, task: &str) -> (Vec<String>, Option<String>) {
    match cfg.prompt_mode {
        PromptMode::Positional => {
            let mut argv = cfg.args.clone();
            argv.push(task.into());
            (argv, None)
        }
        PromptMode::Flag => {
            if cfg.args.iter().any(|a| a.contains("{prompt}")) {
                (
                    cfg.args.iter().map(|a| a.replace("{prompt}", task)).collect(),
                    None,
                )
            } else {
                let mut argv = cfg.args.clone();
                argv.push(task.into());
                (argv, None)
            }
        }
        PromptMode::Stdin => (cfg.args.clone(), Some(task.into())),
    }
}

/// Запускает кодовый харнесс с задачей в репозитории.
///
/// Бинарь запускается с `cwd = repo` и переменными окружения из `cfg.env`;
/// при [`PromptMode::Stdin`] задача пишется в stdin. Прогон ограничен
/// `cfg.timeout_secs`; по таймауту процесс убивается.
///
/// # Errors
/// Бинарь не найден (с подсказкой по установке/конфигу), таймаут, ошибка запуска.
pub async fn run_harness(
    name: &str,
    cfg: &CodingHarnessConfig,
    repo: &Path,
    task: &str,
) -> Result<HarnessRun> {
    let (argv, stdin_data) = build_argv(cfg, task);
    let mut cmd = Command::new(&cfg.binary);
    cmd.args(&argv)
        .current_dir(repo)
        .envs(&cfg.env)
        // kill_on_drop: при отмене по таймауту dropped Child уничтожит процесс.
        .kill_on_drop(true)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if stdin_data.is_some() {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }

    let started = Instant::now();
    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == ErrorKind::NotFound {
            HarnessError::Harness(format!(
                "бинарь '{}' не найден: установите {} или поправьте config.toml [harnesses.{name}]",
                cfg.binary, cfg.binary
            ))
        } else {
            HarnessError::Harness(format!("не удалось запустить '{}': {e}", cfg.binary))
        }
    })?;

    // Пишем задачу в stdin отдельной задачей, чтобы не было дедлока на
    // заполненном pipe-буфере, пока читаются stdout/stderr.
    let writer = match (child.stdin.take(), stdin_data) {
        (Some(mut pipe), Some(data)) => Some(tokio::spawn(async move {
            // Ошибка записи осознанно игнорируется: процесс вправе закрыть stdin раньше.
            let _ = pipe.write_all(data.as_bytes()).await;
            // drop(pipe) закрывает stdin — EOF для процесса.
        })),
        _ => None,
    };

    let wait = tokio::time::timeout(
        Duration::from_secs(cfg.timeout_secs),
        child.wait_with_output(),
    )
    .await;
    if let Some(w) = &writer {
        w.abort();
    }
    let output = match wait {
        Ok(res) => res.map_err(|e| {
            HarnessError::Harness(format!("сбой ожидания '{}': {e}", cfg.binary))
        })?,
        Err(_) => {
            return Err(HarnessError::Harness(format!(
                "таймаут {} с: харнесс '{name}' ('{}') не завершился",
                cfg.timeout_secs, cfg.binary
            )));
        }
    };

    Ok(HarnessRun {
        harness: name.into(),
        exit_code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        duration_secs: started.elapsed().as_secs_f64(),
    })
}

/// Инструмент `handoff_create`: генерация handoff-пакета из агентного цикла.
struct HandoffCreateTool {
    /// Конфигурация (пути к рубрикам, модель по умолчанию).
    cfg: Config,
}

#[async_trait]
impl Tool for HandoffCreateTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "handoff_create".into(),
            description: "Сгенерировать handoff-пакет (.arch-handoff/: TASK.md, ARCHITECTURE.md, CONSTRAINTS.yaml, MANIFEST.json, adr/) для передачи задачи кодовому харнессу".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "repo": {"type": "string", "description": "Корень репозитория (относительно cwd или абсолютный)"},
                    "task": {"type": "string", "description": "Формулировка задачи для кодового харнесса"},
                    "spec": {"type": "array", "items": {"type": "string"}, "description": "Пути к спецификациям/ADR (md), опционально"}
                },
                "required": ["repo", "task"]
            }),
        }
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let Some(repo) = args.get("repo").and_then(Value::as_str) else {
            return Ok(ToolOutput::err(
                "handoff_create: обязательный аргумент 'repo' (string) отсутствует",
            ));
        };
        let Some(task) = args.get("task").and_then(Value::as_str) else {
            return Ok(ToolOutput::err(
                "handoff_create: обязательный аргумент 'task' (string) отсутствует",
            ));
        };
        let spec: Vec<PathBuf> = args
            .get("spec")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(|s| ctx.resolve(s))
                    .collect()
            })
            .unwrap_or_default();
        let repo = ctx.resolve(repo);
        match generate_handoff(&repo, task, &spec, &self.cfg) {
            Ok(packet) => {
                let files = packet
                    .files
                    .iter()
                    .map(|f| format!("- {}", f.display()))
                    .collect::<Vec<_>>()
                    .join("\n");
                Ok(ToolOutput::ok(format!(
                    "Handoff-пакет создан: {}\nEpic-context: ~{} токенов.\nФайлы:\n{files}",
                    packet.dir.display(),
                    packet.epic_context_tokens
                )))
            }
            Err(e) => Ok(ToolOutput::err(format!("handoff_create: {e}"))),
        }
    }
}

/// Лимит вывода `harness_run` (stdout + stderr), символов.
const HARNESS_RUN_MAX_CHARS: usize = 24 * 1024;

/// Вытаскивает JSON-контракт результата из stdout харнесса:
/// последний ```json-блок с полем `status` (см. [`render_task_md`]).
fn extract_contract(stdout: &str) -> Option<Value> {
    let start = stdout.rfind("```json")? + "```json".len();
    let rest = &stdout[start..];
    let end = rest.find("```")?;
    let v: Value = serde_json::from_str(rest[..end].trim()).ok()?;
    v.get("status").and_then(Value::as_str)?;
    Some(v)
}

/// Инструмент `harness_run`: прогон handoff-пакета (или явной задачи)
/// кодовым харнессом — без импровизации через bash (квотинг, permission-
/// промпты, короткие таймауты bash — частые точки отказа такой импровизации).
struct HarnessRunTool {
    /// Конфигурация (адаптеры харнессов).
    cfg: Config,
}

#[async_trait]
impl Tool for HarnessRunTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "harness_run".into(),
            description: "Запустить кодовый харнесс на репозитории и вернуть его вывод. \
                Обычно следует за handoff_create: задача читается из \
                <repo>/.arch-handoff/TASK.md (или передаётся явно). Запуск идёт через \
                настроенный адаптер [harnesses.<имя>] (режим prompt, env, таймаут 30 мин): \
                stdout/stderr и код возврата захватываются, JSON-контракт результата \
                (status/assumptions/open_questions) извлекается в сводку. НЕ запускать \
                харнесс через bash — там промпт ломается о квотинг, таймаут слишком \
                короткий, а env-scrub прячет от команды переменные *_KEY/*_TOKEN, \
                через которые харнесс может авторизовываться."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "harness": {
                        "type": "string",
                        "description": "Имя харнесса: claude-code, qwen-code, openclaw, hermes, theseus, codewhale"
                    },
                    "repo": {
                        "type": "string",
                        "description": "Корень репозитория (относительно cwd или абсолютный)"
                    },
                    "task": {
                        "type": "string",
                        "description": "Явная задача; если не задана — читается <repo>/.arch-handoff/TASK.md"
                    },
                    "timeout_secs": {
                        "type": "integer",
                        "description": "Переопределить таймаут адаптера, секунды (максимум 7200)",
                        "minimum": 1,
                        "maximum": 7200
                    }
                },
                "required": ["harness", "repo"]
            }),
        }
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let Some(name) = args.get("harness").and_then(Value::as_str) else {
            return Ok(ToolOutput::err(
                "harness_run: обязательный аргумент 'harness' (string) отсутствует",
            ));
        };
        let Some(repo) = args.get("repo").and_then(Value::as_str) else {
            return Ok(ToolOutput::err(
                "harness_run: обязательный аргумент 'repo' (string) отсутствует",
            ));
        };
        let Some(hcfg) = self.cfg.harnesses.get(name) else {
            return Ok(ToolOutput::err(format!(
                "harness_run: харнесс '{name}' не настроен. Известные: {}; \
                 адаптеры — в config.toml [harnesses.<имя>]",
                known().join(", ")
            )));
        };
        let repo = ctx.resolve(repo);
        let task = match args.get("task").and_then(Value::as_str) {
            Some(t) => t.to_string(),
            None => {
                let path = repo.join(HANDOFF_DIR).join("TASK.md");
                match std::fs::read_to_string(&path) {
                    Ok(t) => t,
                    Err(e) => {
                        return Ok(ToolOutput::err(format!(
                            "harness_run: нет аргумента 'task' и не читается {}: {e}. \
                             Сначала handoff_create или передайте task явно",
                            path.display()
                        )));
                    }
                }
            }
        };
        // Переопределение таймаута — копией конфига адаптера.
        let mut hcfg = hcfg.clone();
        if let Some(t) = args.get("timeout_secs").and_then(Value::as_u64) {
            hcfg.timeout_secs = t.clamp(1, 7200);
        }
        match run_harness(name, &hcfg, &repo, &task).await {
            Ok(run) => {
                let code = run.exit_code.map_or("сигнал".into(), |c| c.to_string());
                let mut content = format!(
                    "Харнесс '{name}' завершился: код {code}, {:.1} с.\n",
                    run.duration_secs
                );
                match extract_contract(&run.stdout) {
                    Some(v) => {
                        let count = |key: &str| {
                            v.get(key).and_then(Value::as_array).map_or(0, Vec::len)
                        };
                        let _ = writeln!(
                            content,
                            "Контракт результата: status={}; assumptions: {}; \
                             open_questions: {}; conflicts: {}.",
                            v["status"].as_str().unwrap_or("?"),
                            count("assumptions"),
                            count("open_questions"),
                            count("conflicts_with_prior_decisions"),
                        );
                    }
                    None => {
                        content.push_str(
                            "ВНИМАНИЕ: JSON-контракт результата (```json с полем status) \
                             в stdout не найден — ответ может быть неполным; при необходимости \
                             перезапустите с напоминанием о контракте.\n",
                        );
                    }
                }
                content.push_str("--- stdout ---\n");
                content.push_str(run.stdout.trim_end());
                if !run.stderr.trim().is_empty() {
                    content.push_str("\n--- stderr ---\n");
                    content.push_str(run.stderr.trim_end());
                }
                let is_error = run.exit_code != Some(0);
                Ok(ToolOutput {
                    content,
                    is_error,
                }
                .truncated(HARNESS_RUN_MAX_CHARS))
            }
            Err(e) => Ok(ToolOutput::err(format!("harness_run: {e}"))),
        }
    }
}

/// Инструменты домена: `handoff_create`, `harness_run`.
pub fn tools(cfg: &Config) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(HandoffCreateTool { cfg: cfg.clone() }),
        Arc::new(HarnessRunTool { cfg: cfg.clone() }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Конфиг с assets внутри временного каталога (изоляция от ~/.arch-harness).
    fn cfg_in(dir: &Path) -> Config {
        let mut cfg = Config::default();
        cfg.paths.assets_dir = dir.join("assets");
        cfg
    }

    fn write_file(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(path, text).expect("write");
    }

    const SPINE: &str = "# Spine\n\n\
        ## AD-1: Единый стек\n\n\
        **Binds:** все сервисы — Rust 1.85.\n\n\
        **Prevents:** зоопарк языков в контуре.\n\n\
        **Rule:** в CI закреплён toolchain 1.85.\n\n\
        ## Прочее\n\n\
        Абзац один.\n\n\
        Абзац два.\n\n\
        Абзац три — не должен попасть в контекст.\n";

    #[test]
    fn generates_full_packet_and_preserves_user_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).expect("mkdir repo");
        let cfg = cfg_in(tmp.path());
        write_file(
            &cfg.paths.rubrics_dir().join("handoff_quality.yaml"),
            "# якорная рубрика\n",
        );
        let spine = tmp.path().join("specs/spine.md");
        write_file(&spine, SPINE);
        let adr = tmp.path().join("specs/adr/ADR-001.md");
        write_file(&adr, "# ADR-001\n\nСтатус: Accepted.\n");
        let notes = tmp.path().join("specs/notes.md");
        write_file(&notes, "# Заметки\n\nпервый\n\nвторой\n\nтретий\n");

        let packet = generate_handoff(
            &repo,
            "сделать фичу X",
            &[spine.clone(), adr.clone(), notes.clone()],
            &cfg,
        )
        .expect("handoff");
        let dir = repo.join(".arch-handoff");
        assert_eq!(packet.dir, dir);

        let task_md = std::fs::read_to_string(dir.join("TASK.md")).expect("TASK.md");
        assert!(task_md.contains("сделать фичу X"));
        assert!(task_md.contains("## Контракт результата"));
        assert!(task_md.contains("\"complete|partial|blocked\""));

        let arch = std::fs::read_to_string(dir.join("ARCHITECTURE.md")).expect("ARCHITECTURE.md");
        // ADR-блок включён целиком (все три поля на месте).
        for field in ["**Binds:**", "**Prevents:**", "**Rule:**"] {
            assert!(arch.contains(field), "нет поля {field}");
        }
        // Прочие секции — только первые два абзаца.
        assert!(arch.contains("Абзац два."));
        assert!(!arch.contains("Абзац три"));
        assert!(arch.contains("Источники:"));

        // CONSTRAINTS.yaml создан с дефолтными правилами.
        let constraints = dir.join("CONSTRAINTS.yaml");
        let c = std::fs::read_to_string(&constraints).expect("CONSTRAINTS.yaml");
        for marker in [
            "must_not_contain",
            "unwrap",
            "dbg!",
            "file_exists",
            "command_succeeds",
            "cargo check",
            "timeout_secs: 120",
        ] {
            assert!(c.contains(marker), "CONSTRAINTS.yaml: нет '{marker}'");
        }

        // RUBRIC.yaml — копия якорной рубрики.
        let rubric = std::fs::read_to_string(dir.join("RUBRIC.yaml")).expect("RUBRIC.yaml");
        assert_eq!(rubric, "# якорная рубрика\n");

        // MANIFEST.json — мета пакета.
        let manifest_text =
            std::fs::read_to_string(dir.join("MANIFEST.json")).expect("MANIFEST.json");
        let manifest: Value = serde_json::from_str(&manifest_text).expect("manifest json");
        assert_eq!(manifest["task"], "сделать фичу X");
        assert!(manifest["created_at"].is_string());
        assert_eq!(
            manifest["sources"].as_array().expect("sources").len(),
            3
        );
        let chars = manifest["epic_context_chars"].as_u64().expect("chars") as usize;
        assert_eq!(chars, arch.chars().count());
        let tokens = manifest["epic_context_tokens"].as_u64().expect("tokens") as usize;
        assert_eq!(tokens, chars / 4);
        assert_eq!(packet.epic_context_tokens, tokens);

        // adr/ — копия ADR-файла.
        assert!(dir.join("adr/ADR-001.md").is_file());
        assert!(packet.files.contains(&dir.join("adr/ADR-001.md")));

        // Повторный прогон: пользовательские CONSTRAINTS/RUBRIC не затираются,
        // TASK.md и MANIFEST.json перезаписываются.
        std::fs::write(&constraints, "# пользовательские правила\n").expect("custom constraints");
        std::fs::write(dir.join("RUBRIC.yaml"), "# пользовательская рубрика\n")
            .expect("custom rubric");
        let packet2 = generate_handoff(&repo, "другая задача", &[spine, adr, notes], &cfg)
            .expect("second handoff");
        assert_eq!(
            std::fs::read_to_string(&constraints).expect("constraints after"),
            "# пользовательские правила\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("RUBRIC.yaml")).expect("rubric after"),
            "# пользовательская рубрика\n"
        );
        assert!(
            std::fs::read_to_string(dir.join("TASK.md"))
                .expect("TASK.md after")
                .contains("другая задача")
        );
        assert!(packet2.files.contains(&constraints));
    }

    #[test]
    fn long_spec_is_truncated_with_notice() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).expect("mkdir repo");
        let cfg = cfg_in(tmp.path());
        let big = tmp.path().join("big.md");
        let mut text = String::from("# Большая спека\n\n");
        for i in 0..500 {
            text.push_str(&format!(
                "## Секция {i}\n\nДостаточно длинный абзац, чтобы набрать объём контекста.\n\n"
            ));
        }
        write_file(&big, &text);

        let packet = generate_handoff(&repo, "задача", &[big], &cfg).expect("handoff");
        let arch = std::fs::read_to_string(packet.dir.join("ARCHITECTURE.md")).expect("arch");
        assert!(
            arch.chars().count() <= EPIC_CONTEXT_MAX_CHARS,
            "len = {}",
            arch.chars().count()
        );
        assert!(arch.contains("Контекст усечён"));
    }

    #[test]
    fn builds_argv_per_prompt_mode() {
        let cfg = |args: &[&str], mode: PromptMode| CodingHarnessConfig {
            binary: "bin".into(),
            args: args.iter().map(|s| (*s).into()).collect(),
            prompt_mode: mode,
            ..CodingHarnessConfig::default()
        };

        // Positional: задача — позиционный аргумент в конце.
        let (argv, stdin) = build_argv(&cfg(&["-p"], PromptMode::Positional), "TASK");
        assert_eq!(argv, ["-p", "TASK"]);
        assert!(stdin.is_none());

        // Flag с плейсхолдером: подстановка на место {prompt}.
        let (argv, stdin) = build_argv(
            &cfg(&["agent", "--message", "{prompt}"], PromptMode::Flag),
            "TASK",
        );
        assert_eq!(argv, ["agent", "--message", "TASK"]);
        assert!(stdin.is_none());

        // Flag без плейсхолдера: задача добавляется в конец.
        let (argv, stdin) = build_argv(&cfg(&["run", "--task"], PromptMode::Flag), "TASK");
        assert_eq!(argv, ["run", "--task", "TASK"]);
        assert!(stdin.is_none());

        // Stdin: argv без задачи, задача — в stdin.
        let (argv, stdin) = build_argv(&cfg(&["-p"], PromptMode::Stdin), "TASK");
        assert_eq!(argv, ["-p"]);
        assert_eq!(stdin.as_deref(), Some("TASK"));
    }

    #[tokio::test]
    async fn runs_stdin_harness_and_captures_output() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = CodingHarnessConfig {
            binary: "cat".into(),
            prompt_mode: PromptMode::Stdin,
            timeout_secs: 30,
            ..CodingHarnessConfig::default()
        };
        let run = run_harness("test-cat", &cfg, tmp.path(), "привет, харнесс")
            .await
            .expect("run");
        assert_eq!(run.harness, "test-cat");
        assert_eq!(run.exit_code, Some(0));
        assert_eq!(run.stdout, "привет, харнесс");
        assert!(run.stderr.is_empty());
        assert!(run.duration_secs >= 0.0);
    }

    #[tokio::test]
    async fn missing_binary_returns_hint() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = CodingHarnessConfig {
            binary: "definitely-missing-bin".into(),
            timeout_secs: 5,
            ..CodingHarnessConfig::default()
        };
        let err = run_harness("theseus", &cfg, tmp.path(), "задача")
            .await
            .expect_err("должна быть ошибка");
        let msg = err.to_string();
        assert!(msg.contains("definitely-missing-bin"), "{msg}");
        assert!(msg.contains("установите"), "{msg}");
        assert!(msg.contains("[harnesses.theseus]"), "{msg}");
    }

    #[tokio::test]
    async fn timeout_kills_long_run() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = CodingHarnessConfig {
            binary: "sleep".into(),
            args: vec!["5".into()],
            prompt_mode: PromptMode::Stdin,
            timeout_secs: 1,
            ..CodingHarnessConfig::default()
        };
        let err = run_harness("slow", &cfg, tmp.path(), "задача")
            .await
            .expect_err("должен быть таймаут");
        assert!(err.to_string().contains("таймаут"), "{err}");
    }

    #[tokio::test]
    async fn handoff_create_tool_reports_summary() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).expect("mkdir repo");
        let cfg = cfg_in(tmp.path());
        let tool = HandoffCreateTool { cfg: cfg.clone() };
        assert_eq!(tool.spec().name, "handoff_create");
        let ctx = ToolContext::new(tmp.path().to_path_buf(), Arc::new(cfg));

        // Нет обязательного аргумента.
        let out = tool.call(json!({"task": "x"}), &ctx).await.expect("call");
        assert!(out.is_error);
        assert!(out.content.contains("'repo'"));

        // Полный вызов (repo относительно cwd).
        let out = tool
            .call(json!({"repo": "repo", "task": "сделать Y"}), &ctx)
            .await
            .expect("call");
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("Handoff-пакет создан"));
        assert!(repo.join(".arch-handoff/TASK.md").is_file());
    }

    /// Конфиг с поддельным харнессом `fake` на бинаре `cat` (stdin → stdout).
    fn cfg_with_fake_harness(dir: &Path) -> Config {
        let mut cfg = cfg_in(dir);
        cfg.harnesses.insert(
            "fake".into(),
            CodingHarnessConfig {
                binary: "cat".into(),
                prompt_mode: PromptMode::Stdin,
                timeout_secs: 30,
                ..CodingHarnessConfig::default()
            },
        );
        cfg
    }

    #[test]
    fn extract_contract_finds_last_json_block_with_status() {
        let stdout = "текст\n```json\n{\"status\": \"partial\", \"assumptions\": [\"a\"], \
                      \"open_questions\": [], \"conflicts_with_prior_decisions\": []}\n```\n";
        let v = extract_contract(stdout).expect("контракт найден");
        assert_eq!(v["status"], "partial");
        // Без поля status — не контракт.
        assert!(extract_contract("```json\n{\"x\": 1}\n```").is_none());
        // Без fenced-блока — None.
        assert!(extract_contract("plain text").is_none());
        // Битый JSON — None.
        assert!(extract_contract("```json\n{oops}\n```").is_none());
    }

    #[tokio::test]
    async fn harness_run_tool_validates_args() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = cfg_with_fake_harness(tmp.path());
        let tool = HarnessRunTool { cfg: cfg.clone() };
        assert_eq!(tool.spec().name, "harness_run");
        let ctx = ToolContext::new(tmp.path().to_path_buf(), Arc::new(cfg));

        let out = tool.call(json!({"repo": "."}), &ctx).await.expect("call");
        assert!(out.is_error && out.content.contains("'harness'"), "{}", out.content);

        let out = tool
            .call(json!({"harness": "nope", "repo": "."}), &ctx)
            .await
            .expect("call");
        assert!(out.is_error, "{}", out.content);
        assert!(out.content.contains("не настроен"), "{}", out.content);
        assert!(out.content.contains("claude-code"), "список известных: {}", out.content);

        // Нет task и нет TASK.md — понятная ошибка с подсказкой.
        let out = tool
            .call(json!({"harness": "fake", "repo": "."}), &ctx)
            .await
            .expect("call");
        assert!(out.is_error, "{}", out.content);
        assert!(out.content.contains("handoff_create"), "{}", out.content);
    }

    #[tokio::test]
    async fn harness_run_reads_task_md_and_extracts_contract() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        write_file(
            &repo.join(".arch-handoff/TASK.md"),
            "Сделай фичу\n\n```json\n{\"status\": \"complete\", \"assumptions\": [], \
             \"open_questions\": [\"q1\"], \"conflicts_with_prior_decisions\": []}\n```\n",
        );
        let cfg = cfg_with_fake_harness(tmp.path());
        let tool = HarnessRunTool { cfg };
        let ctx = ToolContext::new(tmp.path().to_path_buf(), Arc::new(Config::default()));

        // cat вернёт TASK.md в stdout — контракт извлекается в сводку.
        let out = tool
            .call(json!({"harness": "fake", "repo": "repo"}), &ctx)
            .await
            .expect("call");
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("код 0"), "{}", out.content);
        assert!(out.content.contains("status=complete"), "{}", out.content);
        assert!(out.content.contains("open_questions: 1"), "{}", out.content);
        assert!(out.content.contains("Сделай фичу"), "{}", out.content);
    }

    #[tokio::test]
    async fn harness_run_warns_when_contract_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut cfg = cfg_in(tmp.path());
        cfg.harnesses.insert(
            "fake".into(),
            CodingHarnessConfig {
                binary: "echo".into(),
                args: vec!["нет контракта".into()],
                prompt_mode: PromptMode::Stdin,
                timeout_secs: 30,
                ..CodingHarnessConfig::default()
            },
        );
        let tool = HarnessRunTool { cfg };
        let ctx = ToolContext::new(tmp.path().to_path_buf(), Arc::new(Config::default()));
        // echo не читает stdin, печатает строку без контракта, код 0.
        let out = tool
            .call(json!({"harness": "fake", "repo": ".", "task": "задача"}), &ctx)
            .await
            .expect("call");
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("контракт результата"), "{}", out.content);
        assert!(out.content.contains("не найден"), "{}", out.content);
    }
}
