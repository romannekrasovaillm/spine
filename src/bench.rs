//! Специализированные архитектурные бенчмарки (solution architecture).
//!
//! КОНТРАКТ (владелец: агент `rubric` — общий с rubric.rs):
//! - [`Benchmark`] — YAML-сценарий: имя, описание, постановка задачи
//!   (system+user промпты), ссылка на рубрику оценки, проходной порог;
//! - [`run`] — прогон сценария на модели, оценка ответа рубрикой
//!   ([`crate::rubric::evaluate_with_options`]), запись отчёта в out_dir (md+json);
//! - golden-set (`assets/benchmarks/golden/`): синтетические документы
//!   `<имя>.md` + эталонные оценки `<имя>.expected.yaml` ([`GoldenExpectation`]);
//!   [`run_golden`] — прогон судьи по набору, согласие с эталоном — MAE
//!   ([`mean_absolute_error`]); регрессионный порог — на стороне CLI (ADR-004).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::JudgeConfig;
use crate::error::{HarnessError, Result};
use crate::llm::{ChatMessage, ChatRequest, LlmProvider};

/// Сценарий бенчмарка.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Benchmark {
    /// Имя сценария.
    pub name: String,
    /// Описание: что измеряет.
    pub description: String,
    /// Системный промпт (роль solution-архитектора).
    pub system_prompt: String,
    /// Постановка задачи.
    pub task: String,
    /// Файл рубрики (относительно assets/rubrics или абсолютный).
    pub rubric: String,
    /// Проходной взвешенный порог (0..=scale_max).
    pub pass_threshold: f64,
    /// Теги (integration, adr, nfr, brownfield, …).
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Сводка для списка.
#[derive(Debug, Clone)]
pub struct BenchSummary {
    /// Путь к YAML.
    pub path: PathBuf,
    /// Имя.
    pub name: String,
    /// Описание.
    pub description: String,
    /// Теги.
    pub tags: Vec<String>,
}

/// Отчёт о прогоне.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchReport {
    /// Имя бенчмарка.
    pub bench_name: String,
    /// Модель-испытуемый.
    pub model: String,
    /// Ответ модели (полный текст).
    pub response: String,
    /// Отчёт рубрики (сериализованный).
    pub rubric_report: crate::rubric::RubricReport,
    /// Прошёл ли порог.
    pub passed: bool,
}

/// Эталонные оценки golden-документа (`<имя>.expected.yaml` рядом с `<имя>.md`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoldenExpectation {
    /// Имя рубрики: файл в assets/rubrics (расширение `.yaml` опционально).
    pub rubric: String,
    /// Ожидаемые баллы по критериям (id → 1..=scale_max).
    pub scores: BTreeMap<String, u8>,
}

/// Отчёт по одному golden-документу.
#[derive(Debug, Clone)]
pub struct GoldenCaseReport {
    /// Имя файла документа.
    pub doc: String,
    /// MAE по критериям эталона этого документа.
    pub mae: f64,
    /// Сколько пар «судья × эталон» сравнено.
    pub compared: usize,
}

/// Отчёт golden-прогона судьи (ADR-004).
#[derive(Debug, Clone)]
pub struct GoldenReport {
    /// Модель-судья.
    pub judge_model: String,
    /// Разбор по документам.
    pub cases: Vec<GoldenCaseReport>,
    /// Итоговый MAE по всем парам «документ × критерий».
    pub mae: f64,
    /// Всего сравненных пар.
    pub compared: usize,
}

/// Загружает бенчмарк из YAML.
///
/// # Errors
/// Файл не читается / не валиден.
pub fn load(path: &Path) -> Result<Benchmark> {
    let text = std::fs::read_to_string(path).map_err(|e| HarnessError::io(path, e))?;
    let bench: Benchmark = serde_yaml::from_str(&text)?;
    Ok(bench)
}

/// Список бенчмарков каталога (`*.yaml`/`*.yml`); битые файлы пропускаются.
///
/// # Errors
/// Каталог не читается.
pub fn list(dir: &Path) -> Result<Vec<BenchSummary>> {
    let entries = std::fs::read_dir(dir).map_err(|e| HarnessError::io(dir, e))?;
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let is_yaml = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("yaml") || e.eq_ignore_ascii_case("yml"));
        if !is_yaml {
            continue;
        }
        // Битый файл — не ошибка каталога: пропускаем.
        if let Ok(bench) = load(&path) {
            out.push(BenchSummary {
                path,
                name: bench.name,
                description: bench.description,
                tags: bench.tags,
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Прогоняет бенчмарк на модели и оценивает рубрикой; пишет отчёты в out_dir.
///
/// Ответ модели оценивается тем же провайдером (судья = испытуемая модель),
/// с настройками судьи `judge` (k сэмплов, верификация цитат — ADR-004).
/// Отчёты: `bench-<name>-<model>-<yyyymmdd-hhmmss>.md` (задача, ответ, таблица
/// рубрики, PASS/FAIL) и `.json` (сериализованный [`BenchReport`]).
///
/// # Errors
/// Ошибка модели/судьи/записи.
pub async fn run(
    bench: &Benchmark,
    provider: &dyn LlmProvider,
    rubrics_dir: &Path,
    out_dir: &Path,
    judge: &JudgeConfig,
) -> Result<BenchReport> {
    let request = ChatRequest::chat(vec![
        ChatMessage::system(bench.system_prompt.clone()),
        ChatMessage::user(bench.task.clone()),
    ]);
    let response = provider.complete(request).await?.content;

    let rubric_path = {
        let p = PathBuf::from(&bench.rubric);
        if p.is_absolute() {
            p
        } else {
            rubrics_dir.join(p)
        }
    };
    let rubric = crate::rubric::load(&rubric_path)?;
    // Судья — та же модель, что и испытуемый.
    let rubric_report = crate::rubric::evaluate_with_options(&rubric, &response, provider, judge).await?;
    let passed = rubric_report.weighted_total >= bench.pass_threshold;
    let report = BenchReport {
        bench_name: bench.name.clone(),
        model: provider.model().to_string(),
        response,
        rubric_report,
        passed,
    };

    std::fs::create_dir_all(out_dir).map_err(|e| HarnessError::io(out_dir, e))?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let base = format!(
        "bench-{}-{}-{stamp}",
        sanitize_file_part(&bench.name),
        sanitize_file_part(provider.model())
    );
    let md_path = out_dir.join(format!("{base}.md"));
    std::fs::write(&md_path, report_markdown(bench, &report)).map_err(|e| HarnessError::io(&md_path, e))?;
    let json_path = out_dir.join(format!("{base}.json"));
    let json = serde_json::to_string_pretty(&report)?;
    std::fs::write(&json_path, json).map_err(|e| HarnessError::io(&json_path, e))?;
    Ok(report)
}

/// Загружает golden-set каталога: пары «`<имя>.md` + `<имя>.expected.yaml».
///
/// В отличие от [`list`], битый эталон — ошибка, а не пропуск: молчаливо
/// потерянный документ завышал бы измеренное согласие судьи с эталоном.
///
/// # Errors
/// Каталог не читается; эталон не парсится / без оценок / с баллом 0;
/// документ к эталону отсутствует.
pub fn load_golden(dir: &Path) -> Result<Vec<(PathBuf, GoldenExpectation)>> {
    let entries = std::fs::read_dir(dir).map_err(|e| HarnessError::io(dir, e))?;
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(stem) = name.strip_suffix(".expected.yaml") else {
            continue;
        };
        let text = std::fs::read_to_string(&path).map_err(|e| HarnessError::io(&path, e))?;
        let expectation: GoldenExpectation = serde_yaml::from_str(&text)
            .map_err(|e| HarnessError::Bench(format!("{}: разбор эталона: {e}", path.display())))?;
        if expectation.scores.is_empty() {
            return Err(HarnessError::Bench(format!(
                "{}: эталон без оценок",
                path.display()
            )));
        }
        if let Some((id, _)) = expectation.scores.iter().find(|(_, s)| **s == 0) {
            return Err(HarnessError::Bench(format!(
                "{}: критерий '{id}' — балл 0 вне шкалы",
                path.display()
            )));
        }
        let doc = dir.join(format!("{stem}.md"));
        if !doc.is_file() {
            return Err(HarnessError::Bench(format!(
                "{}: нет документа к эталону {}",
                doc.display(),
                path.display()
            )));
        }
        out.push((doc, expectation));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// Прогоняет судью по golden-set и считает согласие с эталоном (MAE, ADR-004).
///
/// Каждый документ оценивается полным контуром
/// [`crate::rubric::evaluate_with_options`] рубрикой из его эталона; MAE —
/// среднее |судья − эталон| по всем парам «документ × критерий». Эталон,
/// ссылающийся на критерий вне рубрики или на балл выше шкалы, — ошибка
/// данных, а не расхождение судьи.
///
/// # Errors
/// Пустой/битый golden-set, ошибка чтения, модели или рубрики.
pub async fn run_golden(
    provider: &dyn LlmProvider,
    rubrics_dir: &Path,
    golden_dir: &Path,
    judge: &JudgeConfig,
) -> Result<GoldenReport> {
    let cases = load_golden(golden_dir)?;
    if cases.is_empty() {
        return Err(HarnessError::Bench(format!(
            "golden-set пуст: в {} нет пар <имя>.md + <имя>.expected.yaml",
            golden_dir.display()
        )));
    }
    let mut all_pairs: Vec<(f64, f64)> = Vec::new();
    let mut case_reports = Vec::with_capacity(cases.len());
    for (doc_path, expectation) in &cases {
        let rubric_path = {
            let direct = rubrics_dir.join(&expectation.rubric);
            if direct.is_file() {
                direct
            } else {
                rubrics_dir.join(format!("{}.yaml", expectation.rubric))
            }
        };
        let rubric = crate::rubric::load(&rubric_path)?;
        let text = std::fs::read_to_string(doc_path).map_err(|e| HarnessError::io(doc_path, e))?;
        let report = crate::rubric::evaluate_with_options(&rubric, &text, provider, judge).await?;
        let mut pairs = Vec::with_capacity(expectation.scores.len());
        for (criterion_id, &expected) in &expectation.scores {
            if expected > rubric.scale_max {
                return Err(HarnessError::Bench(format!(
                    "эталон {}: критерий '{criterion_id}' — балл {expected} выше шкалы {}",
                    doc_path.display(),
                    rubric.scale_max
                )));
            }
            let got = report
                .scores
                .iter()
                .find(|s| &s.criterion_id == criterion_id)
                .map(|s| s.score)
                .ok_or_else(|| {
                    HarnessError::Bench(format!(
                        "эталон {} ссылается на критерий '{criterion_id}', которого нет в рубрике {}",
                        doc_path.display(),
                        rubric.name
                    ))
                })?;
            pairs.push((f64::from(got), f64::from(expected)));
        }
        let case_mae = mean_absolute_error(&pairs).ok_or_else(|| {
            HarnessError::Bench(format!("{}: нет сравненных пар для MAE", doc_path.display()))
        })?;
        all_pairs.extend(pairs.iter().copied());
        case_reports.push(GoldenCaseReport {
            doc: doc_path
                .file_name()
                .map_or_else(|| doc_path.display().to_string(), |n| n.to_string_lossy().into_owned()),
            mae: case_mae,
            compared: pairs.len(),
        });
    }
    let mae = mean_absolute_error(&all_pairs)
        .ok_or_else(|| HarnessError::Bench("golden-set без сравненных пар для MAE".into()))?;
    Ok(GoldenReport {
        judge_model: provider.model().to_string(),
        cases: case_reports,
        mae,
        compared: all_pairs.len(),
    })
}

/// Средняя абсолютная ошибка по парам (факт, эталон); пустой вход — `None`.
pub fn mean_absolute_error(pairs: &[(f64, f64)]) -> Option<f64> {
    if pairs.is_empty() {
        return None;
    }
    let sum: f64 = pairs.iter().map(|(got, want)| (got - want).abs()).sum();
    Some(sum / pairs.len() as f64)
}

/// Заменяет символы, небезопасные в имени файла, на `-`.
fn sanitize_file_part(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Markdown-отчёт прогона: задача, ответ модели, таблица рубрики, PASS/FAIL.
fn report_markdown(bench: &Benchmark, report: &BenchReport) -> String {
    let mut out = String::new();
    let verdict = if report.passed { "PASS" } else { "FAIL" };
    let _ = writeln!(out, "# Бенчмарк «{}» — {}\n", report.bench_name, verdict);
    let _ = writeln!(out, "**Модель:** {}", report.model);
    let _ = writeln!(out, "**Дата:** {}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
    let _ = writeln!(
        out,
        "**Взвешенный итог:** {:.2}/5 (порог {:.2})\n",
        report.rubric_report.weighted_total, bench.pass_threshold
    );
    let _ = writeln!(out, "## Задача\n\n{}\n", bench.task);
    let _ = writeln!(out, "## Ответ модели\n\n{}\n", report.response);
    let _ = writeln!(out, "## Оценка по рубрике\n\n{}\n", report.rubric_report.to_markdown());
    let cmp = if report.passed { ">=" } else { "<" };
    let _ = writeln!(
        out,
        "## Результат\n\n**{}**: взвешенный итог {:.2} {} порог {:.2}.",
        verdict, report.rubric_report.weighted_total, cmp, bench.pass_threshold
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    /// Рубрика-пример для прогонов.
    const RUBRIC_YAML: &str = "\
name: core
description: Базовая рубрика
scale_max: 5
origin: anchor
criteria:
  - id: context
    name: Контекст
    description: Описан контекст
    weight: 1.0
    anchors:
      1: нет
      3: частично
      5: полный
  - id: risks
    name: Риски
    description: Названы риски
    weight: 1.0
";

    /// Бенчмарк-пример, ссылающийся на [`RUBRIC_YAML`] по имени файла.
    const BENCH_YAML: &str = "\
name: adr-basic
description: Написать ADR
system_prompt: Ты solution-архитектор.
task: Напиши ADR миграции платёжного шлюза.
rubric: core.yaml
pass_threshold: 3.0
tags:
  - adr
";

    /// Фейк-провайдер: на задачу отвечает `answer`, на судейский промпт — `judge`.
    #[derive(Debug)]
    struct FakeLlm {
        answer: String,
        judge: String,
    }

    #[async_trait]
    impl LlmProvider for FakeLlm {
        fn name(&self) -> &str {
            "fake"
        }
        fn model(&self) -> &str {
            "fake-model"
        }
        async fn complete(&self, req: ChatRequest) -> Result<ChatMessage> {
            let is_judge = req
                .messages
                .iter()
                .any(|m| m.content.contains("архитектурный судья"));
            let content = if is_judge {
                self.judge.clone()
            } else {
                self.answer.clone()
            };
            Ok(ChatMessage::assistant(content, Vec::new()))
        }
    }

    #[test]
    fn benchmark_yaml_roundtrip() {
        let bench: Benchmark = serde_yaml::from_str(BENCH_YAML).expect("parse");
        let yaml = serde_yaml::to_string(&bench).expect("serialize");
        let back: Benchmark = serde_yaml::from_str(&yaml).expect("reparse");
        assert_eq!(back.name, "adr-basic");
        assert_eq!(back.system_prompt, "Ты solution-архитектор.");
        assert_eq!(back.rubric, "core.yaml");
        assert_eq!(back.tags, vec!["adr".to_string()]);
        assert!((back.pass_threshold - 3.0).abs() < 1e-9);
    }

    #[test]
    fn list_skips_broken_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("good.yaml"), BENCH_YAML).expect("write");
        std::fs::write(dir.path().join("broken.yml"), "{{{").expect("write");
        std::fs::write(dir.path().join("README.md"), "не бенч").expect("write");
        let items = list(dir.path()).expect("list");
        assert_eq!(items.len(), 1, "битый и не-yaml файлы должны быть пропущены");
        assert_eq!(items[0].name, "adr-basic");
        assert_eq!(items[0].tags, vec!["adr".to_string()]);
    }

    #[test]
    fn sanitize_replaces_unsafe_chars() {
        assert_eq!(sanitize_file_part("openai/gpt 5"), "openai-gpt-5");
        assert_eq!(sanitize_file_part("deepseek-chat"), "deepseek-chat");
    }

    /// Полный прогон бенчмарка на фейковой модели: файлы во временном каталоге.
    async fn run_case(judge: &str) -> (tempfile::TempDir, BenchReport, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let rubrics = dir.path().join("rubrics");
        std::fs::create_dir_all(&rubrics).expect("mkdir");
        std::fs::write(rubrics.join("core.yaml"), RUBRIC_YAML).expect("write rubric");
        let bench: Benchmark = serde_yaml::from_str(BENCH_YAML).expect("bench");
        let llm = FakeLlm {
            answer: "ADR-001: мигрируем платёжный шлюз. Контекст: вендор уходит. \
                     Риски: двойная запись, откат."
                .into(),
            judge: judge.into(),
        };
        let out = dir.path().join("out");
        let report = run(&bench, &llm, &rubrics, &out, &JudgeConfig::default())
            .await
            .expect("run");
        (dir, report, out)
    }

    #[tokio::test]
    async fn run_writes_reports_and_passes_above_threshold() {
        // Цитаты в rationale — дословные фрагменты ответа модели (ADR-004).
        let judge = "{\"scores\": [\
             {\"criterion_id\": \"context\", \"score\": 5, \"rationale\": \"Цитата: \\\"Контекст: вендор уходит\\\" — контекст назван\"}, \
             {\"criterion_id\": \"risks\", \"score\": 4, \"rationale\": \"Цитата: \\\"Риски: двойная запись, откат\\\" — риски перечислены\"}], \
             \"verdict\": \"годно\"}";
        let (_dir, report, out) = run_case(judge).await;
        assert!(report.passed, "4.5 >= 3.0");
        assert_eq!(report.bench_name, "adr-basic");
        assert_eq!(report.model, "fake-model");
        assert!((report.rubric_report.weighted_total - 4.5).abs() < 1e-9);

        // На диске — md и json с контрактным именем.
        let files: Vec<PathBuf> = std::fs::read_dir(&out)
            .expect("read_dir")
            .flatten()
            .map(|e| e.path())
            .collect();
        let md = files
            .iter()
            .find(|p| p.extension().is_some_and(|e| e == "md"))
            .expect("md отчёт");
        let json_path = files
            .iter()
            .find(|p| p.extension().is_some_and(|e| e == "json"))
            .expect("json отчёт");
        let file_name = md.file_name().expect("имя файла").to_string_lossy();
        assert!(
            file_name.starts_with("bench-adr-basic-fake-model-"),
            "имя отчёта: {file_name}"
        );
        let md_text = std::fs::read_to_string(md).expect("read md");
        assert!(md_text.contains("PASS"));
        assert!(md_text.contains("ADR-001"), "ответ модели в отчёте");
        assert!(md_text.contains("| Критерий | Вес | Балл | Метки | Обоснование |"));
        let parsed: BenchReport =
            serde_json::from_str(&std::fs::read_to_string(json_path).expect("read json")).expect("parse json");
        assert!(parsed.passed);
        assert_eq!(parsed.response, report.response);
        assert_eq!(parsed.rubric_report.scores.len(), 2);
    }

    #[tokio::test]
    async fn run_fails_below_threshold() {
        let judge = "{\"scores\": [\
             {\"criterion_id\": \"context\", \"score\": 2, \"rationale\": \"Цитата: \\\"Контекст: вендор уходит\\\" — слабо\"}, \
             {\"criterion_id\": \"risks\", \"score\": 2, \"rationale\": \"Цитата: \\\"Риски: двойная запись, откат\\\" — слабо\"}], \
             \"verdict\": \"плохо\"}";
        let (_dir, report, _out) = run_case(judge).await;
        assert!(!report.passed, "2.0 < 3.0");
        assert!(report.rubric_report.weighted_total < 3.0);
    }

    #[test]
    fn mean_absolute_error_math() {
        assert_eq!(mean_absolute_error(&[]), None, "пустой вход — None");
        let mae = mean_absolute_error(&[(4.0, 5.0), (5.0, 5.0), (1.0, 2.0)]).expect("mae");
        assert!((mae - 2.0 / 3.0).abs() < 1e-9, "MAE: {mae}");
    }

    /// Документ golden-фикстуры без свидетельств (эталон — единицы).
    const GOLDEN_BAD_MD: &str = "Проект важен. Дедлайн скоро.";
    /// Эталон к [`GOLDEN_BAD_MD`].
    const GOLDEN_BAD_YAML: &str = "rubric: core\nscores:\n  context: 1\n  risks: 1\n";
    /// Документ golden-фикстуры со свидетельствами (эталон — пятёрки).
    const GOLDEN_GOOD_MD: &str = "Контекст описан полностью. Риски названы и разобраны подробно.";
    /// Эталон к [`GOLDEN_GOOD_MD`].
    const GOLDEN_GOOD_YAML: &str = "rubric: core\nscores:\n  context: 5\n  risks: 5\n";

    /// Фейк-судья с очередью ответов (по одному на прогон оценки).
    #[derive(Debug)]
    struct QueueLlm {
        replies: std::sync::Mutex<std::collections::VecDeque<String>>,
    }

    impl QueueLlm {
        fn new(replies: &[String]) -> Self {
            Self {
                replies: std::sync::Mutex::new(replies.iter().cloned().collect()),
            }
        }
    }

    #[async_trait]
    impl LlmProvider for QueueLlm {
        fn name(&self) -> &str {
            "fake"
        }
        fn model(&self) -> &str {
            "fake-queue"
        }
        async fn complete(&self, _req: ChatRequest) -> Result<ChatMessage> {
            let reply = self
                .replies
                .lock()
                .expect("mutex poisoned")
                .pop_front()
                .unwrap_or_default();
            Ok(ChatMessage::assistant(reply, Vec::new()))
        }
    }

    /// Пишет golden-set (два документа + эталоны) и рубрику во временный каталог.
    fn golden_dirs() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let rubrics = dir.path().join("rubrics");
        let golden = dir.path().join("golden");
        std::fs::create_dir_all(&rubrics).expect("mkdir");
        std::fs::create_dir_all(&golden).expect("mkdir");
        std::fs::write(rubrics.join("core.yaml"), RUBRIC_YAML).expect("rubric");
        std::fs::write(golden.join("bad.md"), GOLDEN_BAD_MD).expect("doc");
        std::fs::write(golden.join("bad.expected.yaml"), GOLDEN_BAD_YAML).expect("exp");
        std::fs::write(golden.join("good.md"), GOLDEN_GOOD_MD).expect("doc");
        std::fs::write(golden.join("good.expected.yaml"), GOLDEN_GOOD_YAML).expect("exp");
        (dir, rubrics, golden)
    }

    /// JSON-ответ судьи для golden-фикстуры.
    fn judge_reply(context: u8, risks: u8, context_rationale: &str, risks_rationale: &str) -> String {
        format!(
            "{{\"scores\": [\
             {{\"criterion_id\": \"context\", \"score\": {context}, \"rationale\": \"{context_rationale}\"}}, \
             {{\"criterion_id\": \"risks\", \"score\": {risks}, \"rationale\": \"{risks_rationale}\"}}], \
             \"verdict\": \"ok\"}}"
        )
    }

    #[test]
    fn load_golden_reads_sorted_pairs() {
        let (_dir, _rubrics, golden) = golden_dirs();
        let cases = load_golden(&golden).expect("load_golden");
        assert_eq!(cases.len(), 2);
        assert!(cases[0].0.ends_with("bad.md"), "сортировка по имени файла");
        assert_eq!(cases[0].1.scores["context"], 1);
        assert_eq!(cases[1].1.rubric, "core");
        assert_eq!(cases[1].1.scores.len(), 2);
    }

    #[test]
    fn load_golden_rejects_broken_inputs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let golden = dir.path().join("golden");
        std::fs::create_dir_all(&golden).expect("mkdir");
        // Эталон без документа — ошибка, а не пропуск.
        std::fs::write(golden.join("orphan.expected.yaml"), GOLDEN_BAD_YAML).expect("write");
        let err = load_golden(&golden).expect_err("эталон-сирота");
        assert!(err.to_string().contains("нет документа"), "{err}");
        // Балл 0 вне шкалы.
        std::fs::write(golden.join("orphan.md"), "текст").expect("write");
        std::fs::write(golden.join("zero.expected.yaml"), "rubric: core\nscores:\n  context: 0\n")
            .expect("write");
        std::fs::write(golden.join("zero.md"), "текст").expect("write");
        let err = load_golden(&golden).expect_err("балл 0");
        assert!(err.to_string().contains("вне шкалы"), "{err}");
        // Битый YAML.
        std::fs::remove_file(golden.join("zero.expected.yaml")).expect("rm");
        std::fs::remove_file(golden.join("orphan.expected.yaml")).expect("rm");
        std::fs::write(golden.join("broken.expected.yaml"), "rubric: [unclosed").expect("write");
        std::fs::write(golden.join("broken.md"), "текст").expect("write");
        assert!(load_golden(&golden).is_err(), "битый yaml — ошибка");
    }

    #[tokio::test]
    async fn run_golden_computes_mae_against_expectations() {
        let (_dir, rubrics, golden) = golden_dirs();
        let llm = QueueLlm::new(&[
            // bad.md идёт первым по сортировке: судья совпал с эталоном.
            judge_reply(1, 1, "свидетельство отсутствует", "свидетельство отсутствует"),
            // good.md: судья занизил context на балл, risks угадал.
            judge_reply(
                4,
                5,
                "Цитата: \\\"Контекст описан полностью\\\" — почти полный",
                "Цитата: \\\"Риски названы и разобраны подробно\\\" — разобраны",
            ),
        ]);
        let cfg = JudgeConfig {
            samples: 1,
            ..JudgeConfig::default()
        };
        let report = run_golden(&llm, &rubrics, &golden, &cfg).await.expect("golden");
        assert_eq!(report.judge_model, "fake-queue");
        assert_eq!(report.compared, 4, "2 документа × 2 критерия");
        assert_eq!(report.cases.len(), 2);
        assert_eq!(report.cases[0].doc, "bad.md");
        assert!((report.cases[0].mae - 0.0).abs() < 1e-9, "точное совпадение");
        assert_eq!(report.cases[1].doc, "good.md");
        assert!((report.cases[1].mae - 0.5).abs() < 1e-9, "(|4−5| + |5−5|) / 2");
        assert!((report.mae - 0.25).abs() < 1e-9, "итоговый MAE по 4 парам");
    }

    #[tokio::test]
    async fn run_golden_rejects_unknown_criterion_in_expectation() {
        let (_dir, rubrics, golden) = golden_dirs();
        // Ломаем эталон good.md: критерий, которого нет в рубрике core.
        std::fs::write(
            golden.join("good.expected.yaml"),
            "rubric: core\nscores:\n  context: 5\n  ghost: 4\n",
        )
        .expect("write");
        let llm = QueueLlm::new(&[
            judge_reply(1, 1, "свидетельство отсутствует", "свидетельство отсутствует"),
            judge_reply(1, 1, "свидетельство отсутствует", "свидетельство отсутствует"),
        ]);
        let cfg = JudgeConfig {
            samples: 1,
            ..JudgeConfig::default()
        };
        let err = run_golden(&llm, &rubrics, &golden, &cfg)
            .await
            .expect_err("критерий-призрак — ошибка данных");
        assert!(err.to_string().contains("ghost"), "{err}");
    }

    /// Live-прогон golden-set репозитория: `cargo test -- --ignored golden_live`.
    #[tokio::test]
    #[ignore = "нужен API-ключ и сеть: живой прогон судьи по golden-set репозитория"]
    async fn golden_live_repo_set() {
        let cfg = crate::config::Config::load(None).expect("config");
        let registry = crate::llm::LlmRegistry::from_config(&cfg).expect("registry");
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let report = run_golden(
            registry.default().as_ref(),
            &root.join("assets/rubrics"),
            &root.join("assets/benchmarks/golden"),
            &cfg.judge,
        )
        .await
        .expect("golden run");
        assert!(report.compared >= 30, "пар документ×критерий: {}", report.compared);
        eprintln!("golden MAE = {:.2}", report.mae);
    }
}
