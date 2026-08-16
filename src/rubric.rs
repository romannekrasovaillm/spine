//! Движок рубрик архитектурного контроля: якорные и динамические.
//!
//! КОНТРАКТ (владелец: агент `rubric`):
//! - [`Rubric`] — YAML-рубрика: название, описание, шкала, критерии с весами
//!   и якорями уровней (anchor descriptors), опц. секция динамической генерации;
//! - якорные рубрики — готовые YAML из assets/rubrics; динамические —
//!   генерируются LLM под предмет оценки от якорной-основы ([`generate_dynamic`]);
//! - [`evaluate`] — LLM-судья оценивает целевой текст по критериям,
//!   структурированный разбор → [`RubricReport`] (баллы, веса, обоснования,
//!   markdown-отчёт).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{HarnessError, Result};
use crate::llm::{ChatMessage, ChatRequest, LlmProvider, ToolSpec};
use crate::tool::{Tool, ToolContext, ToolOutput};

/// Максимум символов оцениваемого текста в промпте судьи.
const MAX_TARGET_CHARS: usize = 24_000;

/// Сколько символов ответа модели включается в сообщение об ошибке разбора.
const ERR_FRAGMENT_CHARS: usize = 400;

/// Подсказка судье при повторном запросе: только JSON.
const RETRY_JSON_HINT: &str = "Ответ не разобран как JSON. Верни ТОЛЬКО JSON-объект \
     формата {\"scores\": [{\"criterion_id\": \"...\", \"score\": 1, \"rationale\": \"...\"}], \
     \"verdict\": \"...\"} — без markdown-обёрток и любого текста до и после.";

/// Подсказка генератору при повторном запросе: только YAML.
const RETRY_YAML_HINT: &str =
    "Ответ не разобран как YAML. Верни ТОЛЬКО YAML рубрики той же схемы — без markdown-обёрток и пояснений.";

/// Критерий рубрики с весом и якорями уровней.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Criterion {
    /// Идентификатор критерия (snake_case).
    pub id: String,
    /// Название.
    pub name: String,
    /// Что оценивается.
    pub description: String,
    /// Вес (сумма по рубрике — произвольная, нормируется при подсчёте).
    pub weight: f64,
    /// Якоря уровней: «1» → «критерий отсутствует», «5» → «образцово».
    #[serde(default)]
    pub anchors: std::collections::BTreeMap<u8, String>,
}

/// Рубрика оценки (якорная — из YAML, динамическая — сгенерированная).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rubric {
    /// Имя рубрики.
    pub name: String,
    /// Описание назначения.
    pub description: String,
    /// Максимум шкалы (обычно 5).
    pub scale_max: u8,
    /// Критерии с весами.
    pub criteria: Vec<Criterion>,
    /// Пометка происхождения: anchor|dynamic.
    pub origin: String,
}

/// Сводная строка списка рубрик.
#[derive(Debug, Clone)]
pub struct RubricSummary {
    /// Путь к YAML.
    pub path: PathBuf,
    /// Имя.
    pub name: String,
    /// Описание.
    pub description: String,
    /// Число критериев.
    pub criteria_count: usize,
}

/// Оценка одного критерия.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CriterionScore {
    /// Идентификатор критерия.
    pub criterion_id: String,
    /// Вес критерия в рубрике (копия для отчётной таблицы [`RubricReport::to_markdown`]).
    #[serde(default)]
    pub weight: f64,
    /// Балл 1..=scale_max.
    pub score: u8,
    /// Обоснование судьи.
    pub rationale: String,
}

/// Отчёт по рубрике.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RubricReport {
    /// Имя рубрики.
    pub rubric_name: String,
    /// Модель-судья.
    pub judge_model: String,
    /// Оценки по критериям.
    pub scores: Vec<CriterionScore>,
    /// Взвешенный итог (0..=scale_max).
    pub weighted_total: f64,
    /// Общий вердикт судьи.
    pub verdict: String,
}

impl RubricReport {
    /// Markdown-представление отчёта: заголовок, таблица баллов по критериям
    /// (критерий | вес | балл | обоснование), взвешенный итог, вердикт,
    /// имя судьи и дата формирования.
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "# Оценка по рубрике «{}»\n", self.rubric_name);
        let _ = writeln!(out, "| Критерий | Вес | Балл | Обоснование |");
        let _ = writeln!(out, "| --- | --- | --- | --- |");
        for s in &self.scores {
            let rationale = s.rationale.replace('|', "\\|").replace(['\n', '\r'], " ");
            let _ = writeln!(
                out,
                "| {} | {:.2} | {} | {} |",
                s.criterion_id, s.weight, s.score, rationale
            );
        }
        let _ = writeln!(out, "\n**Взвешенный итог:** {:.2}/5", self.weighted_total);
        let _ = writeln!(out, "**Вердикт:** {}", self.verdict);
        let _ = writeln!(out, "**Судья:** {}", self.judge_model);
        let _ = writeln!(out, "**Дата:** {}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
        out
    }
}

/// Загружает рубрику из YAML.
///
/// # Errors
/// Файл не читается / не валиден.
pub fn load(path: &Path) -> Result<Rubric> {
    let text = std::fs::read_to_string(path).map_err(|e| HarnessError::io(path, e))?;
    let rubric: Rubric = serde_yaml::from_str(&text)?;
    Ok(rubric)
}

/// Список рубрик каталога (`*.yaml`/`*.yml`); битые файлы пропускаются.
///
/// # Errors
/// Каталог не читается.
pub fn list(dir: &Path) -> Result<Vec<RubricSummary>> {
    let entries = std::fs::read_dir(dir).map_err(|e| HarnessError::io(dir, e))?;
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !is_yaml_file(&path) {
            continue;
        }
        // Битый файл — не ошибка каталога: пропускаем.
        if let Ok(rubric) = load(&path) {
            out.push(RubricSummary {
                path,
                criteria_count: rubric.criteria.len(),
                name: rubric.name,
                description: rubric.description,
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Вызов LLM с одним повтором при транспортной/декод-ошибке: судья и
/// генератор рубрик идемпотентны, а отказ сети не должен валить гейт
/// (наблюдение симуляции: «судья дал ошибку декодирования ответа, повтор
/// прошёл»). Повтор ручной был — теперь он встроен.
async fn complete_idempotent(llm: &dyn LlmProvider, req: ChatRequest) -> Result<ChatMessage> {
    match llm.complete(req.clone()).await {
        Ok(msg) => Ok(msg),
        Err(first_err) => {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            llm.complete(req).await.map_err(|_| first_err)
        }
    }
}

/// Оценивает целевой текст по рубрике через LLM-судью.
///
/// Судья — независимый архитектурный рецензент («ты не проектировал эту
/// систему — твоя работа найти, что сломается»); каждая оценка обязана
/// опираться на цитату-свидетельство из текста. Ответ судьи — строгий JSON
/// `{"scores": [...], "verdict": "..."}`; при неудаче разбора — один retry
/// с инструкцией «только JSON». Баллы клэмпятся в `1..=scale_max`, критерии,
/// пропущенные судьёй, получают балл 1.
///
/// # Errors
/// Ошибка модели или разбора её структурированного ответа.
pub async fn evaluate(rubric: &Rubric, target: &str, llm: &dyn LlmProvider) -> Result<RubricReport> {
    if rubric.criteria.is_empty() {
        return Err(HarnessError::Rubric(format!(
            "рубрика '{}' не содержит критериев",
            rubric.name
        )));
    }
    let mut messages = vec![
        ChatMessage::system(judge_system_prompt(rubric)),
        ChatMessage::user(judge_user_prompt(rubric, target)),
    ];
    let first = complete_idempotent(llm, ChatRequest::chat(messages.clone())).await?;
    let parsed = match parse_judge_response(&first.content) {
        Ok(parsed) => parsed,
        Err(_) => {
            // Один retry с явной инструкцией «только JSON».
            messages.push(ChatMessage::assistant(first.content.clone(), Vec::new()));
            messages.push(ChatMessage::user(RETRY_JSON_HINT));
            let second = complete_idempotent(llm, ChatRequest::chat(messages)).await?;
            parse_judge_response(&second.content).map_err(|_| {
                HarnessError::Rubric(format!(
                    "судья не вернул валидный JSON даже после повторного запроса: {}",
                    fragment(&second.content)
                ))
            })?
        }
    };
    build_report(rubric, llm.model(), parsed)
}

/// Генерирует динамическую рубрику под предмет оценки от якорной основы.
///
/// Модель выдаёт YAML той же схемы (5–8 критериев с весами и якорями 1/3/5);
/// при заданном `anchor` промпт требует сохранить шкалу и включить первые
/// три критерия якорной рубрики. Разбор терпим к markdown-обёрткам.
///
/// # Errors
/// Ошибка модели или разбора сгенерированной рубрики.
pub async fn generate_dynamic(
    subject: &str,
    anchor: Option<&Rubric>,
    llm: &dyn LlmProvider,
) -> Result<Rubric> {
    let mut user = format!(
        "Сгенерируй рубрику оценки для: {subject}\n\n\
         Требования:\n\
         - 5–8 критериев с весами (сумма произвольна, важность отражена в весе);\n\
         - у каждого критерия якоря уровней 1/3/5 — измеримые, проверяемые по тексту;\n\
         - id критериев — snake_case.\n\n\
         Формат — строго YAML:\n\
         name: <snake_case имя>\n\
         description: <что измеряет>\n\
         scale_max: 5\n\
         origin: dynamic\n\
         criteria:\n  \
         - id: <snake_case>\n    \
         name: <название>\n    \
         description: <что оценивается>\n    \
         weight: <число>\n    \
         anchors:\n      \
         1: <критерий отсутствует>\n      \
         3: <покрыт частично>\n      \
         5: <образцово>\n\n\
         Ответ — только YAML, без пояснений."
    );
    if let Some(anchor) = anchor {
        let ids: Vec<&str> = anchor.criteria.iter().take(3).map(|c| c.id.as_str()).collect();
        let _ = write!(
            user,
            "\n\nСохрани шкалу (scale_max = {}) и включи обязательные критерии якорной рубрики: {}.",
            anchor.scale_max,
            ids.join(", ")
        );
    }
    let system = "Ты — методолог архитектурного контроля: проектируешь измеримые рубрики \
                  оценки архитектурных решений. Отвечаешь строго YAML.";
    let mut messages = vec![ChatMessage::system(system), ChatMessage::user(user)];
    let first = complete_idempotent(llm, ChatRequest::chat(messages.clone())).await?;
    let mut rubric = match parse_rubric_yaml(&first.content) {
        Ok(rubric) => rubric,
        Err(_) => {
            // Один retry с явной инструкцией «только YAML».
            messages.push(ChatMessage::assistant(first.content.clone(), Vec::new()));
            messages.push(ChatMessage::user(RETRY_YAML_HINT));
            let second = complete_idempotent(llm, ChatRequest::chat(messages)).await?;
            parse_rubric_yaml(&second.content).map_err(|_| {
                HarnessError::Rubric(format!(
                    "генератор не вернул валидный YAML рубрики даже после повторного запроса: {}",
                    fragment(&second.content)
                ))
            })?
        }
    };
    if rubric.criteria.is_empty() {
        return Err(HarnessError::Rubric("сгенерированная рубрика без критериев".into()));
    }
    rubric.origin = "dynamic".into();
    Ok(rubric)
}

/// Инструменты домена: `rubric_list`, `rubric_evaluate`, `rubric_generate`.
pub fn tools() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(RubricListTool),
        Arc::new(RubricEvaluateTool),
        Arc::new(RubricGenerateTool),
    ]
}

/// Файл имеет YAML-расширение (`yaml`/`yml`, регистр неважен).
fn is_yaml_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("yaml") || e.eq_ignore_ascii_case("yml"))
}

/// Системный промпт судьи: независимый рецензент, строгий JSON на выходе.
fn judge_system_prompt(rubric: &Rubric) -> String {
    format!(
        "Ты — независимый архитектурный судья. Ты не проектировал эту систему — твоя работа \
         найти, что сломается. Оцени присланный текст по каждому критерию рубрики.\n\
         Жёсткие правила:\n\
         - каждая оценка ОБЯЗАНА опираться на цитату-свидетельство из текста — приведи её в rationale;\n\
         - если свидетельства в тексте нет, ставь 1 и явно пиши, что свидетельство отсутствует;\n\
         - шкала каждого критерия: целые числа 1..={};\n\
         - вердикт: 1–2 предложения о главном риске и готовности решения.\n\
         Ответ — СТРОГО один JSON-объект без markdown-обёрток и пояснений:\n\
         {{\"scores\": [{{\"criterion_id\": \"<id критерия>\", \"score\": <балл>, \
         \"rationale\": \"<обоснование с цитатой>\"}}], \"verdict\": \"<общий вердикт>\"}}",
        rubric.scale_max
    )
}

/// Пользовательский промпт судье: рубрика (критерии + якоря) и целевой текст.
fn judge_user_prompt(rubric: &Rubric, target: &str) -> String {
    let mut out = format!(
        "## Рубрика «{}»\n{}\nШкала: 1..={}\n\n## Критерии\n",
        rubric.name, rubric.description, rubric.scale_max
    );
    for c in &rubric.criteria {
        let _ = writeln!(out, "\n### {} — {} (вес {:.2})", c.id, c.name, c.weight);
        let _ = writeln!(out, "{}", c.description);
        if !c.anchors.is_empty() {
            let _ = writeln!(out, "Якоря:");
            for (level, text) in &c.anchors {
                let _ = writeln!(out, "- {level}: {text}");
            }
        }
    }
    let _ = writeln!(
        out,
        "\n## Оцениваемый текст\n{}",
        truncate_chars(target, MAX_TARGET_CHARS)
    );
    out
}

/// Сырой ответ судьи (JSON).
#[derive(Debug, Deserialize)]
struct JudgeResponse {
    /// Оценки по критериям (могут покрывать не все).
    #[serde(default)]
    scores: Vec<JudgeScore>,
    /// Общий вердикт.
    #[serde(default)]
    verdict: String,
}

/// Сырая оценка одного критерия от судьи.
#[derive(Debug, Deserialize)]
struct JudgeScore {
    /// Идентификатор критерия.
    criterion_id: String,
    /// Балл (терпимо: число или строка с числом).
    #[serde(default, deserialize_with = "de_lenient_f64")]
    score: f64,
    /// Обоснование.
    #[serde(default)]
    rationale: String,
}

/// Терпимый разбор балла: JSON-число или строка с числом.
fn de_lenient_f64<'de, D>(deserializer: D) -> std::result::Result<f64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Value::deserialize(deserializer)? {
        Value::Number(n) => n.as_f64().ok_or_else(|| serde::de::Error::custom("балл не число")),
        Value::String(s) => s
            .trim()
            .parse::<f64>()
            .map_err(|_| serde::de::Error::custom("балл не число")),
        _ => Err(serde::de::Error::custom("балл должен быть числом")),
    }
}

/// Извлекает JSON-объект из ответа: от первой `{` до последней `}`
/// (терпимо к ` ```json `-обёрткам и тексту до/после).
fn extract_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    (end > start).then(|| &text[start..=end])
}

/// Разбирает JSON-ответ судьи (с извлечением объекта из обёртки).
fn parse_judge_response(text: &str) -> Result<JudgeResponse> {
    let json = extract_json_object(text).ok_or_else(|| {
        HarnessError::Rubric(format!("в ответе судьи нет JSON-объекта: {}", fragment(text)))
    })?;
    serde_json::from_str(json)
        .map_err(|e| HarnessError::Rubric(format!("разбор JSON судьи: {e}: {}", fragment(json))))
}

/// Разбирает YAML рубрики из ответа модели (терпимо к ` ```yaml `-обёртке).
fn parse_rubric_yaml(text: &str) -> Result<Rubric> {
    let yaml = extract_yaml_payload(text);
    serde_yaml::from_str(yaml)
        .map_err(|e| HarnessError::Rubric(format!("разбор YAML рубрики: {e}: {}", fragment(yaml))))
}

/// Извлекает YAML-полезную нагрузку: содержимое fence-блока либо текст от `name:`.
fn extract_yaml_payload(text: &str) -> &str {
    if let Some(open) = text.find("```") {
        let after = &text[open + 3..];
        // Пропускаем языковой тег (```yaml) до конца строки.
        let start = after.find('\n').map_or(open + 3, |i| open + 3 + i + 1);
        if let Some(rel_end) = text[start..].find("```") {
            return text[start..start + rel_end].trim();
        }
    }
    if let Some(start) = text.find("name:") {
        return text[start..].trim();
    }
    text.trim()
}

/// Собирает отчёт: баллы в порядке критериев рубрики, клэмп в `1..=scale_max`,
/// пропущенные судьёй критерии — балл 1 с пометкой «судья не оценил».
fn build_report(rubric: &Rubric, judge_model: &str, parsed: JudgeResponse) -> Result<RubricReport> {
    let mut scores = Vec::with_capacity(rubric.criteria.len());
    for c in &rubric.criteria {
        let (score, rationale) = match parsed.scores.iter().find(|s| s.criterion_id == c.id) {
            Some(s) => (clamp_score(s.score, rubric.scale_max), s.rationale.clone()),
            None => (1, "судья не оценил".to_string()),
        };
        scores.push(CriterionScore {
            criterion_id: c.id.clone(),
            weight: c.weight,
            score,
            rationale,
        });
    }
    let weighted_total = weighted_total(&rubric.criteria, &scores)?;
    Ok(RubricReport {
        rubric_name: rubric.name.clone(),
        judge_model: judge_model.to_string(),
        scores,
        weighted_total,
        verdict: parsed.verdict,
    })
}

/// Взвешенный итог: Σ(score·weight)/Σweight.
///
/// # Errors
/// Сумма весов рубрики не положительна.
fn weighted_total(criteria: &[Criterion], scores: &[CriterionScore]) -> Result<f64> {
    let mut sum = 0.0;
    let mut weights = 0.0;
    for c in criteria {
        let score = scores.iter().find(|s| s.criterion_id == c.id).map_or(1, |s| s.score);
        sum += f64::from(score) * c.weight;
        weights += c.weight;
    }
    if weights <= 0.0 {
        return Err(HarnessError::Rubric(
            "сумма весов рубрики не положительна".into(),
        ));
    }
    Ok(sum / weights)
}

/// Клэмпит сырой балл судьи в `1..=scale_max`.
fn clamp_score(raw: f64, scale_max: u8) -> u8 {
    let max = f64::from(scale_max.max(1));
    // После clamp+round значение гарантированно в 1..=scale_max, усечения не будет.
    raw.clamp(1.0, max).round() as u8
}

/// Усекает текст до `max` символов с пометкой об усечении.
fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut s: String = text.chars().take(max).collect();
    s.push_str(&format!("\n… [усечено до {max} символов]"));
    s
}

/// Первые [`ERR_FRAGMENT_CHARS`] символов текста для сообщений об ошибках.
fn fragment(text: &str) -> String {
    let mut s: String = text.chars().take(ERR_FRAGMENT_CHARS).collect();
    if text.chars().count() > ERR_FRAGMENT_CHARS {
        s.push('…');
    }
    s
}

/// Резолвит рубрику: прямой путь (через [`ToolContext::resolve`]) → файл в
/// каталоге рубрик → имя без расширения в каталоге рубрик.
fn resolve_rubric_path(ctx: &ToolContext, name: &str) -> PathBuf {
    let direct = ctx.resolve(name);
    if direct.is_file() {
        return direct;
    }
    let dir = ctx.config.paths.rubrics_dir();
    let in_dir = dir.join(name);
    if in_dir.is_file() {
        return in_dir;
    }
    dir.join(format!("{name}.yaml"))
}

/// Инструмент `rubric_list`: список рубрик каталога `assets/rubrics`.
struct RubricListTool;

#[async_trait]
impl Tool for RubricListTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "rubric_list".into(),
            description: "Список рубрик архитектурного контроля (имя, описание, число критериев)".into(),
            parameters: json!({"type": "object", "properties": {}}),
        }
    }

    async fn call(&self, _args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let dir = ctx.config.paths.rubrics_dir();
        let items = list(&dir)?;
        if items.is_empty() {
            return Ok(ToolOutput::ok(format!("рубрики не найдены в {}", dir.display())));
        }
        let mut out = String::new();
        for r in &items {
            let _ = writeln!(
                out,
                "- {} — {} ({} критериев; {})",
                r.name,
                r.description,
                r.criteria_count,
                r.path.display()
            );
        }
        Ok(ToolOutput::ok(out))
    }
}

/// Инструмент `rubric_evaluate`: оценка текста по рубрике через LLM-судью.
struct RubricEvaluateTool;

#[async_trait]
impl Tool for RubricEvaluateTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "rubric_evaluate".into(),
            description: "Оценить текст (ADR, дизайн-документ) по рубрике через независимого \
                          LLM-судью; с dynamic_subject рубрика генерируется под предмет от якорной"
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "rubric": {"type": "string", "description": "Имя рубрики в assets/rubrics или путь к YAML"},
                    "target": {"type": "string", "description": "Путь к оцениваемому тексту"},
                    "dynamic_subject": {"type": "string", "description": "Опц.: предмет для динамической рубрики (rubric — якорь)"}
                },
                "required": ["rubric", "target"]
            }),
        }
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let Some(registry) = &ctx.llm else {
            return Ok(ToolOutput::err("нет LLM в контексте"));
        };
        let Some(rubric_arg) = args.get("rubric").and_then(Value::as_str) else {
            return Ok(ToolOutput::err("аргумент 'rubric' обязателен (string)"));
        };
        let Some(target_arg) = args.get("target").and_then(Value::as_str) else {
            return Ok(ToolOutput::err("аргумент 'target' обязателен (string)"));
        };
        let llm = registry.default();
        let target_path = ctx.resolve(target_arg);
        let text = std::fs::read_to_string(&target_path).map_err(|e| HarnessError::io(&target_path, e))?;
        let rubric_path = resolve_rubric_path(ctx, rubric_arg);
        let rubric = match args.get("dynamic_subject").and_then(Value::as_str) {
            Some(subject) => {
                let anchor = load(&rubric_path).ok();
                generate_dynamic(subject, anchor.as_ref(), llm.as_ref()).await?
            }
            None => load(&rubric_path)?,
        };
        let report = evaluate(&rubric, &text, llm.as_ref()).await?;
        Ok(ToolOutput::ok(report.to_markdown()))
    }
}

/// Инструмент `rubric_generate`: динамическая рубрика под предмет оценки.
struct RubricGenerateTool;

#[async_trait]
impl Tool for RubricGenerateTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "rubric_generate".into(),
            description: "Сгенерировать динамическую рубрику оценки под предмет \
                          (опц. от якорной основы); ответ — YAML рубрики"
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "subject": {"type": "string", "description": "Предмет оценки (напр. 'ADR миграции платёжного шлюза')"},
                    "anchor": {"type": "string", "description": "Опц.: имя/путь якорной рубрики-основы"}
                },
                "required": ["subject"]
            }),
        }
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let Some(registry) = &ctx.llm else {
            return Ok(ToolOutput::err("нет LLM в контексте"));
        };
        let Some(subject) = args.get("subject").and_then(Value::as_str) else {
            return Ok(ToolOutput::err("аргумент 'subject' обязателен (string)"));
        };
        let llm = registry.default();
        let anchor = match args.get("anchor").and_then(Value::as_str) {
            Some(name) => Some(load(&resolve_rubric_path(ctx, name))?),
            None => None,
        };
        let rubric = generate_dynamic(subject, anchor.as_ref(), llm.as_ref()).await?;
        let yaml = serde_yaml::to_string(&rubric)?;
        Ok(ToolOutput::ok(yaml))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, VecDeque};
    use std::sync::Mutex;

    /// Тестовый провайдер: возвращает заготовленные ответы по очереди.
    #[derive(Debug)]
    struct FakeLlm {
        replies: Mutex<VecDeque<String>>,
    }

    impl FakeLlm {
        fn new(replies: &[&str]) -> Self {
            Self {
                replies: Mutex::new(replies.iter().map(|s| (*s).to_string()).collect()),
            }
        }
    }

    #[async_trait]
    impl LlmProvider for FakeLlm {
        fn name(&self) -> &str {
            "fake"
        }
        fn model(&self) -> &str {
            "fake-judge-1"
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

    /// Рубрика-пример: два критерия с весами 1.0 и 3.0.
    fn sample_rubric() -> Rubric {
        let anchors = |tail: &str| {
            BTreeMap::from([
                (1u8, format!("отсутствует {tail}")),
                (3u8, format!("частично {tail}")),
                (5u8, format!("образцово {tail}")),
            ])
        };
        Rubric {
            name: "adr-quality".into(),
            description: "Качество ADR".into(),
            scale_max: 5,
            criteria: vec![
                Criterion {
                    id: "context".into(),
                    name: "Контекст".into(),
                    description: "Описан контекст и проблема".into(),
                    weight: 1.0,
                    anchors: anchors("контекст"),
                },
                Criterion {
                    id: "alternatives".into(),
                    name: "Альтернативы".into(),
                    description: "Рассмотрены альтернативы".into(),
                    weight: 3.0,
                    anchors: anchors("альтернативы"),
                },
            ],
            origin: "anchor".into(),
        }
    }

    #[test]
    fn rubric_yaml_roundtrip() {
        let rubric = sample_rubric();
        let yaml = serde_yaml::to_string(&rubric).expect("serialize");
        let back: Rubric = serde_yaml::from_str(&yaml).expect("deserialize");
        assert_eq!(back.name, rubric.name);
        assert_eq!(back.scale_max, 5);
        assert_eq!(back.criteria.len(), 2);
        assert_eq!(back.criteria[0].weight, 1.0);
        assert!(back.criteria[0].anchors.contains_key(&5));
        assert_eq!(back.origin, "anchor");
    }

    #[test]
    fn load_reads_yaml_and_list_skips_broken() {
        let dir = tempfile::tempdir().expect("tempdir");
        let good = dir.path().join("good.yaml");
        std::fs::write(&good, serde_yaml::to_string(&sample_rubric()).expect("yaml")).expect("write");
        std::fs::write(dir.path().join("broken.yaml"), "name: [unclosed").expect("write");
        std::fs::write(dir.path().join("notes.txt"), "не yaml").expect("write");

        let loaded = load(&good).expect("load");
        assert_eq!(loaded.name, "adr-quality");

        let items = list(dir.path()).expect("list");
        assert_eq!(items.len(), 1, "битый и не-yaml файлы должны быть пропущены");
        assert_eq!(items[0].name, "adr-quality");
        assert_eq!(items[0].criteria_count, 2);
    }

    #[test]
    fn list_errors_on_missing_dir() {
        let missing = Path::new("/nonexistent/rubrics-dir");
        assert!(list(missing).is_err());
    }

    #[test]
    fn weighted_total_math() {
        let rubric = sample_rubric();
        let scores = vec![
            CriterionScore {
                criterion_id: "context".into(),
                weight: 1.0,
                score: 4,
                rationale: String::new(),
            },
            CriterionScore {
                criterion_id: "alternatives".into(),
                weight: 3.0,
                score: 2,
                rationale: String::new(),
            },
        ];
        // (4*1 + 2*3) / (1+3) = 2.5
        let total = weighted_total(&rubric.criteria, &scores).expect("total");
        assert!((total - 2.5).abs() < 1e-9, "ожидали 2.5, получили {total}");
    }

    #[test]
    fn weighted_total_rejects_zero_weights() {
        let mut rubric = sample_rubric();
        for c in &mut rubric.criteria {
            c.weight = 0.0;
        }
        assert!(weighted_total(&rubric.criteria, &[]).is_err());
    }

    #[test]
    fn extracts_json_from_fence_and_preamble() {
        let fenced = "Вот оценка:\n```json\n{\"scores\": [], \"verdict\": \"ok\"}\n```\nГотово.";
        let json = extract_json_object(fenced).expect("json");
        assert!(json.starts_with('{') && json.ends_with('}'));
        assert!(json.contains("\"verdict\""));

        let bare = "преамбула {\"a\": 1} послесловие";
        assert_eq!(extract_json_object(bare), Some("{\"a\": 1}"));

        assert!(extract_json_object("нет json вообще").is_none());
    }

    #[test]
    fn clamp_score_bounds() {
        assert_eq!(clamp_score(0.0, 5), 1);
        assert_eq!(clamp_score(99.0, 5), 5);
        assert_eq!(clamp_score(3.0, 5), 3);
        assert_eq!(clamp_score(4.0, 0), 1, "scale_max=0 не должен паниковать");
    }

    #[tokio::test]
    async fn evaluate_clamps_scores_and_marks_missing() {
        let judge = "```json\n{\"scores\": [\n\
             {\"criterion_id\": \"context\", \"score\": 99, \"rationale\": \"цитата: 'контекст описан'\"},\n\
             {\"criterion_id\": \"unknown\", \"score\": 3, \"rationale\": \"лишний критерий\"}\n\
             ], \"verdict\": \"годно с оговорками\"}\n```";
        let llm = FakeLlm::new(&[judge]);
        let report = evaluate(&sample_rubric(), "Текст ADR: контекст описан.", &llm)
            .await
            .expect("evaluate");
        assert_eq!(report.scores.len(), 2, "в отчёте только критерии рубрики");
        assert_eq!(report.scores[0].criterion_id, "context");
        assert_eq!(report.scores[0].score, 5, "99 клэмпится в scale_max");
        assert_eq!(report.scores[1].score, 1, "пропущенный судьёй критерий → 1");
        assert_eq!(report.scores[1].rationale, "судья не оценил");
        assert_eq!(report.judge_model, "fake-judge-1");
        assert_eq!(report.verdict, "годно с оговорками");
        // (5*1 + 1*3) / 4 = 2.0
        assert!((report.weighted_total - 2.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn evaluate_retries_once_on_garbage() {
        let llm = FakeLlm::new(&[
            "безобразие, не json",
            "{\"scores\": [{\"criterion_id\": \"context\", \"score\": 4, \"rationale\": \"ok\"}], \"verdict\": \"ok\"}",
        ]);
        let report = evaluate(&sample_rubric(), "текст", &llm)
            .await
            .expect("evaluate после retry");
        assert_eq!(report.scores[0].score, 4);
    }

    #[tokio::test]
    async fn evaluate_fails_after_retry_with_fragment() {
        let llm = FakeLlm::new(&["мусор первый", "мусор второй"]);
        let err = evaluate(&sample_rubric(), "текст", &llm)
            .await
            .expect_err("должна быть ошибка разбора");
        let msg = err.to_string();
        assert!(msg.contains("мусор второй"), "фрагмент ответа в ошибке: {msg}");
    }

    #[tokio::test]
    async fn evaluate_rejects_rubric_without_criteria() {
        let mut rubric = sample_rubric();
        rubric.criteria.clear();
        let llm = FakeLlm::new(&[]);
        let err = evaluate(&rubric, "текст", &llm).await.expect_err("ошибка");
        assert!(err.to_string().contains("не содержит критериев"));
    }

    #[test]
    fn markdown_contains_table_total_verdict_judge() {
        let report = RubricReport {
            rubric_name: "adr-quality".into(),
            judge_model: "fake-judge-1".into(),
            scores: vec![CriterionScore {
                criterion_id: "context".into(),
                weight: 1.0,
                score: 4,
                rationale: "по тексту".into(),
            }],
            weighted_total: 4.0,
            verdict: "годно".into(),
        };
        let md = report.to_markdown();
        assert!(md.contains("# Оценка по рубрике «adr-quality»"));
        assert!(md.contains("| Критерий | Вес | Балл | Обоснование |"));
        assert!(md.contains("| context | 1.00 | 4 | по тексту |"));
        assert!(md.contains("**Взвешенный итог:** 4.00/5"));
        assert!(md.contains("**Вердикт:** годно"));
        assert!(md.contains("**Судья:** fake-judge-1"));
        assert!(md.contains("**Дата:**"));
    }

    #[tokio::test]
    async fn generate_dynamic_parses_fenced_yaml() {
        let yaml = "Вот рубрика:\n```yaml\n\
             name: dyn-x\n\
             description: динамическая\n\
             scale_max: 5\n\
             origin: anchor\n\
             criteria:\n  \
             - id: c1\n    \
             name: C1\n    \
             description: d1\n    \
             weight: 1.0\n    \
             anchors:\n      \
             1: нет\n      \
             3: частично\n      \
             5: да\n\
             ```";
        let llm = FakeLlm::new(&[yaml]);
        let rubric = generate_dynamic("оценка ADR", None, &llm)
            .await
            .expect("generate");
        assert_eq!(rubric.name, "dyn-x");
        assert_eq!(rubric.origin, "dynamic", "origin принудительно dynamic");
        assert_eq!(rubric.criteria.len(), 1);
        assert!(rubric.criteria[0].anchors.contains_key(&3));
    }

    #[tokio::test]
    async fn tools_are_registered_under_contract_names() {
        let names: Vec<String> = tools().iter().map(|t| t.spec().name.clone()).collect();
        assert_eq!(names, ["rubric_list", "rubric_evaluate", "rubric_generate"]);
    }

    #[tokio::test]
    async fn rubric_list_tool_reads_configured_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rubrics = dir.path().join("assets").join("rubrics");
        std::fs::create_dir_all(&rubrics).expect("mkdir");
        std::fs::write(
            rubrics.join("r.yaml"),
            serde_yaml::to_string(&sample_rubric()).expect("yaml"),
        )
        .expect("write");
        let mut cfg = crate::config::Config::default();
        cfg.paths.assets_dir = dir.path().join("assets");
        let ctx = ToolContext::new(dir.path().to_path_buf(), Arc::new(cfg));
        let out = RubricListTool
            .call(json!({}), &ctx)
            .await
            .expect("call");
        assert!(!out.is_error);
        assert!(out.content.contains("adr-quality"), "вывод: {}", out.content);
    }

    #[tokio::test]
    async fn rubric_evaluate_tool_without_llm_is_err() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = ToolContext::new(dir.path().to_path_buf(), Arc::new(crate::config::Config::default()));
        let out = RubricEvaluateTool
            .call(json!({"rubric": "x", "target": "y"}), &ctx)
            .await
            .expect("call");
        assert!(out.is_error);
        assert!(out.content.contains("нет LLM в контексте"));
    }
}
