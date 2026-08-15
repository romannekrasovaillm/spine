//! Дистилляция контекста в архитектурный скилл библиотеки плагинов.
//!
//! КОНТРАКТ (владелец: агент `tools`):
//! - [`distill_to_skill`] — общая процедура: материал → LLM (промпт
//!   `skill_distiller`) → валидный `SKILL.md` → файл
//!   `<plugins_root>/<plugin>/skills/<slug>/SKILL.md`;
//! - зона по умолчанию — плагин `arch-distilled` (managed: там перезапись
//!   разрешена); в чужих плагинах существующий файл не затирается;
//! - инструмент `skill_distill` — дистилляция переданного текста (статьи);
//!   транскрипт текущей сессии дистиллируется слэш-командой `/distill`
//!   (она видит историю, инструмент — нет).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::config::Config;
use crate::error::{HarnessError, Result};
use crate::llm::{ChatMessage, ChatRequest, LlmProvider, ToolSpec};
use crate::tool::{Tool, ToolContext, ToolOutput};

/// Плагин-зона для дистиллированных скиллов (managed: перезапись разрешена).
pub const DISTILL_PLUGIN_DEFAULT: &str = "arch-distilled";

/// Минимум материала для осмысленной дистилляции (символов).
const MIN_CONTENT_CHARS: usize = 200;
/// Максимум материала, уходящего в промпт (символов).
const MAX_CONTENT_CHARS: usize = 30_000;

/// Итог дистилляции.
#[derive(Debug)]
pub struct DistillOutcome {
    /// Путь к записанному SKILL.md.
    pub path: PathBuf,
    /// Финальное имя скилла (slug).
    pub skill_name: String,
    /// Размер записанного файла (символов).
    pub chars: usize,
}

/// Имя скилла → kebab-case slug: латиница/цифры/дефис, остальное выбрасывается.
/// Пустой результат — признак непригодного имени (ошибка у вызывающей).
#[must_use]
pub fn slugify(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut dash = false;
    for c in name.trim().to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    out.trim_end_matches('-').to_string()
}

/// Дистиллирует материал в SKILL.md и записывает в библиотеку плагинов.
///
/// Модель отвечает по промпту `skill_distiller`; если она не вернула
/// frontmatter — он синтезируется (имя = slug, описание — из первой строки).
///
/// # Errors
/// Имя без латиницы/цифр; материала меньше [`MIN_CONTENT_CHARS`]; целевой
/// файл существует вне managed-зоны; ошибка модели или записи файла.
pub async fn distill_to_skill(
    content: &str,
    name: &str,
    plugin: &str,
    provider: &Arc<dyn LlmProvider>,
    plugins_root: &Path,
) -> Result<DistillOutcome> {
    let slug = slugify(name);
    if slug.is_empty() {
        return Err(HarnessError::Tool(format!(
            "имя '{name}' не даёт slug: нужны латинские буквы или цифры"
        )));
    }
    let material: String = content.chars().take(MAX_CONTENT_CHARS).collect();
    if material.chars().count() < MIN_CONTENT_CHARS {
        return Err(HarnessError::Tool(format!(
            "слишком мало материала для дистилляции ({} из минимум {MIN_CONTENT_CHARS} символов)",
            material.chars().count()
        )));
    }
    let plugin = if plugin.trim().is_empty() {
        DISTILL_PLUGIN_DEFAULT
    } else {
        plugin.trim()
    };
    let target = plugins_root
        .join(plugin)
        .join("skills")
        .join(&slug)
        .join("SKILL.md");
    if target.exists() && plugin != DISTILL_PLUGIN_DEFAULT {
        return Err(HarnessError::Tool(format!(
            "{} уже существует вне зоны {DISTILL_PLUGIN_DEFAULT} — не затираю; \
             выберите другое имя или plugin={DISTILL_PLUGIN_DEFAULT}",
            target.display()
        )));
    }

    let request = ChatRequest {
        messages: vec![
            ChatMessage::system(crate::assets::PROMPT_SKILL_DISTILLER),
            ChatMessage::user(format!("Имя скилла: {slug}\n\nМатериал:\n{material}")),
        ],
        tools: Vec::new(),
        temperature: Some(0.3),
        max_tokens: Some(4000),
        thinking: None,
    };
    let raw = provider.complete(request).await?.content;
    let body = ensure_frontmatter(&raw, &slug);

    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| HarnessError::io(parent, e))?;
    }
    std::fs::write(&target, &body).map_err(|e| HarnessError::io(&target, e))?;
    Ok(DistillOutcome {
        path: target,
        skill_name: slug,
        chars: body.chars().count(),
    })
}

/// Гарантирует frontmatter у текста скилла: снимает код-фенсы модели,
/// при отсутствии `---` синтезирует шапку (имя = slug, описание — первая
/// непустая строка тела, усечённая).
fn ensure_frontmatter(raw: &str, slug: &str) -> String {
    let mut text = raw.trim();
    // Модель любит завернуть документ в ```markdown … ``` — снимаем.
    if let Some(rest) = text.strip_prefix("```") {
        let rest = rest.strip_prefix("markdown").unwrap_or(rest);
        let rest = rest.trim_start_matches(['\r', '\n']);
        text = rest.strip_suffix("```").unwrap_or(rest).trim();
    }
    if text.starts_with("---") {
        return format!("{text}\n");
    }
    let first_line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .unwrap_or("Дистиллированный архитектурный скилл.")
        .trim_start_matches(['*', '-', ' ']);
    let description: String = first_line.chars().take(280).collect();
    format!("---\nname: {slug}\ndescription: {description}\n---\n\n{text}\n")
}

/// Инструменты домена: `skill_distill`.
pub fn tools(cfg: &Config) -> Vec<Arc<dyn Tool>> {
    vec![Arc::new(SkillDistillTool {
        plugins_root: cfg.plugins.dirs.first().cloned(),
    })]
}

/// Инструмент `skill_distill`: дистилляция переданного текста в скилл.
struct SkillDistillTool {
    plugins_root: Option<PathBuf>,
}

#[async_trait]
impl Tool for SkillDistillTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "skill_distill".into(),
            description: "Дистиллировать материал (статью, конспект, заметки) в архитектурный \
                скилл библиотеки: модель выделяет повторяемую методику и пишет SKILL.md в \
                plugins/<plugin>/skills/<name>/. Сначала прочитай источник (read_file/web_fetch), \
                затем передавай текст в content. Транскрипт текущей сессии дистиллируется \
                слэш-командой /distill."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "имя скилла (будет приведено к kebab-case), например adr-staging"
                    },
                    "content": {
                        "type": "string",
                        "description": "текст материала для дистилляции (от 200 символов)"
                    },
                    "plugin": {
                        "type": "string",
                        "description": "целевой плагин (по умолчанию arch-distilled — managed-зона)"
                    }
                },
                "required": ["name", "content"]
            }),
        }
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let name = args
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        let content = args
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or("");
        let plugin = args.get("plugin").and_then(Value::as_str).unwrap_or("");
        let Some(root) = &self.plugins_root else {
            return Ok(ToolOutput::err(
                "не настроены каталоги плагинов ([plugins] dirs) — некуда писать скилл",
            ));
        };
        let Some(provider) = ctx
            .provider
            .clone()
            .or_else(|| ctx.llm.as_ref().map(|r| r.default()))
        else {
            return Ok(ToolOutput::err(
                "дистилляция требует модель: LLM не настроен в контексте",
            ));
        };
        match distill_to_skill(content, name, plugin, &provider, root).await {
            Ok(outcome) => Ok(ToolOutput::ok(format!(
                "скилл '{}' дистиллирован → {} ({} символов). \
                 Найдётся через skill_search(\"{}\") или /skills.",
                outcome.skill_name,
                outcome.path.display(),
                outcome.chars,
                outcome.skill_name
            ))),
            Err(e) => Ok(ToolOutput::err(format!("skill_distill: {e}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Провайдер, возвращающий готовый SKILL.md с frontmatter.
    #[derive(Debug)]
    struct GoodLlm;

    #[async_trait]
    impl LlmProvider for GoodLlm {
        fn name(&self) -> &str {
            "good"
        }
        fn model(&self) -> &str {
            "good-1"
        }
        async fn complete(&self, _req: ChatRequest) -> Result<ChatMessage> {
            Ok(ChatMessage::assistant(
                "---\nname: saga-staging\ndescription: Поэтапное внедрение саги. Используй при миграции транзакций.\n---\n\n# Saga staging\n\n## Методика\n1. Шаг.\n",
                Vec::new(),
            ))
        }
    }

    /// Провайдер, возвращающий тело без frontmatter и в код-фенсе.
    #[derive(Debug)]
    struct SloppyLlm;

    #[async_trait]
    impl LlmProvider for SloppyLlm {
        fn name(&self) -> &str {
            "sloppy"
        }
        fn model(&self) -> &str {
            "sloppy-1"
        }
        async fn complete(&self, _req: ChatRequest) -> Result<ChatMessage> {
            Ok(ChatMessage::assistant(
                "```markdown\n# Поэтапное внедрение саги\n\nМетодика внедрения саги по шагам с проверками.\n\n## Методика\n1. Шаг.\n```",
                Vec::new(),
            ))
        }
    }

    fn long_content() -> String {
        "Материал о внедрении саги в платёжном контуре. ".repeat(20)
    }

    #[test]
    fn slugify_kebab_cases_and_drops_garbage() {
        assert_eq!(slugify("Saga Staging"), "saga-staging");
        assert_eq!(slugify("  adr--authoring  "), "adr-authoring");
        assert_eq!(slugify("nfr_design!"), "nfr-design");
        assert_eq!(slugify("сага"), "", "чистая кириллица не даёт slug");
        assert_eq!(slugify("v2 API-gate"), "v2-api-gate");
    }

    #[tokio::test]
    async fn distill_writes_skill_with_model_frontmatter() {
        let tmp = tempfile::tempdir().expect("tmp");
        let provider: Arc<dyn LlmProvider> = Arc::new(GoodLlm);
        let outcome = distill_to_skill(&long_content(), "Saga Staging!", "", &provider, tmp.path())
            .await
            .expect("distill");
        assert_eq!(outcome.skill_name, "saga-staging");
        let text = std::fs::read_to_string(&outcome.path).expect("read");
        assert!(text.starts_with("---\nname: saga-staging"), "text: {text}");
        assert!(outcome.path.ends_with("arch-distilled/skills/saga-staging/SKILL.md"));
    }

    #[tokio::test]
    async fn distill_synthesizes_frontmatter_for_sloppy_model() {
        let tmp = tempfile::tempdir().expect("tmp");
        let provider: Arc<dyn LlmProvider> = Arc::new(SloppyLlm);
        let outcome = distill_to_skill(&long_content(), "saga-staging", "", &provider, tmp.path())
            .await
            .expect("distill");
        let text = std::fs::read_to_string(&outcome.path).expect("read");
        assert!(
            text.starts_with("---\nname: saga-staging\ndescription:"),
            "синтезированный frontmatter: {text}"
        );
        assert!(text.contains("# Поэтапное внедрение саги"));
        assert!(!text.contains("```"), "код-фенс снят: {text}");
    }

    #[tokio::test]
    async fn distill_refuses_tiny_content_and_bad_names() {
        let tmp = tempfile::tempdir().expect("tmp");
        let provider: Arc<dyn LlmProvider> = Arc::new(GoodLlm);
        let err = distill_to_skill("коротко", "ok-name", "", &provider, tmp.path())
            .await
            .expect_err("мало материала");
        assert!(err.to_string().contains("мало материала"), "{err}");
        let err = distill_to_skill(&long_content(), "сага", "", &provider, tmp.path())
            .await
            .expect_err("нет slug");
        assert!(err.to_string().contains("slug"), "{err}");
    }

    #[tokio::test]
    async fn distill_protects_foreign_plugin_but_overwrites_managed_zone() {
        let tmp = tempfile::tempdir().expect("tmp");
        let provider: Arc<dyn LlmProvider> = Arc::new(GoodLlm);
        // Чужой плагин: существующий файл не затираем.
        let foreign = tmp.path().join("arch-core/skills/saga-staging/SKILL.md");
        std::fs::create_dir_all(foreign.parent().expect("parent")).expect("mkdir");
        std::fs::write(&foreign, "пользовательский скилл").expect("write");
        let err = distill_to_skill(&long_content(), "saga-staging", "arch-core", &provider, tmp.path())
            .await
            .expect_err("защита чужой зоны");
        assert!(err.to_string().contains("не затираю"), "{err}");
        let kept = std::fs::read_to_string(&foreign).expect("read");
        assert_eq!(kept, "пользовательский скилл");
        // Managed-зона: перезапись разрешена.
        distill_to_skill(&long_content(), "saga-staging", "", &provider, tmp.path())
            .await
            .expect("первый прогон");
        let again = distill_to_skill(&long_content(), "saga-staging", "", &provider, tmp.path())
            .await
            .expect("перезапись в arch-distilled разрешена");
        assert!(again.path.is_file());
    }
}
