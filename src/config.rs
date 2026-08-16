//! Конфигурация харнесса (`config.toml`).
//!
//! Порядок поиска конфига: `--config <path>` → `./arch-harness.toml` →
//! `~/.config/arch-harness/config.toml` → встроенные дефолты.
//! Команда `arch init` пишет дефолтный конфиг и ассеты в [`Config::home_dir`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{HarnessError, Result};

/// Корневая конфигурация харнесса.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Имя модели по умолчанию (ключ в [`Config::models`]).
    pub default_model: String,
    /// Настроенные LLM-провайдеры (OpenAI-совместимые endpoint'ы).
    pub models: BTreeMap<String, ModelConfig>,
    /// Параметры агентного цикла.
    pub agent: AgentConfig,
    /// Локальная база знаний (каталоги доменных документов).
    pub knowledge: KnowledgeConfig,
    /// Веб-доступ (поиск, фетч, кураторский список архитектурных сайтов).
    pub web: WebConfig,
    /// Адаптеры кодовых харнессов (Claude Code, Qwen Code, OpenClaw, …).
    pub harnesses: BTreeMap<String, CodingHarnessConfig>,
    /// Настройки MCP.
    pub mcp: McpSettings,
    /// Каталоги плагинов (скиллы + MCP в одном пакете).
    pub plugins: PluginsConfig,
    /// Политика автономии инструментов (R-уровни).
    pub policy: PolicyConfig,
    /// Хуки жизненного цикла (shell-команды на событиях агента).
    pub hooks: HooksConfig,
    /// Изоляция окружения bash-команд (scrub секретоподобных переменных).
    pub bash: BashConfig,
    /// Настройки планировщика.
    pub cron: CronSettings,
    /// Пути к ассетам, отчётам и сессиям.
    pub paths: PathsConfig,
    /// Откуда конфиг загружен (нужно `harness_run` для горячего
    /// перечитывания адаптеров — правки config.toml подхватываются без
    /// перезапуска сессии). В файл не сериализуется.
    #[serde(skip)]
    pub loaded_from: Option<PathBuf>,
}

/// Конфигурация одного LLM-провайдера (OpenAI-совместимый API).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelConfig {
    /// Базовый URL API, напр. `https://api.deepseek.com/v1`.
    pub base_url: String,
    /// Идентификатор модели, напр. `deepseek-v4-flash`.
    pub model: String,
    /// Имя переменной окружения с API-ключом.
    pub api_key_env: String,
    /// Запасной путь к файлу с ключом (`~` раскрывается): читается, если
    /// переменная окружения не задана (кейс theseus: ключ Kimi лежит в
    /// `~/.kimi_api_key`, а процесс харнесса env не видит). Содержимое
    /// никуда не выводится — только путь в тексте ошибки.
    #[serde(default)]
    pub api_key_file: Option<String>,
    /// Максимум токенов ответа.
    pub max_tokens: Option<u32>,
    /// Температура сэмплирования.
    pub temperature: Option<f32>,
    /// Бюджет тишины (сек): ожидание заголовков и пауза между чанками стрима.
    pub timeout_secs: u64,
    /// Окно контекста модели в токенах (если задано): автоматическая
    /// компактификация работает от min(agent.context_budget_tokens, этого
    /// окна) — пороги `compact_l1_pct`/`compact_l3_pct` (70%/95%) привязаны
    /// к реальному пределу API, а не к статичному бюджету.
    pub context_limit: Option<usize>,
    /// JSON-объект, сливаемый в тело запроса при включённом ризонинге
    /// (`/think on`): напр. `{"thinking": {"type": "enabled"}}` (DeepSeek V4,
    /// GLM-4.x) или `{"reasoning_effort": "max"}` (Kimi K3).
    /// None — переключение ризонинга для модели не настроено.
    pub thinking_on: Option<serde_json::Map<String, serde_json::Value>>,
    /// То же при `/think off`: `{"thinking": {"type": "disabled"}}` или
    /// `{"reasoning_effort": "low"}`.
    pub thinking_off: Option<serde_json::Map<String, serde_json::Value>>,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            model: String::new(),
            api_key_env: String::new(),
            api_key_file: None,
            max_tokens: Some(8192),
            temperature: None,
            timeout_secs: 180,
            context_limit: None,
            thinking_on: None,
            thinking_off: None,
        }
    }
}

impl ModelConfig {
    /// Читает API-ключ из переменной окружения (содержимое не логируется).
    pub fn api_key(&self) -> Result<String> {
        std::env::var(&self.api_key_env).map_err(|_| {
            HarnessError::Config(format!(
                "переменная окружения {} не установлена (нужен API-ключ для {})",
                self.api_key_env, self.base_url
            ))
        })
    }
}

/// Параметры агентного цикла.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    /// Максимум итераций «модель ↔ инструменты» за один ход.
    pub max_tool_turns: usize,
    /// Бюджет контекста в токенах (грубая оценка, 4 символа ≈ 1 токен).
    pub context_budget_tokens: usize,
    /// Стримить ответы модели (дельты в TUI/stdout).
    pub stream: bool,
    /// Порог L1-компактификации, % бюджета: маскирование старых
    /// tool-результатов (усечение с пометкой).
    pub compact_l1_pct: usize,
    /// Порог L3-компактификации, % бюджета: LLM-саммари истории в одно
    /// сообщение (последняя user-задача не трогается). >100 — отключено.
    pub compact_l3_pct: usize,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_tool_turns: 4800,
            context_budget_tokens: 6_000_000,
            stream: true,
            compact_l1_pct: 70,
            compact_l3_pct: 95,
        }
    }
}

/// Локальная база знаний для `kb search`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct KnowledgeConfig {
    /// Каталоги с доменными документами.
    pub dirs: Vec<PathBuf>,
    /// Расширения файлов для индексации.
    pub extensions: Vec<String>,
}

impl Default for KnowledgeConfig {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_default();
        Self {
            // Нейтральные плейсхолдеры: свои каталоги задаются в config.toml.
            dirs: vec![
                home.join("knowledge/architecture"),
                home.join("knowledge/papers"),
                home.join("knowledge/skills"),
            ],
            extensions: vec!["md".into(), "txt".into(), "rst".into()],
        }
    }
}

/// Кураторский сайт архитектурных знаний.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchSite {
    /// Короткое имя, напр. `aws-arch`.
    pub name: String,
    /// Базовый URL, напр. `https://docs.aws.amazon.com/architecture/`.
    pub base_url: String,
    /// Домен для site:-ограниченного поиска, напр. `docs.aws.amazon.com`.
    pub domain: String,
    /// Однострочное описание (чем сайт полезен архитектору).
    pub description: String,
}

/// Настройки веб-доступа.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WebConfig {
    /// Endpoint поиска (DuckDuckGo HTML).
    pub search_base: String,
    /// User-Agent для запросов.
    pub user_agent: String,
    /// Таймаут запроса, секунды.
    pub timeout_secs: u64,
    /// Максимум символов текста после html→text.
    pub max_fetch_chars: usize,
    /// Кураторский список сайтов для архитектора.
    pub arch_sites: Vec<ArchSite>,
}

impl Default for WebConfig {
    fn default() -> Self {
        let site = |name: &str, base: &str, domain: &str, desc: &str| ArchSite {
            name: name.into(),
            base_url: base.into(),
            domain: domain.into(),
            description: desc.into(),
        };
        Self {
            search_base: "https://html.duckduckgo.com/html/".into(),
            user_agent: "arch-harness/0.1 (+solution-architect harness)".into(),
            timeout_secs: 30,
            max_fetch_chars: 24_000,
            arch_sites: vec![
                site("aws-arch", "https://docs.aws.amazon.com/architecture/", "docs.aws.amazon.com", "AWS Architecture Center: reference architectures, Well-Architected"),
                site("azure-arch", "https://learn.microsoft.com/azure/architecture/", "learn.microsoft.com", "Azure Architecture Center: паттерны, reference architectures"),
                site("gcp-arch", "https://cloud.google.com/architecture", "cloud.google.com", "Google Cloud Architecture Center"),
                site("fowler", "https://martinfowler.com/architecture/", "martinfowler.com", "Мартин Фаулер: эссе по архитектуре, микросервисам, эволюционному дизайну"),
                site("infoq-arch", "https://www.infoq.com/architecture-design/", "infoq.com", "InfoQ Architecture & Design: статьи и тренды"),
                site("microservices-io", "https://microservices.io/", "microservices.io", "Каталог паттернов микросервисов (Крис Ричардсон)"),
                site("c4", "https://c4model.com/", "c4model.com", "C4 model: нотация визуализации архитектуры"),
                site("arc42", "https://docs.arc42.org/", "docs.arc42.org", "arc42: шаблон документирования архитектуры"),
                site("togaf", "https://pubs.opengroup.org/togaf-standard/", "pubs.opengroup.org", "TOGAF Standard (Open Group)"),
                site("sei", "https://insights.sei.cmu.edu/library/", "insights.sei.cmu.edu", "SEI/CAD: ATAM, архитектурные тактики, quality attributes"),
                site("awesome-arch", "https://awesome-architecture.com/", "awesome-architecture.com", "Кураторский список ресурсов по software architecture"),
            ],
        }
    }
}

/// Как адаптер передаёт промпт кодовому харнессу.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PromptMode {
    /// Промпт — позиционный аргумент (`claude "..."`).
    Positional,
    /// Промпт через флаг (`agent --prompt "..."`).
    Flag,
    /// Промпт пишется в stdin.
    Stdin,
}

/// Адаптер кодового харнесса (Claude Code, Qwen Code, OpenClaw, Hermes, Theseus, CodeWhale).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CodingHarnessConfig {
    /// Имя бинаря в PATH.
    pub binary: String,
    /// Дополнительные аргументы (плейсхолдер `{prompt}` подставляется при PromptMode::Flag).
    pub args: Vec<String>,
    /// Режим передачи промпта.
    pub prompt_mode: PromptMode,
    /// Дополнительные переменные окружения.
    pub env: BTreeMap<String, String>,
    /// Таймаут прогона, секунды (абсолютный потолок).
    pub timeout_secs: u64,
    /// Таймаут тишины, секунды: прогон прерывается, если харнесс не пишет
    /// в stdout/stderr И не меняет файлы в репозитории дольше этого срока.
    /// 0 — отключить (только абсолютный потолок). Работающий молча харнесс
    /// (длинный tool-вызов внутри Claude Code и т.п.) при наличии активности
    /// файловой системы НЕ считается зависшим.
    pub idle_timeout_secs: u64,
    /// Авто-коммит незакоммиченных правок после успешного прогона
    /// (`Termination::Completed`): контракт TASK.md требует от исполнителя
    /// финального коммита, но исполнитель может его не сделать — тогда
    /// харнесс сам фиксирует оставшиеся изменения (кроме `.arch-handoff/`
    /// и мусора вида `__pycache__/`). Работа исполнителя всегда оказывается
    /// в git — это точка интеграции параллельных прогонов.
    pub auto_commit: bool,
}

impl Default for CodingHarnessConfig {
    fn default() -> Self {
        Self {
            binary: String::new(),
            args: Vec::new(),
            prompt_mode: PromptMode::Positional,
            env: BTreeMap::new(),
            timeout_secs: 1800,
            idle_timeout_secs: 600,
            auto_commit: true,
        }
    }
}

/// Настройки MCP: путь к файлу серверов (формат как у Claude Code `mcp.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct McpSettings {
    /// Путь к `mcp.json` (`{"mcpServers": {name: {command, args, env}}}`).
    pub servers_file: PathBuf,
    /// Подключаться к серверам при старте (иначе — лениво, по первому вызову).
    pub connect_on_start: bool,
    /// Таймаут MCP-вызова, секунды.
    pub timeout_secs: u64,
}

impl Default for McpSettings {
    fn default() -> Self {
        Self {
            servers_file: Config::home_dir().join("mcp.json"),
            connect_on_start: false,
            timeout_secs: 60,
        }
    }
}

/// Настройки плагинов: каталоги пакетов «скиллы + MCP»
/// (открытый стандарт agent-plugins.org: `plugin.json` + `skills/*/SKILL.md`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PluginsConfig {
    /// Каталоги с плагинами (каждый прямой потомок — плагин).
    pub dirs: Vec<PathBuf>,
    /// Подхватывать MCP-серверы из плагинов (`mcpServers` в plugin.json
    /// или `.mcp.json` в корне плагина) в общий MCP-пул.
    pub include_mcp: bool,
    /// Исполнять хуки плагинов (`hooks/hooks.json`): shell-команды из
    /// установленных плагинов — включайте только для доверенных библиотек.
    /// Дефолт `true` (как у include_mcp).
    pub include_hooks: bool,
}

impl Default for PluginsConfig {
    fn default() -> Self {
        Self {
            // По умолчанию — только библиотека харнесса; свои каталоги
            // плагинов добавляются в config.toml.
            dirs: vec![Config::home_dir().join("plugins")],
            include_mcp: true,
            include_hooks: true,
        }
    }
}

/// Хуки жизненного цикла (см. [`crate::hooks`]). Пусто — без хуков.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HooksConfig {
    /// Спецификации хуков: `[[hooks.specs]]` с event/tool/command/timeout_secs.
    pub specs: Vec<crate::hooks::HookSpec>,
}

/// Изоляция окружения bash-команд (defensive pattern DeepSeek Harness:
/// «никогда не давай недоверенному выводу ambient-окружение»).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BashConfig {
    /// Выбрасывать из окружения дочернего процесса переменные с
    /// секретоподобными именами (токены имени: KEY, SECRET, TOKEN,
    /// PASSWORD, PASSWD, PASS, CREDENTIAL(S), AUTH). Дефолт `true`:
    /// команды, которые пишет модель, не видят ключи провайдеров и не
    /// могут отправить их наружу (curl, логи, spill-файлы).
    pub env_scrub: bool,
    /// Точные имена переменных, которые пропускаются НЕСМОТРЯ на scrub
    /// (например `GH_TOKEN` для `gh`). Пусто — исключений нет.
    pub env_allow: Vec<String>,
}

impl Default for BashConfig {
    fn default() -> Self {
        Self {
            env_scrub: true,
            env_allow: Vec::new(),
        }
    }
}

/// Политика автономии инструментов (R-уровни R0–R5, по AI-Disrupt PDLC).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PolicyConfig {
    /// Уровень автономии: R0 (всё через человека) … R5 (полная).
    /// Дефолт R2: чтения и изменения в рабочем каталоге — авто, деструктив —
    /// отказ. R4 — деструктив с подтверждением. R5 не рекомендуется.
    pub autonomy: String,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            autonomy: "R2".into(),
        }
    }
}

/// Настройки планировщика md-задач.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CronSettings {
    /// Файл расписания (`cron.toml`).
    pub file: PathBuf,
    /// Каталог отчётов крона (по умолчанию `paths.reports_dir/cron`).
    pub out_dir: Option<PathBuf>,
}

impl Default for CronSettings {
    fn default() -> Self {
        Self {
            file: Config::home_dir().join("cron.toml"),
            out_dir: None,
        }
    }
}

/// Пути к ассетам и данным харнесса.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PathsConfig {
    /// Корень ассетов (prompts/, rubrics/, benchmarks/, ascii/).
    pub assets_dir: PathBuf,
    /// Каталог отчётов (рубрики, бенчи, контроль).
    pub reports_dir: PathBuf,
    /// Каталог сессий (append-only журналы агента).
    pub sessions_dir: PathBuf,
}

impl Default for PathsConfig {
    fn default() -> Self {
        let home = Config::home_dir();
        Self {
            assets_dir: home.join("assets"),
            reports_dir: home.join("reports"),
            sessions_dir: home.join("sessions"),
        }
    }
}

impl PathsConfig {
    /// Каталог библиотеки промптов.
    pub fn prompts_dir(&self) -> PathBuf {
        self.assets_dir.join("prompts")
    }

    /// Каталог якорных рубрик.
    pub fn rubrics_dir(&self) -> PathBuf {
        self.assets_dir.join("rubrics")
    }

    /// Каталог бенчмарков.
    pub fn benchmarks_dir(&self) -> PathBuf {
        self.assets_dir.join("benchmarks")
    }
}

/// Карты ризонинга в стиле `thinking: {type: enabled/disabled}`
/// (DeepSeek V4, GLM-4.x/5.x): возвращает (on, off).
fn thinking_type_maps() -> (
    Option<serde_json::Map<String, serde_json::Value>>,
    Option<serde_json::Map<String, serde_json::Value>>,
) {
    let make = |kind: &str| {
        let mut inner = serde_json::Map::new();
        inner.insert("type".into(), kind.into());
        let mut outer = serde_json::Map::new();
        outer.insert("thinking".into(), inner.into());
        Some(outer)
    };
    (make("enabled"), make("disabled"))
}

impl Default for Config {
    fn default() -> Self {
        // Модели сверены с официальной документацией (август 2026):
        // - DeepSeek: deepseek-chat/reasoner сняты 2026-07-24 → v4-flash/v4-pro,
        //   ризонинг — `thinking: {type: enabled/disabled}` (api-docs.deepseek.com/
        //   guides/thinking_mode); с tools требуется эхо reasoning_content (см. llm.rs);
        // - Kimi: kimi-k2 снят 2025-05-25 → kimi-k3 (1M ctx), ризонинг —
        //   `reasoning_effort: low/high/max` (platform.kimi.ai/docs/models);
        // - GLM: glm-4.6 → glm-4.7, ризонинг — `thinking: {type: ...}` (docs.z.ai).
        let mut models = BTreeMap::new();
        let (think_on, think_off) = thinking_type_maps();
        models.insert(
            "deepseek".into(),
            ModelConfig {
                base_url: "https://api.deepseek.com/v1".into(),
                model: "deepseek-v4-flash".into(),
                api_key_env: "DEEPSEEK_API_KEY".into(),
                context_limit: Some(1_000_000),
                thinking_on: think_on.clone(),
                thinking_off: think_off.clone(),
                ..ModelConfig::default()
            },
        );
        models.insert(
            "deepseek-pro".into(),
            ModelConfig {
                base_url: "https://api.deepseek.com/v1".into(),
                model: "deepseek-v4-pro".into(),
                api_key_env: "DEEPSEEK_API_KEY".into(),
                timeout_secs: 300,
                context_limit: Some(1_000_000),
                thinking_on: think_on.clone(),
                thinking_off: think_off,
                ..ModelConfig::default()
            },
        );
        models.insert(
            "kimi".into(),
            ModelConfig {
                // Официальная coding-поверхность Kimi Code (доки kimi.com/code):
                // модели k3 / k3-256k; старая /v1 → 404. thinking-параметры
                // поверхность НЕ принимает (400 без reasoning_content в истории
                // — его харнесс эхом возвращает); temperature — только 1,
                // поэтому не шлём вовсе. Ключ — env KIMI_API_KEY или файл.
                base_url: "https://api.kimi.com/coding/v1".into(),
                model: "k3".into(),
                api_key_env: "KIMI_API_KEY".into(),
                api_key_file: Some("~/.kimi_api_key".into()),
                context_limit: Some(1_000_000),
                ..ModelConfig::default()
            },
        );
        let (glm_on, glm_off) = thinking_type_maps();
        let glm_entry = |model: &str, timeout: u64, context_limit: usize| ModelConfig {
            // Международная площадка Z.AI (для Китая — open.bigmodel.cn).
            base_url: "https://api.z.ai/api/paas/v4".into(),
            model: model.into(),
            api_key_env: "ZHIPU_API_KEY".into(),
            timeout_secs: timeout,
            context_limit: Some(context_limit),
            thinking_on: glm_on.clone(),
            thinking_off: glm_off.clone(),
            ..ModelConfig::default()
        };
        models.insert("glm".into(), glm_entry("glm-5.2", 180, 1_000_000));
        models.insert("glm-4.7".into(), glm_entry("glm-4.7", 180, 204_800));
        models.insert("glm-air".into(), glm_entry("glm-4.5-air", 120, 131_072));
        models.insert("glm-flash".into(), glm_entry("glm-4.7-flash", 120, 204_800));

        let harness = |binary: &str, args: &[&str], mode: PromptMode| CodingHarnessConfig {
            binary: binary.into(),
            args: args.iter().map(|s| (*s).into()).collect(),
            prompt_mode: mode,
            ..CodingHarnessConfig::default()
        };
        let mut harnesses = BTreeMap::new();
        // Claude Code в headless (`-p` без TTY) на файловых операциях встаёт на
        // permission-промпте и ждёт вечно → --dangerously-skip-permissions
        // обязателен для unattended-прогонов. Права процесса ограничены
        // каталогом репозитория; уберите флаг в config.toml для интерактива.
        harnesses.insert(
            "claude-code".into(),
            harness(
                "claude",
                &["-p", "--dangerously-skip-permissions"],
                PromptMode::Stdin,
            ),
        );
        harnesses.insert("qwen-code".into(), harness("qwen", &[], PromptMode::Stdin));
        // Флаги валидированы живыми прогонами флота (бенч 04_payment-idempotency,
        // 2026-08): неверные режимы/флаги давали код 2 на argparse.
        harnesses.insert(
            "openclaw".into(),
            harness(
                "openclaw",
                &["agent", "--agent", "main", "--message", "{prompt}"],
                PromptMode::Flag,
            ),
        );
        harnesses.insert("hermes".into(), harness("hermes", &["-z", "{prompt}"], PromptMode::Flag));
        harnesses.insert("theseus".into(), harness("theseus", &["-p", "{prompt}"], PromptMode::Flag));
        harnesses.insert("codewhale".into(), harness("codewhale", &["-p", "{prompt}"], PromptMode::Flag));

        Self {
            default_model: "deepseek".into(),
            models,
            agent: AgentConfig::default(),
            knowledge: KnowledgeConfig::default(),
            web: WebConfig::default(),
            harnesses,
            mcp: McpSettings::default(),
            plugins: PluginsConfig::default(),
            policy: PolicyConfig::default(),
            hooks: HooksConfig::default(),
            bash: BashConfig::default(),
            cron: CronSettings::default(),
            paths: PathsConfig::default(),
            loaded_from: None,
        }
    }
}

impl Config {
    /// Домашний каталог харнесса (`~/.arch-harness` или `$ARCH_HOME`).
    pub fn home_dir() -> PathBuf {
        std::env::var_os("ARCH_HOME")
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|h| h.join(".arch-harness")))
            .unwrap_or_else(|| PathBuf::from(".arch-harness"))
    }

    /// Каталог конфигурации (`~/.config/arch-harness`).
    pub fn config_dir() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("arch-harness")
    }

    /// Загружает конфиг: `--config` → `./arch-harness.toml` →
    /// `~/.config/arch-harness/config.toml` → дефолты.
    ///
    /// # Errors
    /// Ошибка чтения/разбора файла, если явный путь задан и недоступен.
    pub fn load(cli_path: Option<&Path>) -> Result<Self> {
        let candidates: Vec<PathBuf> = match cli_path {
            Some(p) => vec![p.to_path_buf()],
            None => vec![
                PathBuf::from("arch-harness.toml"),
                Self::config_dir().join("config.toml"),
            ],
        };
        for path in &candidates {
            if path.is_file() {
                let text = std::fs::read_to_string(path)
                    .map_err(|e| HarnessError::io(path, e))?;
                let mut cfg: Config = toml::from_str(&text)?;
                cfg.expand_tildes();
                cfg.loaded_from = Some(path.clone());
                return Ok(cfg);
            }
        }
        let mut cfg = Config::default();
        cfg.expand_tildes();
        Ok(cfg)
    }

    /// Сохраняет конфиг в `~/.config/arch-harness/config.toml`.
    ///
    /// # Errors
    /// Ошибка создания каталога или записи файла.
    pub fn save_default(&self) -> Result<PathBuf> {
        let path = Self::config_dir().join("config.toml");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| HarnessError::io(parent, e))?;
        }
        let text = toml::to_string_pretty(self)
            .map_err(|e| HarnessError::Config(format!("сериализация конфига: {e}")))?;
        std::fs::write(&path, text).map_err(|e| HarnessError::io(&path, e))?;
        Ok(path)
    }

    /// Подставляет `~` в начале путей (toml не раскрывает тильду).
    fn expand_tildes(&mut self) {
        let expand = |p: &mut PathBuf| {
            if let Ok(s) = p.to_path_buf().into_os_string().into_string() {
                if let Some(rest) = s.strip_prefix("~/") {
                    if let Some(home) = dirs::home_dir() {
                        *p = home.join(rest);
                    }
                }
            }
        };
        for d in &mut self.knowledge.dirs {
            expand(d);
        }
        expand(&mut self.mcp.servers_file);
        for d in &mut self.plugins.dirs {
            expand(d);
        }
        expand(&mut self.cron.file);
        if let Some(d) = &mut self.cron.out_dir {
            expand(d);
        }
        expand(&mut self.paths.assets_dir);
        expand(&mut self.paths.reports_dir);
        expand(&mut self.paths.sessions_dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_three_providers() {
        let cfg = Config::default();
        for name in ["deepseek", "kimi", "glm"] {
            assert!(cfg.models.contains_key(name), "нет модели {name}");
        }
        assert!(cfg.harnesses.contains_key("claude-code"));
        assert!(cfg.harnesses.contains_key("codewhale"));
        assert!(cfg.web.arch_sites.len() >= 8);
    }

    #[test]
    fn default_adapters_carry_validated_flags() {
        // Дефолты валидированы живыми прогонами флота (2026-08): неверный
        // флаг/режим давал argparse-код 2 (hermes: «unrecognized arguments: -p»).
        let cfg = Config::default();
        let h = &cfg.harnesses;
        let flags = |name: &str| (h[name].args.clone(), h[name].prompt_mode.clone());
        assert_eq!(flags("hermes"), (vec!["-z".to_string(), "{prompt}".to_string()], PromptMode::Flag));
        assert_eq!(flags("theseus"), (vec!["-p".to_string(), "{prompt}".to_string()], PromptMode::Flag));
        assert_eq!(flags("codewhale"), (vec!["-p".to_string(), "{prompt}".to_string()], PromptMode::Flag));
        assert_eq!(
            flags("openclaw"),
            (
                vec![
                    "agent".to_string(),
                    "--agent".to_string(),
                    "main".to_string(),
                    "--message".to_string(),
                    "{prompt}".to_string()
                ],
                PromptMode::Flag
            )
        );
    }

    #[test]
    fn load_remembers_config_path_for_hot_reload() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, "default_model = \"glm\"\n").expect("write");
        let cfg = Config::load(Some(&path)).expect("load");
        assert_eq!(cfg.loaded_from.as_deref(), Some(path.as_path()));
        assert_eq!(cfg.default_model, "glm");
        // Снапшот дефолтов без файла — loaded_from пуст (горячего источника нет).
        assert!(Config::default().loaded_from.is_none());
        // loaded_from не утекает в сериализацию конфига.
        let text = toml::to_string_pretty(&cfg).expect("serialize");
        assert!(!text.contains("loaded_from"), "{text}");
    }

    #[test]
    fn flagship_models_default_to_1m_context() {
        let cfg = Config::default();
        for name in ["deepseek", "deepseek-pro", "kimi", "glm"] {
            let limit = cfg.models[name].context_limit.expect("context_limit задан");
            assert_eq!(limit, 1_000_000, "окно {name}");
        }
        // Бюджетные GLM остаются на своих окнах.
        assert_eq!(cfg.models["glm-air"].context_limit, Some(131_072));
    }

    #[test]
    fn config_roundtrips_through_toml() {
        let cfg = Config::default();
        let text = toml::to_string_pretty(&cfg).expect("serialize");
        let back: Config = toml::from_str(&text).expect("deserialize");
        assert_eq!(back.default_model, cfg.default_model);
        assert_eq!(back.models.len(), cfg.models.len());
    }
}
