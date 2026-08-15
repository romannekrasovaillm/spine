//! Провайдеры LLM: единый трейт [`LlmProvider`], реестр [`LlmRegistry`].
//!
//! Все провайдеры (DeepSeek, Kimi, GLM) — OpenAI-совместимые endpoint'ы;
//! общая реализация живёт в [`openai_compat`], файлы провайдеров — тонкие
//! фабрики с пресетами base_url.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::config::{Config, ModelConfig};
use crate::error::{HarnessError, Result};

pub mod deepseek;
pub mod glm;
pub mod kimi;
pub mod openai_compat;

/// Роль сообщения в чате.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Системный промпт.
    System,
    /// Сообщение пользователя.
    User,
    /// Ответ ассистента.
    Assistant,
    /// Результат инструмента.
    Tool,
}

impl Role {
    /// Строковое представление для API.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Вызов инструмента, запрошенный моделью.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// Идентификатор вызова (связывает ответ инструмента).
    pub id: String,
    /// Имя инструмента.
    pub name: String,
    /// Аргументы (JSON по схеме из [`ToolSpec`]).
    pub arguments: Value,
}

/// Сообщение чата.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    /// Роль автора.
    pub role: Role,
    /// Текстовое содержимое (может быть пустым при tool_calls).
    pub content: String,
    /// Запрошенные вызовы инструментов (для роли Assistant).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// Идентификатор вызова, на который отвечает это сообщение (роль Tool).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Цепочка рассуждений (роль Assistant, ризонинг-модели: DeepSeek V4
    /// thinking, Kimi k2.6/K3, GLM-4.x). DeepSeek ТРЕБУЕТ возвращать её в
    /// последующих запросах, если были tool_calls (иначе HTTP 400) — поэтому
    /// поле хранится и эхом уходит в API, но в чате не отображается.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
}

impl ChatMessage {
    /// Системное сообщение.
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            reasoning_content: None,
        }
    }

    /// Сообщение пользователя.
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            reasoning_content: None,
        }
    }

    /// Ответ ассистента (возможно, с вызовами инструментов).
    pub fn assistant(content: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_calls,
            tool_call_id: None,
            reasoning_content: None,
        }
    }

    /// Результат выполнения инструмента.
    pub fn tool_result(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id.into()),
            reasoning_content: None,
        }
    }

    /// Грубая оценка размера в токенах (4 символа ≈ 1 токен).
    pub fn rough_tokens(&self) -> usize {
        (self.content.len()
            + self.reasoning_content.as_deref().unwrap_or("").len()
            + self.tool_calls.iter().map(|c| c.arguments.to_string().len()).sum::<usize>()) / 4
    }
}

/// Спецификация инструмента для function calling (JSON Schema).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    /// Имя инструмента (snake_case).
    pub name: String,
    /// Описание для модели: что делает и когда вызывать.
    pub description: String,
    /// JSON Schema объекта параметров.
    pub parameters: Value,
}

/// Запрос к модели.
#[derive(Debug, Clone, Default)]
pub struct ChatRequest {
    /// История сообщений.
    pub messages: Vec<ChatMessage>,
    /// Доступные инструменты.
    pub tools: Vec<ToolSpec>,
    /// Температура (None — дефолт провайдера/конфига).
    pub temperature: Option<f32>,
    /// Максимум токенов ответа.
    pub max_tokens: Option<u32>,
    /// Переключатель ризонинга для этого запроса: `Some(true/false)` —
    /// слить в тело `thinking_on`/`thinking_off` из конфига модели;
    /// `None` — ничего не слать (дефолт провайдера).
    pub thinking: Option<bool>,
}

impl ChatRequest {
    /// Запрос без инструментов из списка сообщений.
    pub fn chat(messages: Vec<ChatMessage>) -> Self {
        Self {
            messages,
            tools: Vec::new(),
            temperature: None,
            max_tokens: None,
            thinking: None,
        }
    }

    /// Установить инструменты.
    pub fn with_tools(mut self, tools: Vec<ToolSpec>) -> Self {
        self.tools = tools;
        self
    }
}

/// Статистика токенов.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    /// Токены промпта.
    pub prompt_tokens: u64,
    /// Токены ответа.
    pub completion_tokens: u64,
}

impl Usage {
    /// Суммарные токены.
    pub fn total(&self) -> u64 {
        self.prompt_tokens + self.completion_tokens
    }
}

/// Событие стриминга ответа модели.
#[derive(Debug, Clone)]
pub enum LlmEvent {
    /// Порция текста.
    Delta(String),
    /// Операционная заметка провайдера (например, «поток оборвался, повторяю
    /// запрос»): для UI, НЕ часть собираемого ответа и журнала сессии.
    Note(String),
    /// Финал: статистика токенов.
    Done(Usage),
}

/// Провайдер LLM (OpenAI-совместимый чат-комплишн).
#[async_trait]
pub trait LlmProvider: Send + Sync + fmt::Debug {
    /// Короткое имя провайдера (`deepseek`, `kimi`, `glm`, …).
    fn name(&self) -> &str;
    /// Идентификатор модели.
    fn model(&self) -> &str;
    /// Нестриминговый запрос: полный ответ разом.
    async fn complete(&self, req: ChatRequest) -> Result<ChatMessage>;
    /// Стриминговый запрос: дельты в `tx`, возвращает собранный ответ.
    ///
    /// Реализация по умолчанию — обёртка над [`LlmProvider::complete`]:
    /// отправляет весь текст одной дельтой.
    async fn stream(&self, req: ChatRequest, tx: mpsc::Sender<LlmEvent>) -> Result<ChatMessage> {
        let msg = self.complete(req).await?;
        if !msg.content.is_empty() {
            let _ = tx.send(LlmEvent::Delta(msg.content.clone())).await;
        }
        let _ = tx.send(LlmEvent::Done(Usage::default())).await;
        Ok(msg)
    }
}

/// Реестр провайдеров из конфигурации.
#[derive(Clone)]
pub struct LlmRegistry {
    providers: HashMap<String, Arc<dyn LlmProvider>>,
    default_name: String,
}

impl fmt::Debug for LlmRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LlmRegistry")
            .field("providers", &self.providers.keys().collect::<Vec<_>>())
            .field("default_name", &self.default_name)
            .finish()
    }
}

impl LlmRegistry {
    /// Строит реестр из конфигурации. Известным именам (`deepseek`, `kimi`,
    /// `glm`) соответствуют фабрики модулей; остальные — generic OpenAI-compat.
    ///
    /// # Errors
    /// `default_model` отсутствует в `models`.
    pub fn from_config(cfg: &Config) -> Result<Self> {
        let mut providers: HashMap<String, Arc<dyn LlmProvider>> = HashMap::new();
        for (name, mc) in &cfg.models {
            let provider = Self::build(name, mc)?;
            providers.insert(name.clone(), provider);
        }
        if !providers.contains_key(&cfg.default_model) {
            return Err(HarnessError::Config(format!(
                "default_model '{}' отсутствует в [models]",
                cfg.default_model
            )));
        }
        Ok(Self {
            providers,
            default_name: cfg.default_model.clone(),
        })
    }

    fn build(name: &str, mc: &ModelConfig) -> Result<Arc<dyn LlmProvider>> {
        match name {
            n if n.starts_with("deepseek") => deepseek::provider(name, mc),
            n if n.starts_with("kimi") => kimi::provider(name, mc),
            n if n.starts_with("glm") => glm::provider(name, mc),
            _ => openai_compat::generic_provider(name, mc),
        }
    }

    /// Провайдер по имени.
    ///
    /// # Errors
    /// Имя не найдено в реестре.
    pub fn get(&self, name: &str) -> Result<Arc<dyn LlmProvider>> {
        self.providers
            .get(name)
            .cloned()
            .ok_or_else(|| HarnessError::Llm(format!("модель '{name}' не настроена")))
    }

    /// Провайдер по умолчанию.
    pub fn default(&self) -> Arc<dyn LlmProvider> {
        self.providers
            .get(&self.default_name)
            .map(Arc::clone)
            .unwrap_or_else(|| unreachable!("default_model проверен в from_config"))
    }

    /// Имя модели по умолчанию.
    pub fn default_name(&self) -> &str {
        &self.default_name
    }

    /// Имена всех настроенных провайдеров.
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.providers.keys().cloned().collect();
        names.sort();
        names
    }
}
