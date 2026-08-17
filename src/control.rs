//! Архитектурный контроль: fitness functions, линтер spine, сенсоры спек,
//! маршрутизация по Architecture Significance Score.
//!
//! КОНТРАКТ (владелец: агент `control`) — детерминированный механический слой
//! (идеи из docs/SOURCE_BRIEF.md: линтер spine, сенсоры required-sections и
//! upstream-coverage, 15 триггеров значимости, маршруты Fast/Standard/Critical):
//! - [`lint_spine`] — проверки ARCHITECTURE-SPINE.md: дубли AD-id, пустые
//!   Binds/Prevents/Rule, заглушки (TODO/TBD), непиннутые версии, ссылки на
//!   несуществующие AD;
//! - [`sensors_check`] — сенсоры спецификаций: наличие обязательных секций,
//!   upstream-coverage (артефакт ссылается на входы из consumes);
//! - [`check`] — fitness functions из CONSTRAINTS.yaml: must_contain /
//!   must_not_contain (regex по glob-набору файлов), file_exists,
//!   command_succeeds (с таймаутом); итог PASS/FAIL + находки;
//! - [`significance_score`] — по ответам на 15 триггеров → Score + Route.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use walkdir::WalkDir;

use crate::error::{HarnessError, Result};
use crate::llm::ToolSpec;
use crate::tool::{Tool, ToolContext, ToolOutput};

/// Маршрут изменения по значимости.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Route {
    /// Низкий риск: дельта-спека + авто-валидация.
    Fast,
    /// Средний: контракт Spec→Plan→Tasks + Architecture Fit автоматически.
    Standard,
    /// Архитектурно/регуляторно значимое: Solutioning + human decision (A3).
    Critical,
}

impl std::str::FromStr for Route {
    type Err = String;

    /// Парсит маршрут из строки (fast/standard/critical, без учёта регистра).
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "fast" => Ok(Self::Fast),
            "standard" => Ok(Self::Standard),
            "critical" => Ok(Self::Critical),
            other => Err(format!(
                "неизвестный маршрут '{other}' (допустимы: fast, standard, critical)"
            )),
        }
    }
}

impl std::fmt::Display for Route {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Fast => "Fast",
            Self::Standard => "Standard",
            Self::Critical => "Critical",
        };
        f.write_str(s)
    }
}

/// Результат оценки значимости.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Significance {
    /// Число сработавших триггеров.
    pub score: usize,
    /// Сработавшие триггеры.
    pub fired: Vec<String>,
    /// Маршрут.
    pub route: Route,
}

/// Канонический список из 15 триггеров архитектурной значимости
/// (из обзора AI-Disrupt PDLC, см. docs/SOURCE_BRIEF.md §C.3).
pub const SIGNIFICANCE_TRIGGERS: [&str; 15] = [
    "new_component",
    "new_datastore",
    "new_vendor",
    "domain_ownership_change",
    "cross_domain_integration",
    "api_contract_change",
    "data_contract_change",
    "security_boundary_change",
    "trust_zone_change",
    "consistency_model_change",
    "significant_nfr",
    "rto_rpo_targets",
    "irreversible_migration",
    "financial_impact",
    "criticality_or_exception",
];

/// Обязательные заголовки спецификации (сенсор `required_sections`).
pub const REQUIRED_SECTIONS: [&str; 3] = ["## Проблема", "## Критерии приёмки", "## Риски"];

/// Оценивает значимость по карте «триггер → сработал».
/// Маршрут: 0–1 → Fast, 2–4 → Standard, 5+ или критические триггеры
/// (security_boundary_change, irreversible_migration, criticality_or_exception) → Critical.
pub fn significance_score(answers: &BTreeMap<String, bool>) -> Significance {
    let fired: Vec<String> = answers
        .iter()
        .filter(|(_, v)| **v)
        .map(|(k, _)| k.clone())
        .collect();
    let critical = [
        "security_boundary_change",
        "irreversible_migration",
        "criticality_or_exception",
    ]
    .iter()
    .any(|t| fired.iter().any(|f| f == t));
    let score = fired.len();
    let route = if critical || score >= 5 {
        Route::Critical
    } else if score >= 2 {
        Route::Standard
    } else {
        Route::Fast
    };
    Significance {
        score,
        fired,
        route,
    }
}

/// Находка линтера/сенсора.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LintIssue {
    /// Файл.
    pub file: PathBuf,
    /// Строка (0 — файл целиком).
    pub line: usize,
    /// Код правила (dup_ad_id, empty_field, stub_marker, unpinned_version,
    /// broken_ad_ref либо имя fitness-правила из CONSTRAINTS.yaml).
    pub rule: String,
    /// Сообщение.
    pub message: String,
    /// Критичность: error|warn.
    pub severity: String,
}

/// Линтер ARCHITECTURE-SPINE.md.
///
/// Определением AD-блока считается заголовок `### AD-<n> …` либо строка вида
/// `AD-<n>:` / `AD-<n>.` в начале строки; остальные вхождения `AD-<n>` —
/// ссылки. Блок простирается до следующего определения AD (или конца файла).
///
/// # Errors
/// Файл не читается.
pub fn lint_spine(path: &Path) -> Result<Vec<LintIssue>> {
    let content = std::fs::read_to_string(path).map_err(|e| HarnessError::io(path, e))?;
    let lines: Vec<&str> = content.lines().collect();

    let re_def_heading = spine_regex(r"^\s{0,3}#{1,6}\s+AD-(\d+)\b")?;
    let re_def_bare = spine_regex(r"^\s*AD-(\d+)\s*[:.]")?;
    let re_ad_ref = spine_regex(r"\bAD-(\d+)\b")?;
    let re_stub = spine_regex(r"\b(?:TODO|TBD|FIXME|XXX)\b|\?\?\?")?;
    let re_unpinned = spine_regex(r#"\blatest\b|[:=]\s*["']?\*["']?(?:\s|$)"#)?;
    let re_field = spine_regex(r"\b(Binds|Prevents|Rule)\*{0,2}\s*:\s*\*{0,2}\s*(.*)$")?;

    // Первый проход: определения AD-блоков.
    let mut def_lines: Vec<Option<u64>> = vec![None; lines.len()];
    let mut definitions: Vec<(u64, usize)> = Vec::new(); // (id, строка 1-based)
    for (idx, line) in lines.iter().enumerate() {
        let caps = re_def_heading
            .captures(line)
            .or_else(|| re_def_bare.captures(line));
        if let Some(caps) = caps {
            let id: u64 = caps[1].parse().map_err(|_| {
                HarnessError::Control(format!(
                    "{}:{}: некорректный идентификатор AD",
                    path.display(),
                    idx + 1
                ))
            })?;
            def_lines[idx] = Some(id);
            definitions.push((id, idx + 1));
        }
    }
    let defined: BTreeSet<u64> = definitions.iter().map(|(id, _)| *id).collect();

    let mut issues = Vec::new();
    let mut push = |line: usize, rule: &str, severity: &str, message: String| {
        issues.push(LintIssue {
            file: path.to_path_buf(),
            line,
            rule: rule.into(),
            message,
            severity: severity.into(),
        });
    };

    // dup_ad_id: повторное определение того же идентификатора.
    let mut first_seen: BTreeMap<u64, usize> = BTreeMap::new();
    for (id, line_no) in &definitions {
        if let Some(first) = first_seen.get(id) {
            push(
                *line_no,
                "dup_ad_id",
                "error",
                format!("повторное определение AD-{id} (первое — строка {first})"),
            );
        } else {
            first_seen.insert(*id, *line_no);
        }
    }

    // empty_field: у каждого AD-блока должны быть непустые Binds/Prevents/Rule.
    for (pos, (id, def_line)) in definitions.iter().enumerate() {
        let start = def_line - 1;
        let end = definitions
            .get(pos + 1)
            .map_or(lines.len(), |(_, next)| next - 1);
        let block = &lines[start..end];
        for field in ["Binds", "Prevents", "Rule"] {
            let mut found = false;
            for (off, line) in block.iter().enumerate() {
                let Some(caps) = re_field.captures(line) else {
                    continue;
                };
                if &caps[1] != field {
                    continue;
                }
                found = true;
                let value = caps[2].trim().trim_matches('*').trim();
                if value.is_empty() {
                    push(
                        start + off + 1,
                        "empty_field",
                        "error",
                        format!("AD-{id}: пустое обязательное поле '{field}'"),
                    );
                }
            }
            if !found {
                push(
                    *def_line,
                    "empty_field",
                    "error",
                    format!("AD-{id}: отсутствует обязательное поле '{field}'"),
                );
            }
        }
    }

    // Построчные правила: заглушки и непиннутые версии.
    for (idx, line) in lines.iter().enumerate() {
        if let Some(m) = re_stub.find(line) {
            push(
                idx + 1,
                "stub_marker",
                "warn",
                format!("заглушка '{}' — заполнить до гейта", m.as_str()),
            );
        }
        if re_unpinned.is_match(line) {
            let token = if line.contains("latest") {
                "'latest'"
            } else {
                "'*'"
            };
            push(
                idx + 1,
                "unpinned_version",
                "warn",
                format!("непиннутая версия зависимости: {token}"),
            );
        }
    }

    // broken_ad_ref: ссылки на несуществующие в файле AD.
    for (idx, line) in lines.iter().enumerate() {
        let mut reported: BTreeSet<u64> = BTreeSet::new();
        for caps in re_ad_ref.captures_iter(line) {
            let Ok(id) = caps[1].parse::<u64>() else {
                continue;
            };
            // Собственный идентификатор на строке определения — не ссылка.
            if def_lines[idx] == Some(id) || defined.contains(&id) || !reported.insert(id) {
                continue;
            }
            push(
                idx + 1,
                "broken_ad_ref",
                "warn",
                format!("ссылка на несуществующий AD-{id}"),
            );
        }
    }

    issues.sort_by(|a, b| (a.line, &a.rule).cmp(&(b.line, &b.rule)));
    Ok(issues)
}

/// Компилирует статический regex линтера (ошибка компиляции — внутренний дефект).
fn spine_regex(pattern: &str) -> Result<Regex> {
    Regex::new(pattern).map_err(|e| HarnessError::Control(format!("внутренний regex линтера: {e}")))
}

/// Результат сенсора.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SensorResult {
    /// Имя сенсора (required_sections, upstream_coverage).
    pub sensor: String,
    /// Проверенный файл.
    pub file: PathBuf,
    /// Прошёл ли.
    pub passed: bool,
    /// Детали.
    pub details: String,
}

/// Прогон сенсоров по каталогу спецификаций (нерекурсивно, только `*.md`).
///
/// Сенсоры:
/// - `required_sections` — наличие заголовков из [`REQUIRED_SECTIONS`];
/// - `upstream_coverage` — относительные ссылки `[..](path.md)` существуют
///   относительно каталога спецификаций.
///
/// # Errors
/// Каталог не читается.
pub fn sensors_check(spec_dir: &Path) -> Result<Vec<SensorResult>> {
    let mut files: Vec<PathBuf> = Vec::new();
    let rd = std::fs::read_dir(spec_dir).map_err(|e| HarnessError::io(spec_dir, e))?;
    for entry in rd {
        let entry = entry.map_err(|e| HarnessError::io(spec_dir, e))?;
        let p = entry.path();
        if p.is_file() && p.extension().is_some_and(|ext| ext == "md") {
            files.push(p);
        }
    }
    files.sort();

    let re_link = Regex::new(r"\[[^\]]*\]\(\s*([^)\s]+)[^)]*\)")
        .map_err(|e| HarnessError::Control(format!("внутренний regex сенсоров: {e}")))?;

    let mut results = Vec::new();
    for file in files {
        let content = std::fs::read_to_string(&file).map_err(|e| HarnessError::io(&file, e))?;

        let missing: Vec<&str> = REQUIRED_SECTIONS
            .iter()
            .copied()
            .filter(|sec| !content.lines().any(|l| l.trim_start().starts_with(sec)))
            .collect();
        results.push(SensorResult {
            sensor: "required_sections".into(),
            file: file.clone(),
            passed: missing.is_empty(),
            details: if missing.is_empty() {
                "все обязательные секции на месте".into()
            } else {
                format!("нет секций: {}", missing.join(", "))
            },
        });

        let mut total = 0usize;
        let mut broken: Vec<String> = Vec::new();
        for caps in re_link.captures_iter(&content) {
            let raw = &caps[1];
            if raw.starts_with("http://")
                || raw.starts_with("https://")
                || raw.starts_with("mailto:")
                || raw.starts_with('#')
            {
                continue;
            }
            let target = raw.split('#').next().unwrap_or(raw);
            if target.is_empty() {
                continue;
            }
            total += 1;
            let p = Path::new(target);
            let full = if p.is_absolute() {
                p.to_path_buf()
            } else {
                spec_dir.join(p)
            };
            if !full.exists() {
                broken.push(target.to_string());
            }
        }
        results.push(SensorResult {
            sensor: "upstream_coverage".into(),
            file,
            passed: broken.is_empty(),
            details: if broken.is_empty() {
                format!("все ссылки валидны ({total})")
            } else {
                format!("битые ссылки: {}", broken.join(", "))
            },
        });
    }
    Ok(results)
}

/// Отчёт fitness-контроля.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FitnessReport {
    /// Репозиторий.
    pub repo: PathBuf,
    /// Все правила пройдены.
    pub passed: bool,
    /// Находки по правилам.
    pub issues: Vec<LintIssue>,
    /// Сводка (для отчёта).
    pub summary: String,
}

/// Тип fitness-правила из CONSTRAINTS.yaml.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RuleKind {
    /// Regex должен найтись хотя бы в одном файле по glob.
    MustContain,
    /// Regex не должен встречаться ни в одном файле по glob (issue на каждое вхождение).
    MustNotContain,
    /// Файл существует относительно корня репозитория.
    FileExists,
    /// Команда (`bash -c`, в корне репозитория) завершается кодом 0 до таймаута.
    CommandSucceeds,
}

/// Одно правило из CONSTRAINTS.yaml.
#[derive(Debug, Deserialize)]
struct FitnessRule {
    /// Имя правила (становится кодом находки).
    name: String,
    /// Тип проверки.
    #[serde(rename = "type")]
    kind: RuleKind,
    /// Glob набора файлов (для must_contain/must_not_contain; дефолт `**/*`).
    glob: Option<String>,
    /// Regex (для must_contain/must_not_contain).
    pattern: Option<String>,
    /// Путь относительно репозитория (для file_exists).
    path: Option<String>,
    /// Команда (для command_succeeds).
    command: Option<String>,
    /// Критичность находок правила: error|warn (дефолт error).
    #[serde(default = "default_severity")]
    severity: String,
    /// Таймаут команды, секунды (дефолт 60).
    #[serde(default = "default_timeout_secs")]
    timeout_secs: u64,
}

fn default_severity() -> String {
    "error".into()
}

fn default_timeout_secs() -> u64 {
    60
}

/// Корень CONSTRAINTS.yaml.
#[derive(Debug, Deserialize)]
struct ConstraintsFile {
    /// Список правил.
    #[serde(default)]
    rules: Vec<FitnessRule>,
}

/// Прогон fitness functions из CONSTRAINTS.yaml по репозиторию.
///
/// `passed = true`, если нет находок с severity `error`. Обход репозитория
/// пропускает каталоги `.git` и `target`; файлы в не-UTF8 кодировке читаются
/// с потерями (content-правила по ним приблизительны).
///
/// # Errors
/// CONSTRAINTS.yaml не читается/не валиден, репозиторий недоступен,
/// правило некорректно (нет pattern/path/command, невалидный regex/severity).
pub fn check(repo: &Path, constraints: &Path) -> Result<FitnessReport> {
    if !repo.is_dir() {
        return Err(HarnessError::Control(format!(
            "репозиторий недоступен: {}",
            repo.display()
        )));
    }
    let yaml =
        std::fs::read_to_string(constraints).map_err(|e| HarnessError::io(constraints, e))?;
    let parsed: ConstraintsFile = serde_yaml::from_str(&yaml)?;

    let mut issues = Vec::new();
    for rule in &parsed.rules {
        run_rule(rule, repo, &mut issues)?;
    }
    issues.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then(a.line.cmp(&b.line))
            .then(a.rule.cmp(&b.rule))
    });

    let errors = issues.iter().filter(|i| i.severity == "error").count();
    let warns = issues.len() - errors;
    let summary = format!(
        "Правил: {}, нарушений: {} (error: {errors}, warn: {warns})",
        parsed.rules.len(),
        issues.len()
    );
    Ok(FitnessReport {
        repo: repo.to_path_buf(),
        passed: errors == 0,
        issues,
        summary,
    })
}

/// Выполняет одно fitness-правило, добавляя находки в `issues`.
fn run_rule(rule: &FitnessRule, repo: &Path, issues: &mut Vec<LintIssue>) -> Result<()> {
    if rule.severity != "error" && rule.severity != "warn" {
        return Err(HarnessError::Control(format!(
            "правило '{}': severity должно быть error|warn, получено '{}'",
            rule.name, rule.severity
        )));
    }
    let mut issue = |file: PathBuf, line: usize, message: String| {
        issues.push(LintIssue {
            file,
            line,
            rule: rule.name.clone(),
            message,
            severity: rule.severity.clone(),
        });
    };
    match rule.kind {
        RuleKind::MustContain => {
            let (re, glob, files) = prep_content_rule(rule, repo)?;
            let pattern = rule.pattern.as_deref().unwrap_or_default();
            let mut found = false;
            for (_, abs) in &files {
                let bytes = std::fs::read(abs).map_err(|e| HarnessError::io(abs, e))?;
                if re.is_match(&String::from_utf8_lossy(&bytes)) {
                    found = true;
                    break;
                }
            }
            if !found {
                issue(
                    PathBuf::from(&glob),
                    0,
                    format!(
                        "must_contain: паттерн '{pattern}' не найден ни в одном файле по glob '{glob}'"
                    ),
                );
            }
        }
        RuleKind::MustNotContain => {
            let (re, _, files) = prep_content_rule(rule, repo)?;
            let pattern = rule.pattern.as_deref().unwrap_or_default();
            for (rel, abs) in &files {
                let bytes = std::fs::read(abs).map_err(|e| HarnessError::io(abs, e))?;
                let content = String::from_utf8_lossy(&bytes);
                for (idx, line) in content.lines().enumerate() {
                    if re.is_match(line) {
                        let snippet: String = line.trim().chars().take(120).collect();
                        issue(
                            PathBuf::from(rel),
                            idx + 1,
                            format!("must_not_contain: запрещённый паттерн '{pattern}': {snippet}"),
                        );
                    }
                }
            }
        }
        RuleKind::FileExists => {
            let rel = rule.path.as_deref().ok_or_else(|| {
                HarnessError::Control(format!(
                    "правило '{}': для file_exists нужен path",
                    rule.name
                ))
            })?;
            if !repo.join(rel).exists() {
                issue(
                    PathBuf::from(rel),
                    0,
                    format!("file_exists: файл не найден: {rel}"),
                );
            }
        }
        RuleKind::CommandSucceeds => {
            let cmd = rule.command.as_deref().ok_or_else(|| {
                HarnessError::Control(format!(
                    "правило '{}': для command_succeeds нужен command",
                    rule.name
                ))
            })?;
            match run_with_timeout(repo, cmd, Duration::from_secs(rule.timeout_secs))? {
                Some(status) if status.success() => {}
                Some(status) => {
                    let code = status
                        .code()
                        .map_or_else(|| "завершена сигналом".to_string(), |c| format!("код {c}"));
                    issue(
                        repo.to_path_buf(),
                        0,
                        format!("command_succeeds: команда '{cmd}' завершилась неуспешно ({code})"),
                    );
                }
                None => {
                    issue(
                        repo.to_path_buf(),
                        0,
                        format!(
                            "command_succeeds: команда '{cmd}' превысила таймаут {}s и была убита",
                            rule.timeout_secs
                        ),
                    );
                }
            }
        }
    }
    Ok(())
}

/// Общая подготовка content-правил: компилированный regex, glob, набор файлов.
fn prep_content_rule(
    rule: &FitnessRule,
    repo: &Path,
) -> Result<(Regex, String, Vec<(String, PathBuf)>)> {
    let pattern = rule.pattern.as_deref().ok_or_else(|| {
        HarnessError::Control(format!(
            "правило '{}': для {:?} нужен pattern",
            rule.name, rule.kind
        ))
    })?;
    let re = Regex::new(pattern).map_err(|e| {
        HarnessError::Control(format!(
            "правило '{}': невалидный regex '{pattern}': {e}",
            rule.name
        ))
    })?;
    let glob = rule.glob.as_deref().unwrap_or("**/*").to_string();
    let files = collect_files(repo, &glob)?;
    Ok((re, glob, files))
}

/// Собирает файлы репозитория по простому glob-шаблону (`**` — любая глубина,
/// `*` — внутри сегмента, `?` — один символ). Возвращает (относительный путь,
/// абсолютный путь), отсортированные по относительному пути.
///
/// Служебные и производные каталоги исключены всегда: `.git`, `target`,
/// `node_modules`, `dist`, `__pycache__`, `.next`, `.pytest_cache` и
/// `.arch-handoff` — fitness-правила целятся в АРТЕФАКТЫ РЕАЛИЗАЦИИ, а не в
/// документы решения: пакет handoff содержит текст spine/TASK.md, и правило
/// must_not_contain срабатывало на собственные цитаты контракта (кейс 1).
fn collect_files(repo: &Path, glob: &str) -> Result<Vec<(String, PathBuf)>> {
    const SKIP: [&str; 8] = [
        ".git",
        "target",
        "node_modules",
        "dist",
        "__pycache__",
        ".next",
        ".pytest_cache",
        ".arch-handoff",
    ];
    let mut out = Vec::new();
    let walker = WalkDir::new(repo).follow_links(false).into_iter();
    for entry in walker.filter_entry(|e| {
        let name = e.file_name().to_string_lossy();
        !(e.file_type().is_dir() && SKIP.contains(&name.as_ref()))
    }) {
        let entry = entry.map_err(|e| {
            HarnessError::Control(format!("обход репозитория {}: {e}", repo.display()))
        })?;
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry.path().strip_prefix(repo).map_err(|e| {
            HarnessError::Control(format!(
                "относительный путь {}: {e}",
                entry.path().display()
            ))
        })?;
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        if glob_matches(glob, &rel_str) {
            out.push((rel_str, entry.path().to_path_buf()));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// Матч одного сегмента пути по glob-шаблону (`*` — любые символы, `?` — один).
fn segment_matches(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    let (mut pi, mut ni) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None; // (позиция '*' в шаблоне, позиция в имени)
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ni));
            pi += 1;
        } else if let Some((sp, sn)) = star {
            pi = sp + 1;
            ni = sn + 1;
            star = Some((sp, sn + 1));
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Матч относительного пути по glob-шаблону с поддержкой `**` (любая глубина,
/// включая ноль сегментов: `**/*.rs` матчит и `main.rs`).
fn glob_matches(pattern: &str, path: &str) -> bool {
    let pat: Vec<&str> = pattern.split('/').collect();
    let parts: Vec<&str> = path.split('/').collect();
    match_glob_segments(&pat, &parts)
}

fn match_glob_segments(pat: &[&str], parts: &[&str]) -> bool {
    if pat.is_empty() {
        return parts.is_empty();
    }
    if pat[0] == "**" {
        return (0..=parts.len()).any(|skip| match_glob_segments(&pat[1..], &parts[skip..]));
    }
    if parts.is_empty() {
        return false;
    }
    segment_matches(pat[0], parts[0]) && match_glob_segments(&pat[1..], &parts[1..])
}

/// Запускает `bash -c <command>` в `repo` с ручным таймаутом:
/// spawn + опрос `try_wait` каждые 50 мс + `kill` по истечении.
/// `Ok(None)` — команда превысила таймаут и была убита.
fn run_with_timeout(repo: &Path, command: &str, timeout: Duration) -> Result<Option<ExitStatus>> {
    let mut child = Command::new("bash")
        .arg("-c")
        .arg(command)
        .current_dir(repo)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| HarnessError::Control(format!("не удалось запустить bash: {e}")))?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(Some(status)),
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait(); // забрать зомби
                    return Ok(None);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(HarnessError::Control(format!(
                    "ошибка ожидания команды '{command}': {e}"
                )));
            }
        }
    }
}

/// Создаёт новый ADR по шаблону AI-DLC (Status/Context/Decision/Alternatives/
/// Consequences/Reversibility/References) с очередным номером в каталоге.
///
/// Номер — max(существующие `ADR-NNN-*`) + 1; каталог создаётся при
/// отсутствии. Имя файла: `ADR-NNN-kebab-case-title.md` (не-ASCII символы
/// заменяются на `-`, без транслитерации).
///
/// # Errors
/// Каталог недоступен/не создаётся, файл уже существует.
pub fn adr_new(dir: &Path, title: &str) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).map_err(|e| HarnessError::io(dir, e))?;
    let re_adr = Regex::new(r"^ADR-(\d+)")
        .map_err(|e| HarnessError::Control(format!("внутренний regex ADR: {e}")))?;
    let mut max_n = 0u64;
    let rd = std::fs::read_dir(dir).map_err(|e| HarnessError::io(dir, e))?;
    for entry in rd {
        let entry = entry.map_err(|e| HarnessError::io(dir, e))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if let Some(caps) = re_adr.captures(&name) {
            let n: u64 = caps[1].parse().map_err(|_| {
                HarnessError::Control(format!("некорректный номер ADR в имени файла '{name}'"))
            })?;
            max_n = max_n.max(n);
        }
    }
    let next = max_n + 1;
    let file = dir.join(format!("ADR-{next:03}-{}.md", kebab_slug(title)));
    if file.exists() {
        return Err(HarnessError::Control(format!(
            "ADR уже существует: {}",
            file.display()
        )));
    }
    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    std::fs::write(&file, adr_template(next, title, &date))
        .map_err(|e| HarnessError::io(&file, e))?;
    Ok(file)
}

/// Практичная транслитерация кириллической буквы для слагов (ADR-002).
/// `Some("")` для ъ/ь (опускаются без дефиса), `None` для не-кириллицы.
fn translit_cyrillic(ch: char) -> Option<&'static str> {
    let lat = match ch {
        'а' => "a",
        'б' => "b",
        'в' => "v",
        'г' => "g",
        'д' => "d",
        'е' => "e",
        'ё' => "yo",
        'ж' => "zh",
        'з' => "z",
        'и' => "i",
        'й' => "y",
        'к' => "k",
        'л' => "l",
        'м' => "m",
        'н' => "n",
        'о' => "o",
        'п' => "p",
        'р' => "r",
        'с' => "s",
        'т' => "t",
        'у' => "u",
        'ф' => "f",
        'х' => "h",
        'ц' => "c",
        'ч' => "ch",
        'ш' => "sh",
        'щ' => "sch",
        'ъ' | 'ь' => "",
        'ы' => "y",
        'э' => "e",
        'ю' => "yu",
        'я' => "ya",
        _ => return None,
    };
    Some(lat)
}

/// kebab-case slug заголовка: ASCII-буквы/цифры в нижний регистр, кириллица
/// транслитерируется (`translit_cyrillic`), всё прочее — в `-`, повторы `-`
/// схлопываются. Пустой результат (пустой/символьный заголовок) → `"adr"`.
fn kebab_slug(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    let mut dash = true; // подавляет '-' в начале
    for ch in title.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            dash = false;
        } else if let Some(lat) = translit_cyrillic(ch) {
            out.push_str(lat);
            if !lat.is_empty() {
                dash = false;
            }
        } else if !dash {
            out.push('-');
            dash = true;
        }
    }
    let trimmed = out.trim_end_matches('-');
    if trimmed.is_empty() {
        "adr".into()
    } else {
        trimmed.to_string()
    }
}

/// Шаблон ADR по AI-DLC с placeholder-комментариями.
fn adr_template(n: u64, title: &str, date: &str) -> String {
    format!(
        "# ADR-{n:03}. {title}\n\
        \n\
        - Date: {date}\n\
        - Status: Proposed\n\
        \n\
        ## Context\n\
        \n\
        <!-- Что заставляет принять решение: контекст, силы, ограничения. -->\n\
        \n\
        ## Decision\n\
        \n\
        <!-- Принятое решение: одно, явно сформулированное. -->\n\
        \n\
        ## Alternatives Considered\n\
        \n\
        | Вариант | Плюсы | Минусы |\n\
        |---------|-------|--------|\n\
        | <!-- вариант --> | <!-- плюсы --> | <!-- минусы --> |\n\
        \n\
        ## Consequences\n\
        \n\
        ### Positive\n\
        \n\
        <!-- Что станет лучше. -->\n\
        \n\
        ### Negative\n\
        \n\
        <!-- Цена решения: что станет хуже, какие риски принимаем. Обязательно к заполнению. -->\n\
        \n\
        ## Reversibility\n\
        \n\
        <!-- Обратимость: reversible | costly | irreversible. Обоснование оценки. -->\n\
        \n\
        ## References\n\
        \n\
        <!-- Ссылки на spine (AD-n), спеки, обсуждения. -->\n"
    )
}

/// Инструменты домена: `adr_new`, `spine_lint`, `fitness_check`, `significance_score`.
pub fn tools() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(AdrNewTool),
        Arc::new(SpineLintTool),
        Arc::new(FitnessCheckTool),
        Arc::new(SignificanceScoreTool),
    ]
}

/// Инструмент `adr_new`: создать ADR по шаблону AI-DLC с очередным номером.
pub struct AdrNewTool;

#[derive(Debug, Deserialize)]
struct AdrNewArgs {
    /// Заголовок решения.
    title: String,
    /// Каталог ADR (дефолт `docs/adr`).
    dir: Option<String>,
}

#[async_trait]
impl Tool for AdrNewTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "adr_new".into(),
            description: "Создать новый ADR (Architecture Decision Record) по шаблону AI-DLC \
                          (Context/Decision/Alternatives/Consequences/Reversibility) с очередным \
                          номером ADR-NNN в каталоге"
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "title": {"type": "string", "description": "Заголовок решения"},
                    "dir": {"type": "string", "description": "Каталог ADR (по умолчанию docs/adr)"}
                },
                "required": ["title"]
            }),
        }
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let args: AdrNewArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => {
                return Ok(ToolOutput::err(format!(
                    "adr_new: невалидные аргументы: {e}"
                )));
            }
        };
        let dir = ctx.resolve(args.dir.as_deref().unwrap_or("docs/adr"));
        match adr_new(&dir, &args.title) {
            Ok(path) => Ok(ToolOutput::ok(format!("ADR создан: {}", path.display()))),
            Err(e) => Ok(ToolOutput::err(format!("adr_new: {e}"))),
        }
    }
}

/// Инструмент `spine_lint`: линтер ARCHITECTURE-SPINE.md.
pub struct SpineLintTool;

#[derive(Debug, Deserialize)]
struct SpineLintArgs {
    /// Путь к файлу spine.
    path: String,
}

#[async_trait]
impl Tool for SpineLintTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "spine_lint".into(),
            description: "Проверить ARCHITECTURE-SPINE.md: дубли AD-id, пустые/отсутствующие \
                          Binds/Prevents/Rule, заглушки (TODO/TBD), непиннутые версии, \
                          ссылки на несуществующие AD"
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Путь к ARCHITECTURE-SPINE.md"}
                },
                "required": ["path"]
            }),
        }
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let args: SpineLintArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => {
                return Ok(ToolOutput::err(format!(
                    "spine_lint: невалидные аргументы: {e}"
                )));
            }
        };
        let path = ctx.resolve(&args.path);
        match lint_spine(&path) {
            Ok(issues) if issues.is_empty() => Ok(ToolOutput::ok("spine: нарушений нет")),
            Ok(issues) => {
                let mut out = format!("spine: {} находок\n", issues.len());
                for i in &issues {
                    let _ = writeln!(
                        out,
                        "[{}] {}:{} {} — {}",
                        i.severity,
                        i.file.display(),
                        i.line,
                        i.rule,
                        i.message
                    );
                }
                Ok(ToolOutput::ok(out))
            }
            Err(e) => Ok(ToolOutput::err(format!("spine_lint: {e}"))),
        }
    }
}

/// Инструмент `fitness_check`: прогон fitness functions из CONSTRAINTS.yaml.
pub struct FitnessCheckTool;

#[derive(Debug, Deserialize)]
struct FitnessCheckArgs {
    /// Корень репозитория.
    repo: String,
    /// Путь к CONSTRAINTS.yaml (дефолт `<repo>/.arch-handoff/CONSTRAINTS.yaml`).
    constraints: Option<String>,
}

#[async_trait]
impl Tool for FitnessCheckTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "fitness_check".into(),
            description: "Прогнать fitness functions из CONSTRAINTS.yaml по репозиторию: \
                          must_contain / must_not_contain (regex по glob-набору файлов), \
                          file_exists, command_succeeds (с таймаутом). Итог PASS/FAIL + находки"
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "repo": {"type": "string", "description": "Корень репозитория"},
                    "constraints": {
                        "type": "string",
                        "description": "Путь к CONSTRAINTS.yaml (по умолчанию <repo>/.arch-handoff/CONSTRAINTS.yaml)"
                    }
                },
                "required": ["repo"]
            }),
        }
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let args: FitnessCheckArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => {
                return Ok(ToolOutput::err(format!(
                    "fitness_check: невалидные аргументы: {e}"
                )));
            }
        };
        let repo = ctx.resolve(&args.repo);
        let constraints = args.constraints.map_or_else(
            || repo.join(".arch-handoff/CONSTRAINTS.yaml"),
            |c| ctx.resolve(c),
        );
        // Прогон может занимать минуты (command_succeeds) — уводим с worker'а runtime.
        match tokio::task::spawn_blocking(move || check(&repo, &constraints)).await {
            Ok(Ok(report)) => {
                let mut out = String::new();
                let _ = writeln!(out, "{}", report.summary);
                for i in &report.issues {
                    let _ = writeln!(
                        out,
                        "  [{}] {}:{} {} — {}",
                        i.severity,
                        i.file.display(),
                        i.line,
                        i.rule,
                        i.message
                    );
                }
                let _ = writeln!(out, "Итог: {}", if report.passed { "PASS" } else { "FAIL" });
                Ok(ToolOutput::ok(out))
            }
            Ok(Err(e)) => Ok(ToolOutput::err(format!("fitness_check: {e}"))),
            Err(e) => Ok(ToolOutput::err(format!(
                "fitness_check: задача прервана: {e}"
            ))),
        }
    }
}

/// Инструмент `significance_score`: Architecture Significance Score → маршрут.
pub struct SignificanceScoreTool;

#[derive(Debug, Deserialize)]
struct SignificanceScoreArgs {
    /// Карта «триггер → сработал».
    triggers: BTreeMap<String, bool>,
}

#[async_trait]
impl Tool for SignificanceScoreTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "significance_score".into(),
            description: "Оценить Architecture Significance Score по 15 триггерам и вернуть \
                          маршрут изменения: Fast (0–1), Standard (2–4), Critical (5+ или \
                          критические триггеры security_boundary_change / irreversible_migration / \
                          criticality_or_exception)"
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "triggers": {
                        "type": "object",
                        "description": "Карта «триггер → true/false», ключи — из 15 канонических триггеров",
                        "additionalProperties": {"type": "boolean"}
                    }
                },
                "required": ["triggers"]
            }),
        }
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let _ = ctx;
        let args: SignificanceScoreArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => {
                return Ok(ToolOutput::err(format!(
                    "significance_score: невалидные аргументы: {e}"
                )));
            }
        };
        let s = significance_score(&args.triggers);
        let fired = if s.fired.is_empty() {
            "нет".to_string()
        } else {
            s.fired.join(", ")
        };
        let mut out = format!(
            "Score: {} → маршрут {:?}; сработали: {fired}",
            s.score, s.route
        );
        let unknown: Vec<&str> = s
            .fired
            .iter()
            .map(String::as_str)
            .filter(|f| !SIGNIFICANCE_TRIGGERS.contains(f))
            .collect();
        if !unknown.is_empty() {
            let _ = write!(
                out,
                "\nВнимание: триггеры вне канонических 15: {}",
                unknown.join(", ")
            );
        }
        Ok(ToolOutput::ok(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn significance_routes_by_count() {
        let mut answers = BTreeMap::new();
        assert_eq!(significance_score(&answers).route, Route::Fast);
        answers.insert("new_component".into(), true);
        answers.insert("new_vendor".into(), true);
        assert_eq!(significance_score(&answers).route, Route::Standard);
        answers.insert("security_boundary_change".into(), true);
        assert_eq!(significance_score(&answers).route, Route::Critical);
    }

    /// Пишет файл в каталог и возвращает его путь.
    fn write_file(dir: &Path, name: &str, content: &str) -> PathBuf {
        let p = dir.join(name);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, content).unwrap();
        p
    }

    #[test]
    fn spine_detects_all_rule_kinds() {
        let dir = tempfile::tempdir().unwrap();
        let spine = write_file(
            dir.path(),
            "ARCHITECTURE-SPINE.md",
            "# ARCHITECTURE-SPINE\n\
             \n\
             ### AD-1. Единый брокер сообщений\n\
             - Binds: интеграционный контур\n\
             - Prevents: point-to-point связность\n\
             - Rule: все события — через брокер\n\
             \n\
             ### AD-2. Хранилище\n\
             - Binds:\n\
             - Rule: см. AD-99\n\
             \n\
             ### AD-1. Повторное определение\n\
             - Binds: x\n\
             - Prevents: y\n\
             - Rule: z\n\
             \n\
             ### AD-3. Версии зависимостей\n\
             - Binds: стек\n\
             - Prevents: дрейф версий\n\
             - Rule: все зависимости пинуются\n\
             - kafka-client = \"latest\"  # TODO запиновать\n",
        );
        let issues = lint_spine(&spine).unwrap();
        let count = |rule: &str| issues.iter().filter(|i| i.rule == rule).count();
        assert_eq!(
            count("dup_ad_id"),
            1,
            "должен найти повтор AD-1: {issues:?}"
        );
        assert_eq!(
            count("empty_field"),
            2,
            "AD-2: пустой Binds + нет Prevents: {issues:?}"
        );
        assert_eq!(count("stub_marker"), 1, "TODO: {issues:?}");
        assert_eq!(count("unpinned_version"), 1, "latest: {issues:?}");
        assert_eq!(count("broken_ad_ref"), 1, "AD-99 не определён: {issues:?}");
        assert!(
            issues
                .iter()
                .all(|i| i.severity == "error" || i.severity == "warn")
        );
        assert!(
            issues
                .iter()
                .any(|i| i.rule == "broken_ad_ref" && i.message.contains("AD-99"))
        );
        assert!(issues.iter().all(|i| i.line > 0));
    }

    #[test]
    fn spine_clean_file_has_no_issues() {
        let dir = tempfile::tempdir().unwrap();
        let spine = write_file(
            dir.path(),
            "SPINE.md",
            "### AD-1. Брокер\n\
             - Binds: контур\n\
             - Prevents: хаос\n\
             - Rule: только через брокер\n\
             \n\
             ### AD-2. Хранилище\n\
             - Binds: данные\n\
             - Prevents: дубли\n\
             - Rule: одно хранилище, см. AD-1\n",
        );
        let issues = lint_spine(&spine).unwrap();
        assert!(issues.is_empty(), "чистый spine: {issues:?}");
    }

    #[test]
    fn sensors_pass_and_fail_per_file() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            dir.path(),
            "good.md",
            "# Спека\n\
             \n\
             ## Проблема\n\
             текст\n\
             \n\
             ## Критерии приёмки\n\
             текст\n\
             \n\
             ## Риски\n\
             текст\n\
             \n\
             Связано: [вторая спека](bad.md), [сайт](https://example.com), [якорь](#риски)\n",
        );
        write_file(
            dir.path(),
            "bad.md",
            "# Плохая спека\n\
             \n\
             ## Проблема\n\
             только проблема, ссылка [битая](missing.md)\n",
        );
        write_file(
            dir.path(),
            "ignored.txt",
            "## Проблема\nне md — игнорируем\n",
        );

        let results = sensors_check(dir.path()).unwrap();
        assert_eq!(results.len(), 4, "2 md-файла × 2 сенсора: {results:?}");
        let find = |file: &str, sensor: &str| {
            results
                .iter()
                .find(|r| r.file.ends_with(file) && r.sensor == sensor)
                .unwrap()
        };
        assert!(find("good.md", "required_sections").passed);
        assert!(find("good.md", "upstream_coverage").passed);
        let bad_sections = find("bad.md", "required_sections");
        assert!(!bad_sections.passed);
        assert!(bad_sections.details.contains("## Риски"));
        assert!(bad_sections.details.contains("## Критерии приёмки"));
        let bad_links = find("bad.md", "upstream_coverage");
        assert!(!bad_links.passed);
        assert!(bad_links.details.contains("missing.md"));
    }

    #[test]
    fn fitness_all_rule_types() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        write_file(&repo, "src/main.rs", "fn main() { println!(\"hi\"); }\n");
        write_file(
            &repo,
            "src/lib.rs",
            "pub fn f() {}\n// unsafe тут запрещён\n",
        );
        write_file(&repo, "Cargo.toml", "[package]\nname = \"x\"\n");
        let constraints = write_file(
            dir.path(),
            "CONSTRAINTS.yaml",
            "rules:\n\
             \x20 - name: has_main\n\
             \x20   type: must_contain\n\
             \x20   glob: '**/*.rs'\n\
             \x20   pattern: 'fn main'\n\
             \x20 - name: no_such_token\n\
             \x20   type: must_contain\n\
             \x20   glob: '**/*.rs'\n\
             \x20   pattern: 'zzqwxv_never'\n\
             \x20 - name: no_unsafe\n\
             \x20   type: must_not_contain\n\
             \x20   glob: '**/*.rs'\n\
             \x20   pattern: '\\bunsafe\\b'\n\
             \x20   severity: warn\n\
             \x20 - name: cargo_toml_exists\n\
             \x20   type: file_exists\n\
             \x20   path: Cargo.toml\n\
             \x20 - name: readme_exists\n\
             \x20   type: file_exists\n\
             \x20   path: README.md\n\
             \x20 - name: cmd_true\n\
             \x20   type: command_succeeds\n\
             \x20   command: 'true'\n\
             \x20 - name: cmd_false\n\
             \x20   type: command_succeeds\n\
             \x20   command: 'false'\n",
        );
        let report = check(&repo, &constraints).unwrap();
        assert_eq!(
            report.summary,
            "Правил: 7, нарушений: 4 (error: 3, warn: 1)"
        );
        assert!(!report.passed);
        let by_rule = |name: &str| report.issues.iter().find(|i| i.rule == name).unwrap();
        assert_eq!(by_rule("no_such_token").line, 0);
        assert_eq!(by_rule("no_such_token").severity, "error");
        let unsafe_issue = by_rule("no_unsafe");
        assert_eq!(unsafe_issue.severity, "warn");
        assert_eq!(unsafe_issue.line, 2, "unsafe на второй строке lib.rs");
        assert!(unsafe_issue.file.ends_with("src/lib.rs"));
        assert_eq!(by_rule("readme_exists").severity, "error");
        assert!(by_rule("cmd_false").message.contains("неуспешно"));
        assert!(report.issues.iter().all(|i| i.rule != "has_main"
            && i.rule != "cargo_toml_exists"
            && i.rule != "cmd_true"));
    }

    #[test]
    fn fitness_skips_handoff_packet_and_junk_dirs() {
        // Разрыв P2 «fitness целится в документ решения»: правило с широким
        // glob срабатывало на текст spine внутри пакета. Служебные каталоги
        // (.arch-handoff, node_modules, __pycache__, …) исключены из обхода.
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        write_file(&repo, "src/bad.py", "print('hi')\n");
        write_file(&repo, ".arch-handoff/TASK.md", "Контекст: print( запрещён в проде\n");
        write_file(&repo, "node_modules/pkg/index.js", "print('junk')\n");
        let constraints = write_file(
            dir.path(),
            "CONSTRAINTS.yaml",
            "rules:\n\
             \x20 - name: no_print\n\
             \x20   type: must_not_contain\n\
             \x20   glob: '**/*'\n\
             \x20   pattern: 'print\\('\n",
        );
        let report = check(&repo, &constraints).unwrap();
        assert_eq!(report.issues.len(), 1, "только код, не пакет: {:?}", report.issues);
        assert!(report.issues[0].file.ends_with("src/bad.py"));
    }

    #[test]
    fn fitness_passes_when_all_rules_hold() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        write_file(&repo, "src/main.rs", "fn main() {}\n");
        let constraints = write_file(
            dir.path(),
            "CONSTRAINTS.yaml",
            "rules:\n\
             \x20 - name: has_main\n\
             \x20   type: must_contain\n\
             \x20   glob: '**/*.rs'\n\
             \x20   pattern: 'fn main'\n\
             \x20 - name: cmd_ok\n\
             \x20   type: command_succeeds\n\
             \x20   command: 'true'\n\
             \x20   timeout_secs: 5\n",
        );
        let report = check(&repo, &constraints).unwrap();
        assert!(report.passed);
        assert!(report.issues.is_empty());
        assert_eq!(
            report.summary,
            "Правил: 2, нарушений: 0 (error: 0, warn: 0)"
        );
    }

    #[test]
    fn fitness_command_timeout_fails_rule() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let constraints = write_file(
            dir.path(),
            "CONSTRAINTS.yaml",
            "rules:\n\
             \x20 - name: slow\n\
             \x20   type: command_succeeds\n\
             \x20   command: 'sleep 5'\n\
             \x20   timeout_secs: 1\n",
        );
        let started = std::time::Instant::now();
        let report = check(&repo, &constraints).unwrap();
        assert!(!report.passed);
        assert_eq!(report.issues.len(), 1);
        assert!(report.issues[0].message.contains("таймаут"));
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "таймаут должен убить команду раньше её завершения"
        );
    }

    #[test]
    fn fitness_rejects_invalid_constraints() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let bad_yaml = write_file(dir.path(), "bad.yaml", "rules: [\n");
        assert!(check(&repo, &bad_yaml).is_err(), "битый YAML — ошибка");
        let bad_severity = write_file(
            dir.path(),
            "sev.yaml",
            "rules:\n\
             \x20 - name: x\n\
             \x20   type: file_exists\n\
             \x20   path: a.txt\n\
             \x20   severity: fatal\n",
        );
        assert!(
            check(&repo, &bad_severity).is_err(),
            "неизвестный severity — ошибка"
        );
        assert!(
            check(&repo, &repo.join("missing.yaml")).is_err(),
            "несуществующий constraints — ошибка"
        );
    }

    #[test]
    fn adr_new_numbers_sequentially() {
        let dir = tempfile::tempdir().unwrap();
        let adr_dir = dir.path().join("docs/adr"); // каталога ещё нет — должен создаться
        let first = adr_new(&adr_dir, "API Contract Change").unwrap();
        assert_eq!(
            first.file_name().unwrap().to_string_lossy(),
            "ADR-001-api-contract-change.md"
        );
        let content = std::fs::read_to_string(&first).unwrap();
        for needle in [
            "# ADR-001. API Contract Change",
            "- Status: Proposed",
            "## Context",
            "## Decision",
            "## Alternatives Considered",
            "### Positive",
            "### Negative",
            "## Reversibility",
            "reversible | costly | irreversible",
            "## References",
            "<!--",
        ] {
            assert!(content.contains(needle), "в шаблоне нет '{needle}'");
        }
        let second = adr_new(&adr_dir, "Шина событий").unwrap();
        assert_eq!(
            second.file_name().unwrap().to_string_lossy(),
            "ADR-002-shina-sobytiy.md",
            "кириллица транслитерируется (ADR-002)"
        );
    }

    #[test]
    fn kebab_slug_transliterates_cyrillic() {
        assert_eq!(
            kebab_slug("Сегментация доверенных зон (4 зоны)"),
            "segmentaciya-doverennyh-zon-4-zony"
        );
        assert_eq!(
            kebab_slug("Стратегия идемпотентности на точках входа"),
            "strategiya-idempotentnosti-na-tochkah-vhoda"
        );
        // ъ/ь опускаются без дефиса, ё → yo, щ → sch
        assert_eq!(kebab_slug("Подъём щёточный"), "podyom-schyotochnyy");
    }

    #[test]
    fn kebab_slug_mixed_latin_cyrillic() {
        assert_eq!(kebab_slug("Outbox паттерн"), "outbox-pattern");
        assert_eq!(
            kebab_slug("ADR для async рельсов"),
            "adr-dlya-async-relsov"
        );
    }

    #[test]
    fn kebab_slug_empty_falls_back_to_adr() {
        assert_eq!(kebab_slug(""), "adr");
        assert_eq!(kebab_slug("!!! ..."), "adr");
    }

    #[test]
    fn glob_matcher_cases() {
        assert!(glob_matches("**/*.rs", "src/main.rs"));
        assert!(
            glob_matches("**/*.rs", "main.rs"),
            "** матчит ноль сегментов"
        );
        assert!(glob_matches("*.md", "a.md"));
        assert!(!glob_matches("*.md", "docs/a.md"));
        assert!(glob_matches("docs/**", "docs/a/b.txt"));
        assert!(glob_matches("src/*/mod.rs", "src/foo/mod.rs"));
        assert!(!glob_matches("src/*/mod.rs", "src/foo/bar/mod.rs"));
        assert!(glob_matches("plain/path.txt", "plain/path.txt"));
        assert!(!glob_matches("plain/path.txt", "plain/other.txt"));
        assert!(segment_matches("f?o.rs", "foo.rs"));
        assert!(!segment_matches("f?o.rs", "fo.rs"));
        assert!(segment_matches("*", "anything"));
    }

    #[test]
    fn tools_expose_four_domain_specs() {
        let mut names: Vec<String> = tools().iter().map(|t| t.spec().name.clone()).collect();
        names.sort();
        assert_eq!(
            names,
            [
                "adr_new",
                "fitness_check",
                "significance_score",
                "spine_lint"
            ]
        );
    }

    #[tokio::test]
    async fn significance_tool_scores_via_call() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(
            dir.path().to_path_buf(),
            Arc::new(crate::config::Config::default()),
        );
        let tool = SignificanceScoreTool;
        let out = tool
            .call(
                json!({"triggers": {"new_component": true, "new_vendor": true, "exotic": true}}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{:?}", out.content);
        assert!(out.content.contains("Standard"), "{}", out.content);
        assert!(
            out.content.contains("exotic"),
            "неизвестный триггер подсвечен: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn spine_tool_reports_findings() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "SPINE.md", "### AD-1. X\n- Binds:\n");
        let ctx = ToolContext::new(
            dir.path().to_path_buf(),
            Arc::new(crate::config::Config::default()),
        );
        let tool = SpineLintTool;
        let out = tool.call(json!({"path": "SPINE.md"}), &ctx).await.unwrap();
        assert!(!out.is_error);
        assert!(out.content.contains("empty_field"), "{}", out.content);
        let err = tool.call(json!({"path": "nope.md"}), &ctx).await.unwrap();
        assert!(err.is_error, "несуществующий файл — is_error");
    }
}
