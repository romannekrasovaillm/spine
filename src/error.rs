//! Корневой тип ошибок харнесса.
//!
//! Библиотечный слой: вызывающий может матчиться на варианты.
//! Прикладной слой (`main`, обработчики CLI) использует `anyhow`.

use std::path::{Path, PathBuf};

/// Результат по умолчанию для всех модулей харнесса.
pub type Result<T> = std::result::Result<T, HarnessError>;

/// Корневой тип ошибок харнесса.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum HarnessError {
    /// Ошибка конфигурации (файл, поля, переменные окружения).
    #[error("config: {0}")]
    Config(String),
    /// I/O-ошибка с привязкой к пути.
    #[error("io: {path}: {source}")]
    Io {
        /// Путь, на котором произошла ошибка.
        path: PathBuf,
        /// Исходная ошибка.
        source: std::io::Error,
    },
    /// I/O-ошибка без привязки к пути.
    #[error(transparent)]
    IoBare(#[from] std::io::Error),
    /// Сетевая ошибка HTTP-клиента.
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    /// Ошибка JSON-сериализации.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// Ошибка YAML-сериализации.
    #[error(transparent)]
    Yaml(#[from] serde_yaml::Error),
    /// Ошибка разбора TOML.
    #[error(transparent)]
    Toml(#[from] toml::de::Error),
    /// Ошибка провайдера LLM.
    #[error("llm: {0}")]
    Llm(String),
    /// Ошибка инструмента.
    #[error("tool: {0}")]
    Tool(String),
    /// Ошибка MCP-клиента.
    #[error("mcp: {0}")]
    Mcp(String),
    /// Ошибка разбора/рендера mermaid.
    #[error("mermaid: {0}")]
    Mermaid(String),
    /// Ошибка движка рубрик.
    #[error("rubric: {0}")]
    Rubric(String),
    /// Ошибка раннера бенчмарков.
    #[error("bench: {0}")]
    Bench(String),
    /// Ошибка веб-доступа (поиск/фетч).
    #[error("web: {0}")]
    Web(String),
    /// Ошибка локальной базы знаний.
    #[error("kb: {0}")]
    Kb(String),
    /// Ошибка архитектурного контроля (fitness functions, линтер spine).
    #[error("control: {0}")]
    Control(String),
    /// Ошибка адаптера кодового харнесса.
    #[error("harness: {0}")]
    Harness(String),
    /// Ошибка планировщика.
    #[error("cron: {0}")]
    Cron(String),
    /// Ошибка агентного цикла.
    #[error("agent: {0}")]
    Agent(String),
    /// Ошибка TUI.
    #[error("tui: {0}")]
    Tui(String),
}

impl HarnessError {
    /// Обёртка для I/O-ошибки с привязкой к конкретному пути.
    pub fn io(path: impl AsRef<Path>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.as_ref().to_path_buf(),
            source,
        }
    }
}
