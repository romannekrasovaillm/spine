//! Специализированные архитектурные бенчмарки (solution architecture).
//!
//! КОНТРАКТ (владелец: агент `rubric` — общий с rubric.rs):
//! - [`Benchmark`] — YAML-сценарий: имя, описание, постановка задачи
//!   (system+user промпты), ссылка на рубрику оценки, проходной порог;
//! - [`run`] — прогон сценария на модели, оценка ответа рубрикой
//!   ([`crate::rubric::evaluate`]), запись отчёта в out_dir (md+json).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

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
/// Ответ модели оценивается тем же провайдером (судья = испытуемая модель).
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
    let rubric_report = crate::rubric::evaluate(&rubric, &response, provider).await?;
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
        let report = run(&bench, &llm, &rubrics, &out).await.expect("run");
        (dir, report, out)
    }

    #[tokio::test]
    async fn run_writes_reports_and_passes_above_threshold() {
        let judge = "{\"scores\": [\
             {\"criterion_id\": \"context\", \"score\": 5, \"rationale\": \"есть контекст\"}, \
             {\"criterion_id\": \"risks\", \"score\": 4, \"rationale\": \"есть риски\"}], \
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
        assert!(md_text.contains("| Критерий | Вес | Балл | Обоснование |"));
        let parsed: BenchReport =
            serde_json::from_str(&std::fs::read_to_string(json_path).expect("read json")).expect("parse json");
        assert!(parsed.passed);
        assert_eq!(parsed.response, report.response);
        assert_eq!(parsed.rubric_report.scores.len(), 2);
    }

    #[tokio::test]
    async fn run_fails_below_threshold() {
        let judge = "{\"scores\": [\
             {\"criterion_id\": \"context\", \"score\": 2, \"rationale\": \"слабо\"}, \
             {\"criterion_id\": \"risks\", \"score\": 2, \"rationale\": \"слабо\"}], \
             \"verdict\": \"плохо\"}";
        let (_dir, report, _out) = run_case(judge).await;
        assert!(!report.passed, "2.0 < 3.0");
        assert!(report.rubric_report.weighted_total < 3.0);
    }
}
