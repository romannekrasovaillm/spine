//! Локальная база знаний архитектора: поиск по файловой системе.
//!
//! КОНТРАКТ (владелец: агент `web` — общий с web.rs):
//! - [`search`] — обходит `KnowledgeConfig::dirs` (walkdir), фильтрует по
//!   расширениям, ищет: точные вхождения (бонус), fuzzy по терминам запроса
//!   (fuzzy-matcher), ранжирование; возвращает хиты со сниппетом (±2 строки
//!   контекста, подсветка маркерами `>>>`);
//! - файлы >5 МБ индексируются только по имени; битый UTF-8 — lossy;
//!   скрытые каталоги и `target/.git/node_modules` пропускаются;
//! - поиск по именам файлов тоже (бонус к скору).

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use walkdir::WalkDir;

use crate::error::{HarnessError, Result};
use crate::llm::ToolSpec;
use crate::tool::{Tool, ToolContext, ToolOutput};

/// Файлы больше этого размера индексируются только по имени.
const MAX_CONTENT_BYTES: u64 = 5 * 1024 * 1024;
/// Максимум учитываемых вхождений термина на файл.
const MAX_TERM_OCCURRENCES: usize = 50;
/// Строк контекста вокруг совпадения в сниппете.
const CONTEXT_LINES: usize = 2;
/// Максимальная длина строки сниппета (символов).
const MAX_SNIPPET_LINE: usize = 240;

/// Хит локального поиска.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KbHit {
    /// Путь к файлу.
    pub path: PathBuf,
    /// Номер строки совпадения (1-based; 0 — совпадение по имени файла).
    pub line: usize,
    /// Оценка релевантности (больше — лучше).
    pub score: f64,
    /// Сниппет с контекстом.
    pub snippet: String,
}

/// Поиск по локальной базе знаний.
///
/// Обход выполняется в `spawn_blocking`, чтобы не блокировать runtime.
/// Недоступные каталоги и нечитаемые файлы пропускаются.
///
/// # Errors
/// Фоновой обход прерван (`JoinError`).
pub async fn search(
    dirs: &[PathBuf],
    exts: &[String],
    query: &str,
    limit: usize,
) -> Result<Vec<KbHit>> {
    let dirs = dirs.to_vec();
    let exts = exts.to_vec();
    let query = query.to_string();
    tokio::task::spawn_blocking(move || search_blocking(&dirs, &exts, &query, limit))
        .await
        .map_err(|e| HarnessError::Kb(format!("обход базы знаний прерван: {e}")))
}

/// Инструменты домена: `kb_search`.
#[must_use]
pub fn tools() -> Vec<Arc<dyn Tool>> {
    vec![Arc::new(KbSearchTool)]
}

/// Синхронный обход и ранжирование (вызывается из `spawn_blocking`).
fn search_blocking(dirs: &[PathBuf], exts: &[String], query: &str, limit: usize) -> Vec<KbHit> {
    let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if terms.is_empty() {
        return Vec::new();
    }
    let exts: Vec<String> = exts
        .iter()
        .map(|e| e.trim_start_matches('.').to_lowercase())
        .collect();
    let matcher = SkimMatcherV2::default();

    let mut hits = Vec::new();
    for dir in dirs {
        let walker = WalkDir::new(dir)
            .follow_links(false)
            .into_iter()
            .filter_entry(is_searchable);
        for entry in walker.filter_map(std::result::Result::ok) {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            if !exts.is_empty() && !has_allowed_extension(path, &exts) {
                continue;
            }
            collect_file_hits(path, &terms, &matcher, &mut hits);
        }
    }

    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.line.cmp(&b.line))
    });
    let mut seen = HashSet::new();
    hits.retain(|h| seen.insert((h.path.clone(), h.line)));
    hits.truncate(limit);
    hits
}

/// Пропускает скрытые каталоги/файлы и служебные каталоги сборки.
fn is_searchable(entry: &walkdir::DirEntry) -> bool {
    if entry.depth() == 0 {
        return true;
    }
    let name = entry.file_name().to_string_lossy();
    !name.starts_with('.') && name != "target" && name != "node_modules"
}

/// Расширение файла входит в список допустимых (сравнение без учёта регистра).
fn has_allowed_extension(path: &Path, exts: &[String]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| exts.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

/// Скоринг одного файла (имя + содержимое); хиты добавляются в `hits`.
fn collect_file_hits(
    path: &Path,
    terms: &[String],
    matcher: &SkimMatcherV2,
    hits: &mut Vec<KbHit>,
) {
    let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
        return;
    };
    let name_lower = file_name.to_lowercase();

    // Баллы за имя: +25 за каждый термин в имени, fuzzy-бонус до +30.
    let mut score: u64 = 0;
    let mut name_matched = false;
    for term in terms {
        if name_lower.contains(term.as_str()) {
            score += 25;
            name_matched = true;
        }
        if let Some(fuzzy) = matcher.fuzzy_match(file_name, term) {
            score += u64::try_from(fuzzy.clamp(0, 30)).unwrap_or(0);
        }
    }

    // Содержимое: большие файлы — только матч имени.
    let small_enough = std::fs::metadata(path).is_ok_and(|m| m.len() <= MAX_CONTENT_BYTES);
    let content = if small_enough {
        std::fs::read(path)
            .ok()
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    } else {
        None
    };

    // Баллы за текст: +10 за вхождение термина (не более MAX_TERM_OCCURRENCES на термин).
    let mut match_lines: Vec<usize> = Vec::new();
    if let Some(text) = &content {
        let lower = text.to_lowercase();
        for term in terms {
            let occ = lower
                .matches(term.as_str())
                .count()
                .min(MAX_TERM_OCCURRENCES);
            score += (occ * 10) as u64;
        }
        for (idx, line) in text.lines().enumerate() {
            let line_lower = line.to_lowercase();
            if terms.iter().any(|t| line_lower.contains(t.as_str())) {
                match_lines.push(idx);
            }
        }
    }

    if match_lines.is_empty() {
        if name_matched {
            hits.push(KbHit {
                path: path.to_path_buf(),
                line: 0,
                score: to_score(score),
                snippet: format!("совпадение по имени файла: {file_name}"),
            });
        }
        return;
    }

    // content гарантированно Some: match_lines непуст
    let text = content.unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    // Соседние совпадения (зазор не больше двух контекстов) сливаются в один хит.
    let mut group_start = 0usize;
    for i in 1..=match_lines.len() {
        let same_group =
            i < match_lines.len() && match_lines[i] - match_lines[i - 1] <= 2 * CONTEXT_LINES + 1;
        if same_group {
            continue;
        }
        hits.push(make_hit(
            path,
            &lines,
            &match_lines[group_start..i],
            terms,
            score,
        ));
        group_start = i;
    }
}

/// Хит по группе соседних совпадений: лучшая строка + сниппет с контекстом.
fn make_hit(path: &Path, lines: &[&str], group: &[usize], terms: &[String], score: u64) -> KbHit {
    let best = best_line(lines, group, terms);
    let start = group
        .first()
        .copied()
        .unwrap_or(best)
        .saturating_sub(CONTEXT_LINES);
    let end = group
        .last()
        .copied()
        .unwrap_or(best)
        .saturating_add(CONTEXT_LINES)
        .min(lines.len().saturating_sub(1));
    let group_set: HashSet<usize> = group.iter().copied().collect();

    let mut snippet = String::new();
    for (idx, line) in lines.iter().enumerate().take(end + 1).skip(start) {
        let marker = if group_set.contains(&idx) {
            ">>> "
        } else {
            "    "
        };
        // записи в String инфаллибильны
        let _ = writeln!(snippet, "{marker}{}", truncate_line(line.trim_end()));
    }
    KbHit {
        path: path.to_path_buf(),
        line: best + 1,
        score: to_score(score),
        snippet: snippet.trim_end().to_string(),
    }
}

/// Лучшая строка группы: максимум различных терминов; при равенстве — первая.
fn best_line(lines: &[&str], group: &[usize], terms: &[String]) -> usize {
    let mut best = group.first().copied().unwrap_or(0);
    let mut best_count = 0usize;
    for &idx in group {
        let Some(line) = lines.get(idx) else {
            continue;
        };
        let lower = line.to_lowercase();
        let count = terms.iter().filter(|t| lower.contains(t.as_str())).count();
        if count > best_count {
            best_count = count;
            best = idx;
        }
    }
    best
}

/// Усекает строку сниппета до [`MAX_SNIPPET_LINE`] символов.
fn truncate_line(line: &str) -> String {
    if line.chars().count() <= MAX_SNIPPET_LINE {
        line.to_string()
    } else {
        let cut: String = line.chars().take(MAX_SNIPPET_LINE).collect();
        format!("{cut}…")
    }
}

/// u64 → f64 без потери точности для реалистичных скоров (насыщение на `u32::MAX`).
fn to_score(score: u64) -> f64 {
    f64::from(u32::try_from(score).unwrap_or(u32::MAX))
}

/// Аргументы инструмента `kb_search`.
#[derive(Debug, Deserialize)]
struct KbSearchArgs {
    /// Поисковый запрос (термины через пробел).
    query: String,
    /// Максимум хитов (по умолчанию 10, не больше 20).
    limit: Option<usize>,
}

/// Инструмент `kb_search`: поиск по локальной базе знаний из конфига.
struct KbSearchTool;

#[async_trait]
impl Tool for KbSearchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "kb_search".into(),
            description: "Поиск по локальной базе знаний архитектора (каталоги knowledge.dirs из конфига). Возвращает ранжированные хиты: путь, строка, сниппет с контекстом.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Поисковый запрос: термины через пробел"},
                    "limit": {"type": "integer", "description": "Максимум хитов (по умолчанию 10, не больше 20)", "default": 10}
                },
                "required": ["query"]
            }),
        }
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let args: KbSearchArgs = serde_json::from_value(args)
            .map_err(|e| HarnessError::Tool(format!("kb_search: невалидные аргументы: {e}")))?;
        let limit = args.limit.unwrap_or(10).min(20);
        let kb = &ctx.config.knowledge;
        let hits = search(&kb.dirs, &kb.extensions, &args.query, limit).await?;
        if hits.is_empty() {
            return Ok(ToolOutput::ok(format!(
                "По запросу «{}» в базе знаний ничего не найдено.",
                args.query
            )));
        }
        let mut buf = String::new();
        for hit in &hits {
            // записи в String инфаллибильны
            let _ = writeln!(
                buf,
                "── {}:{} (score {:.1})",
                hit.path.display(),
                hit.line,
                hit.score
            );
            let _ = writeln!(buf, "{}", hit.snippet);
        }
        Ok(ToolOutput::ok(buf))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Корпус из трёх md-файлов во временном каталоге.
    fn corpus() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let write = |name: &str, content: &str| {
            std::fs::write(dir.path().join(name), content).expect("write");
        };
        write(
            "adr-042-kafka.md",
            "# ADR-042\n\nБрокер сообщений.\nKafka выбран как шина событий.\nИтог: kafka в проде.\n",
        );
        write(
            "integration-notes.md",
            "# Интеграция\n\nKafka для обмена событиями.\nИтог: kafka в проде.\n",
        );
        write("readme.md", "# Общее\n\nНичего про брокеров.\n");
        dir
    }

    #[tokio::test]
    async fn ranks_filename_match_above_content_match() {
        let dir = corpus();
        let hits = search(&[dir.path().to_path_buf()], &["md".into()], "kafka", 10)
            .await
            .expect("search");
        assert_eq!(hits.len(), 2);
        let top = hits.first().expect("top hit");
        assert_eq!(
            top.path.file_name().and_then(|n| n.to_str()),
            Some("adr-042-kafka.md")
        );
        assert!(top.score > hits[1].score);
    }

    #[tokio::test]
    async fn snippet_marks_match_line_and_context() {
        let dir = corpus();
        let hits = search(&[dir.path().to_path_buf()], &["md".into()], "kafka", 10)
            .await
            .expect("search");
        let hit = hits
            .iter()
            .find(|h| h.path.ends_with("adr-042-kafka.md"))
            .expect("hit");
        assert_eq!(hit.line, 4);
        assert!(
            hit.snippet.contains(">>> Kafka выбран как шина событий."),
            "сниппет: {}",
            hit.snippet
        );
        assert!(
            hit.snippet.contains("    Брокер сообщений."),
            "контекст без маркера: {}",
            hit.snippet
        );
    }

    #[tokio::test]
    async fn respects_limit() {
        let dir = corpus();
        let hits = search(&[dir.path().to_path_buf()], &["md".into()], "kafka", 1)
            .await
            .expect("search");
        assert_eq!(hits.len(), 1);
    }

    #[tokio::test]
    async fn skips_hidden_dirs_and_foreign_extensions() {
        let dir = corpus();
        std::fs::write(dir.path().join("code.rs"), "fn kafka() {}").expect("write rs");
        let hidden = dir.path().join(".hidden");
        std::fs::create_dir(&hidden).expect("mkdir");
        std::fs::write(hidden.join("secret.md"), "kafka в скрытом каталоге").expect("write hidden");
        let hits = search(&[dir.path().to_path_buf()], &["md".into()], "kafka", 10)
            .await
            .expect("search");
        assert!(
            hits.iter()
                .all(|h| h.path.extension().and_then(|e| e.to_str()) == Some("md"))
        );
        assert!(
            hits.iter()
                .all(|h| !h.path.to_string_lossy().contains(".hidden"))
        );
    }

    #[tokio::test]
    async fn empty_query_returns_no_hits() {
        let dir = corpus();
        let hits = search(&[dir.path().to_path_buf()], &["md".into()], "   ", 10)
            .await
            .expect("search");
        assert!(hits.is_empty());
    }

    #[test]
    fn exposes_kb_search_tool() {
        let tools = tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].spec().name, "kb_search");
    }
}
