//! Контракт инструментов харнесса: трейт [`Tool`], реестр [`ToolRegistry`].
//!
//! Харнесс намеренно тонкий: bash + файловые инструменты + специализированные
//! инструменты архитектора (mermaid, рубрики, контроль, знания, веб, handoff).
//! Реализации — в модуле [`crate::tools`] и в доменных модулях (каждый домен
//! экспортирует `tools() -> Vec<Arc<dyn Tool>>`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use crate::config::Config;
use crate::error::Result;
use crate::llm::{LlmRegistry, ToolSpec};

/// Вариант ответа на вопрос агента (инструмент `propose_options`).
#[derive(Debug, Clone)]
pub struct AskOption {
    /// Короткое название варианта (1–5 слов).
    pub label: String,
    /// Последствия/компромиссы варианта.
    pub description: String,
}

/// Запрос интерактивного выбора от инструмента к UI (TUI).
///
/// UI отвечает выбранным `label` через oneshot-канал; пустая строка —
/// отказ от выбора (Esc). Пока запрос висит без ответа, инструмент ждёт,
/// агентный ход стоит — это сознательно: решение за человеком.
#[derive(Debug)]
pub struct AskRequest {
    /// Вопрос пользователю (что выбираем и почему это важно).
    pub question: String,
    /// Варианты (2–4).
    pub options: Vec<AskOption>,
    /// `label` рекомендуемого варианта (если агент рекомендует).
    pub recommended: Option<String>,
    /// Канал ответа: выбранный `label` или пустая строка при отказе.
    pub reply: oneshot::Sender<String>,
}

/// Контекст вызова инструмента.
#[derive(Clone)]
pub struct ToolContext {
    /// Рабочий каталог (все относительные пути инструментов — от него).
    pub cwd: PathBuf,
    /// Конфигурация харнесса.
    pub config: Arc<Config>,
    /// Реестр LLM (некоторым инструментам нужна модель: рубрики, динамические проверки).
    pub llm: Option<Arc<LlmRegistry>>,
    /// Мост интерактивных вопросов к UI (TUI). None — headless-режим:
    /// `propose_options` деградирует до текстовой инструкции модели.
    pub ask: Option<mpsc::Sender<AskRequest>>,
    /// Активная модель чата (для субагентов и дистилляции). Поддерживается
    /// агентной сессией: `/model` обновляет это поле вместе с провайдером.
    pub provider: Option<Arc<dyn crate::llm::LlmProvider>>,
    /// Реестр фоновых субагентов (общий между сессией и слэш-командами).
    pub subagents: Option<crate::subagent::SubagentRegistry>,
}

impl ToolContext {
    /// Контекст без LLM (для CLI-подкоманд, не требующих модели).
    pub fn new(cwd: PathBuf, config: Arc<Config>) -> Self {
        Self {
            cwd,
            config,
            llm: None,
            ask: None,
            provider: None,
            subagents: None,
        }
    }

    /// Добавляет реестр LLM.
    pub fn with_llm(mut self, llm: Arc<LlmRegistry>) -> Self {
        self.llm = Some(llm);
        self
    }

    /// Подключает мост интерактивных вопросов к UI (TUI).
    pub fn with_ask(mut self, ask: mpsc::Sender<AskRequest>) -> Self {
        self.ask = Some(ask);
        self
    }

    /// Задаёт активную модель (для субагентов и дистилляции).
    pub fn with_provider(mut self, provider: Arc<dyn crate::llm::LlmProvider>) -> Self {
        self.provider = Some(provider);
        self
    }

    /// Подключает реестр фоновых субагентов.
    pub fn with_subagents(mut self, registry: crate::subagent::SubagentRegistry) -> Self {
        self.subagents = Some(registry);
        self
    }

    /// Резолвит путь относительно рабочего каталога.
    pub fn resolve(&self, path: impl AsRef<std::path::Path>) -> PathBuf {
        let p = path.as_ref();
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.cwd.join(p)
        }
    }
}

/// Результат выполнения инструмента.
#[derive(Debug, Clone)]
pub struct ToolOutput {
    /// Текстовое содержимое для модели/пользователя.
    pub content: String,
    /// Признак ошибки (модель должна увидеть и скорректировать план).
    pub is_error: bool,
}

impl ToolOutput {
    /// Успешный результат.
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
        }
    }

    /// Ошибочный результат (не паника, а сигнал модели).
    pub fn err(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
        }
    }

    /// Обрезает содержимое до `max_chars` с пометкой об усечении.
    pub fn truncated(mut self, max_chars: usize) -> Self {
        if self.content.len() > max_chars {
            let mut cut = self.content.chars().take(max_chars).collect::<String>();
            cut.push_str(&format!(
                "\n… [усечено: {} из {} байт]",
                max_chars,
                self.content.len()
            ));
            self.content = cut;
        }
        self
    }
}

/// Инструмент харнесса.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Спецификация для function calling.
    fn spec(&self) -> ToolSpec;
    /// Выполнить вызов.
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput>;
}

/// Реестр инструментов.
#[derive(Clone)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
    /// Политика автономии (R-уровни); по умолчанию R2.
    policy: crate::policy::Policy,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self {
            tools: HashMap::new(),
            policy: crate::policy::Policy::default(),
        }
    }
}

impl ToolRegistry {
    /// Пустой реестр.
    pub fn new() -> Self {
        Self::default()
    }

    /// Установить политику автономии.
    pub fn with_policy(mut self, policy: crate::policy::Policy) -> Self {
        self.policy = policy;
        self
    }

    /// Зарегистрировать инструмент (по имени из спецификации).
    pub fn register(&mut self, tool: Arc<dyn Tool>) -> &mut Self {
        self.tools.insert(tool.spec().name.clone(), tool);
        self
    }

    /// Builder-вариант регистрации.
    pub fn with(mut self, tool: Arc<dyn Tool>) -> Self {
        self.register(tool);
        self
    }

    /// Инструмент по имени.
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    /// Подмножество реестра по whitelist имён (ограничение инструментов
    /// субагента). Неизвестные имена пропускаются; политика наследуется.
    #[must_use]
    pub fn subset(&self, names: &[String]) -> Self {
        let keep: std::collections::HashSet<&str> = names.iter().map(String::as_str).collect();
        let mut out = Self::new().with_policy(self.policy);
        for (name, tool) in &self.tools {
            if keep.contains(name.as_str()) {
                out.tools.insert(name.clone(), Arc::clone(tool));
            }
        }
        out
    }

    /// Реестр без перечисленных инструментов (антирекурсия: субагенту не
    /// выдаются `subagent_*`). Политика наследуется.
    #[must_use]
    pub fn excluding(&self, names: &[&str]) -> Self {
        let mut out = Self::new().with_policy(self.policy);
        for (name, tool) in &self.tools {
            if !names.contains(&name.as_str()) {
                out.tools.insert(name.clone(), Arc::clone(tool));
            }
        }
        out
    }

    /// Спецификации всех инструментов (для ChatRequest).
    pub fn specs(&self) -> Vec<ToolSpec> {
        let mut specs: Vec<ToolSpec> = self.tools.values().map(|t| t.spec()).collect();
        specs.sort_by(|a, b| a.name.cmp(&b.name));
        specs
    }

    /// Имена всех инструментов.
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.tools.keys().cloned().collect();
        names.sort();
        names
    }

    /// Выполнить вызов по имени. Ошибка превращается в [`ToolOutput::err`],
    /// чтобы агентный цикл не рвался на сбое одного инструмента.
    /// Перед вызовом — проверка политики автономии (R-уровни).
    pub async fn dispatch(&self, name: &str, args: Value, ctx: &ToolContext) -> ToolOutput {
        match self.policy.check(name, &args) {
            crate::policy::PolicyDecision::Allow => {}
            crate::policy::PolicyDecision::RequireConfirm(reason) => {
                return ToolOutput::err(format!(
                    "ТРЕБУЕТСЯ ПОДТВЕРЖДЕНИЕ ЧЕЛОВЕКА: {reason}. В неинтерактивном режиме действие отклонено — эскалируйте архитектору или повторите с повышенным уровнем автономии."
                ));
            }
            crate::policy::PolicyDecision::Deny(reason) => {
                return ToolOutput::err(format!("ЗАПРЕЩЕНО политикой автономии: {reason}"));
            }
        }
        match self.get(name) {
            Some(tool) => match tool.call(args, ctx).await {
                Ok(out) => out,
                Err(e) => ToolOutput::err(format!("{name}: {e}")),
            },
            None => ToolOutput::err(format!("неизвестный инструмент: {name}")),
        }
    }
}
