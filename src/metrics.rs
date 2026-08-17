//! Операционные метрики харнесса из append-only журналов сессий и отчётов.
//!
//! Идеи из обзоров (§E.24 SOURCE_BRIEF): first-pass validation rate, доля
//! ошибок инструментов, стоимость в токенах, прохождение рубрик/бенчей.
//! Всё считается локально из `sessions/*.jsonl` и `reports/`.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

use crate::error::Result;

/// Сводные метрики.
#[derive(Debug, Clone, Default, Serialize)]
pub struct HarnessMetrics {
    /// Сессий (журналов).
    pub sessions: usize,
    /// Сообщений пользователя.
    pub user_messages: usize,
    /// Ответов ассистента.
    pub assistant_messages: usize,
    /// Вызовов инструментов.
    pub tool_calls: usize,
    /// Ошибок инструментов (is_error).
    pub tool_errors: usize,
    /// Грубая оценка токенов (4 символа ≈ 1 токен).
    pub approx_tokens: u64,
    /// Вызовы по инструментам (имя → счётчик).
    pub tools_by_name: BTreeMap<String, usize>,
    /// Рубричных отчётов в reports/.
    pub rubric_reports: usize,
    /// Средний взвешенный балл рубрик.
    pub rubric_avg: Option<f64>,
    /// Бенч-отчётов (json), из них прошедших.
    pub bench_reports: usize,
    /// Прошедших бенчей.
    pub bench_passed: usize,
    /// Handoff-пакетов (MANIFEST.json в .arch-handoff известных? нет — cron-отчёты).
    pub cron_reports: usize,
    /// Интерактивных выборов (`propose_options`), зафиксированных в журналах.
    pub asks: usize,
    /// Из них — отказ пользователя (Esc, «реши сам»).
    pub asks_declined: usize,
    /// Из них — выбор рекомендованного варианта без изменений.
    pub asks_chose_recommended: usize,
    /// Репозиториев в реестре `repos.txt` с дрейфом AGENTS.md (lint-ошибки).
    /// Заполняется из CLI (`arch metrics`), не из `collect`.
    pub agentsmd_stale: usize,
    /// Репозиториев в реестре всего (0 — реестр не задан).
    pub agentsmd_total: usize,
}

impl HarnessMetrics {
    /// Доля ошибок инструментов (0..1).
    pub fn tool_error_rate(&self) -> f64 {
        if self.tool_calls == 0 {
            0.0
        } else {
            self.tool_errors as f64 / self.tool_calls as f64
        }
    }

    /// First-pass rate: сессии без ошибок инструментов / все сессии с инструментами.
    pub fn first_pass_note(&self) -> String {
        format!("{:.1}%", (1.0 - self.tool_error_rate()) * 100.0)
    }

    /// Доля «бездумных согласий» (Esc + выбор рекомендации без изменений),
    /// % от всех интерактивных выборов. None — выборов ещё не было.
    pub fn auto_approval_pct(&self) -> Option<f64> {
        if self.asks == 0 {
            None
        } else {
            Some(
                100.0 * (self.asks_declined + self.asks_chose_recommended) as f64
                    / self.asks as f64,
            )
        }
    }

    /// Флаг approval theater (обзоры `_24_августа`: >90–95% согласий без
    /// замечаний = театр одобрений) — при выборке от 5 вопросов.
    pub fn approval_theater(&self) -> bool {
        self.asks >= 5 && self.auto_approval_pct().unwrap_or(0.0) >= 90.0
    }

    /// Стоимость одного проверенного результата (₽): оценка токенов × тариф
    /// / (рубричные отчёты + пройденные бенчи). Грубая прокси-метрика из
    /// обзоров (cost per validated outcome); None — результатов ещё нет.
    pub fn cost_per_outcome(&self) -> Option<f64> {
        let outcomes = self.rubric_reports + self.bench_passed;
        if outcomes == 0 {
            None
        } else {
            Some(self.approx_tokens as f64 * 0.0001 / outcomes as f64)
        }
    }

    /// Markdown-отчёт.
    pub fn to_markdown(&self) -> String {
        let mut out = String::from("# Метрики харнесса arch\n\n");
        out.push_str(&format!(
            "- Сессий: **{}** (user: {}, assistant: {})\n",
            self.sessions, self.user_messages, self.assistant_messages
        ));
        out.push_str(&format!(
            "- Вызовов инструментов: **{}**, ошибок: {} ({:.1}%)\n",
            self.tool_calls,
            self.tool_errors,
            self.tool_error_rate() * 100.0
        ));
        out.push_str(&format!(
            "- Оценка токенов: ~{} (≈ {:.2}₽ при 0.1₽/1K)\n",
            self.approx_tokens,
            self.approx_tokens as f64 * 0.0001
        ));
        out.push_str(&format!(
            "- Рубричных отчётов: {}, средний балл: {}\n",
            self.rubric_reports,
            self.rubric_avg
                .map(|a| format!("{a:.2}"))
                .unwrap_or_else(|| "—".into())
        ));
        out.push_str(&format!(
            "- Бенчей: {}, прошло: {} ({})\n",
            self.bench_reports,
            self.bench_passed,
            if self.bench_reports > 0 {
                format!(
                    "{:.0}%",
                    100.0 * self.bench_passed as f64 / self.bench_reports as f64
                )
            } else {
                "—".into()
            }
        ));
        out.push_str(&format!("- Cron-отчётов: {}\n", self.cron_reports));
        out.push_str("\n## Трансформационные KPI (обзоры _24_августа)\n\n");
        match self.auto_approval_pct() {
            Some(pct) => {
                out.push_str(&format!(
                    "- Согласия без изменений (Esc + рекомендованное): **{pct:.0}%** из {} выборов{}\n",
                    self.asks,
                    if self.approval_theater() {
                        " — ⚠ approval theater: решения фактически не проверяются человеком"
                    } else {
                        ""
                    }
                ));
            }
            None => out.push_str("- Интерактивных выборов ещё не было (propose_options).\n"),
        }
        if self.agentsmd_total > 0 {
            out.push_str(&format!(
                "- Architecture drift: **{}/{}** репозиториев реестра с дрейфом AGENTS.md\n",
                self.agentsmd_stale, self.agentsmd_total
            ));
        }
        if let Some(cpo) = self.cost_per_outcome() {
            out.push_str(&format!(
                "- Cost per validated outcome: ≈ **{cpo:.2}₽** (токены × 0.1₽/1K / (рубрики + пройденные бенчи))\n"
            ));
        }
        if !self.tools_by_name.is_empty() {
            out.push_str("\n## Инструменты по вызовам\n\n");
            for (name, count) in &self.tools_by_name {
                out.push_str(&format!("- {name}: {count}\n"));
            }
        }
        out
    }
}

/// Считает метрики по каталогам сессий и отчётов.
///
/// # Errors
/// Каталоги недоступны на чтение.
pub fn collect(sessions_dir: &Path, reports_dir: &Path) -> Result<HarnessMetrics> {
    let mut m = HarnessMetrics::default();
    if let Ok(rd) = std::fs::read_dir(sessions_dir) {
        for entry in rd.flatten() {
            let path = entry.path();
            let is_log = path
                .file_name()
                .map(|n| {
                    let n = n.to_string_lossy();
                    n.starts_with("session-") && n.ends_with(".jsonl")
                })
                .unwrap_or(false);
            if !is_log {
                continue;
            }
            m.sessions += 1;
            if let Ok(text) = std::fs::read_to_string(&path) {
                parse_journal(&text, &mut m);
            }
        }
    }
    collect_reports(reports_dir, &mut m);
    Ok(m)
}

/// Разбор одного журнала: счётчики сообщений/инструментов/токенов.
fn parse_journal(text: &str, m: &mut HarnessMetrics) {
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let kind = v.get("kind").and_then(|k| k.as_str()).unwrap_or("");
        let content_len = v
            .get("content")
            .and_then(|c| c.as_str())
            .map_or(0, str::len) as u64;
        m.approx_tokens += content_len / 4;
        match kind {
            "user" => m.user_messages += 1,
            "assistant" => {
                m.assistant_messages += 1;
                if let Some(calls) = v.get("tool_calls").and_then(|t| t.as_array()) {
                    for c in calls {
                        if let Some(name) = c.get("name").and_then(|n| n.as_str()) {
                            m.tool_calls += 1;
                            *m.tools_by_name.entry(name.into()).or_default() += 1;
                        }
                    }
                }
            }
            "tool" => {
                let is_err = v.get("is_error").and_then(|e| e.as_bool()).unwrap_or(false);
                if is_err {
                    m.tool_errors += 1;
                }
            }
            "event" => {
                if v.get("event").and_then(|e| e.as_str()) == Some("ask") {
                    m.asks += 1;
                    if v.get("declined").and_then(|d| d.as_bool()).unwrap_or(false) {
                        m.asks_declined += 1;
                    }
                    if v.get("chose_recommended")
                        .and_then(|c| c.as_bool())
                        .unwrap_or(false)
                    {
                        m.asks_chose_recommended += 1;
                    }
                }
            }
            _ => {}
        }
    }
}

/// Сбор из отчётов: рубрики (md с «взвешенный итог»), бенчи (json), крон.
fn collect_reports(dir: &Path, m: &mut HarnessMetrics) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut rubric_sum = 0.0;
    for entry in rd.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with("rubric-") && name.ends_with(".md") {
            if let Ok(text) = std::fs::read_to_string(&path) {
                m.rubric_reports += 1;
                // Ищем «X.XX/5» в строке итога.
                for line in text.lines() {
                    let low = line.to_lowercase();
                    if low.contains("взвешенн") || low.contains("weighted") {
                        // Формат отчёта rubric.rs: «**Взвешенный итог:** 4.20/5».
                        if let Some(score) = line.split_whitespace().find_map(|t| {
                            let t = t.trim_matches(['*', ':', '—']);
                            let (num, denom) = t.split_once('/')?;
                            if denom.trim().parse::<f64>().is_ok() {
                                num.parse::<f64>().ok()
                            } else {
                                None
                            }
                        }) {
                            rubric_sum += score;
                        }
                        break;
                    }
                }
            }
        } else if name.starts_with("bench-") && name.ends_with(".json") {
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                    m.bench_reports += 1;
                    if v.get("passed").and_then(|p| p.as_bool()).unwrap_or(false) {
                        m.bench_passed += 1;
                    }
                }
            }
        }
    }
    // Крон-отчёты в подкаталоге cron/.
    if let Ok(rd) = std::fs::read_dir(dir.join("cron")) {
        m.cron_reports = rd
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
            .count();
    }
    if m.rubric_reports > 0 {
        m.rubric_avg = Some(rubric_sum / m.rubric_reports as f64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ask_events_feed_approval_theater_metric() {
        let mut m = HarnessMetrics::default();
        // 4 согласия (2 Esc + 2 рекомендованное) + 1 самостоятельный выбор.
        let journal = concat!(
            "{\"kind\":\"event\",\"event\":\"ask\",\"declined\":true,\"chose_recommended\":false}\n",
            "{\"kind\":\"event\",\"event\":\"ask\",\"declined\":true,\"chose_recommended\":false}\n",
            "{\"kind\":\"event\",\"event\":\"ask\",\"declined\":false,\"chose_recommended\":true}\n",
            "{\"kind\":\"event\",\"event\":\"ask\",\"declined\":false,\"chose_recommended\":true}\n",
            "{\"kind\":\"event\",\"event\":\"ask\",\"declined\":false,\"chose_recommended\":false}\n",
            "{\"kind\":\"event\",\"event\":\"compact\",\"folded\":3}\n"
        );
        parse_journal(journal, &mut m);
        assert_eq!(m.asks, 5);
        assert_eq!(m.asks_declined, 2);
        assert_eq!(m.asks_chose_recommended, 2);
        let pct = m.auto_approval_pct().expect("выборы есть");
        assert!((pct - 80.0).abs() < 0.1, "pct: {pct}");
        assert!(!m.approval_theater(), "80% — ещё не театр");
        m.asks_chose_recommended += 1; // 5/6 = 83%… нужно ≥90%
        m.asks = 6;
        assert!(!m.approval_theater());
        m.asks_declined += 4; // 9/10 = 90% — театр
        m.asks = 10;
        assert!(m.approval_theater(), "90% бездумных согласий — театр");
        // Без выборов — None, флага нет.
        let empty = HarnessMetrics::default();
        assert!(empty.auto_approval_pct().is_none());
        assert!(!empty.approval_theater());
    }

    #[test]
    fn cost_per_validated_outcome_is_none_without_outcomes() {
        let mut m = HarnessMetrics::default();
        assert!(m.cost_per_outcome().is_none());
        m.approx_tokens = 40_000; // ≈ 4₽ при 0.1₽/1K
        m.rubric_reports = 2;
        m.bench_passed = 2;
        let cpo = m.cost_per_outcome().expect("outcomes есть");
        assert!((cpo - 1.0).abs() < 0.01, "cpo: {cpo}");
    }

    #[test]
    fn collects_metrics_from_journals_and_reports() {
        let tmp = tempfile::tempdir().expect("tmp");
        let sessions = tmp.path().join("sessions");
        let reports = tmp.path().join("reports");
        std::fs::create_dir_all(&sessions).expect("sessions");
        std::fs::create_dir_all(reports.join("cron")).expect("cron");
        std::fs::write(
            sessions.join("session-20260814-100000.jsonl"),
            concat!(
                "{\"ts\":\"t\",\"kind\":\"system\",\"content\":\"sys\"}\n",
                "{\"ts\":\"t\",\"kind\":\"user\",\"content\":\"привет архитектор\"}\n",
                "{\"ts\":\"t\",\"kind\":\"assistant\",\"content\":\"\",\"tool_calls\":[{\"name\":\"bash\",\"arguments\":{}}]}\n",
                "{\"ts\":\"t\",\"kind\":\"tool\",\"is_error\":false}\n",
                "{\"ts\":\"t\",\"kind\":\"assistant\",\"content\":\"готово\"}\n"
            ),
        )
        .expect("journal");
        std::fs::write(
            reports.join("rubric-solution_architecture-20260814.md"),
            "# Отчёт\n\nВзвешенный итог: **4.20/5**\n",
        )
        .expect("rubric");
        std::fs::write(
            reports.join("bench-payment_integration-deepseek-x.json"),
            "{\"bench_name\":\"p\",\"model\":\"m\",\"response\":\"\",\"rubric_report\":null,\"passed\":true}",
        )
        .expect("bench");
        std::fs::write(reports.join("cron/kb-digest-x.md"), "# Дайджест\n").expect("cron rep");

        let m = collect(&sessions, &reports).expect("collect");
        assert_eq!(m.sessions, 1);
        assert_eq!(m.user_messages, 1);
        assert_eq!(m.assistant_messages, 2);
        assert_eq!(m.tool_calls, 1);
        assert_eq!(m.tools_by_name.get("bash"), Some(&1));
        assert_eq!(m.tool_errors, 0);
        assert_eq!(m.rubric_reports, 1);
        assert_eq!(m.rubric_avg, Some(4.2));
        assert_eq!(m.bench_reports, 1);
        assert_eq!(m.bench_passed, 1);
        assert_eq!(m.cron_reports, 1);
        let md = m.to_markdown();
        assert!(md.contains("4.20"), "{md}");
    }

    #[test]
    fn empty_dirs_yield_zeroes() {
        let tmp = tempfile::tempdir().expect("tmp");
        let m = collect(&tmp.path().join("nope"), &tmp.path().join("nada")).expect("collect");
        assert_eq!(m.sessions, 0);
        assert_eq!(m.tool_error_rate(), 0.0);
    }
}
