# Архитектура arch-harness

Устройство харнесса: карта модулей, замороженные контракты, поток данных
агентного цикла, акторная модель TUI, точки расширения.

Принципы: тонкий харнесс (никакой магии поверх OpenAI-совместимого
function calling), ошибки инструментов — данные для модели, а не паника;
секреты только из окружения; `unsafe` запрещён (`Cargo.toml`:
`unsafe_code = "forbid"`); ошибки — `HarnessError`/`Result` (thiserror,
`src/error.rs`), прикладной слой — anyhow с `.context()`.

## Карта модулей `src/`

| Модуль | Назначение |
|---|---|
| `main.rs` | Тонкая точка входа: clap-парсинг → вызов lib → код возврата. Все CLI-подкоманды. |
| `lib.rs` | Корень библиотеки `arch_harness`; реэкспорт `Config`, `HarnessError`, `Result`. |
| `config.rs` | `Config` и секции (`ModelConfig`, `AgentConfig`, `KnowledgeConfig`, `WebConfig`, `CodingHarnessConfig`, `McpSettings`, `CronSettings`, `PathsConfig`). Порядок поиска: `--config` → `./arch-harness.toml` → `~/.config/arch-harness/config.toml` → дефолты. |
| `error.rs` | `HarnessError` (thiserror): доменные варианты Config/Llm/Tool/Agent/Rubric/Bench/Control/Harness/Mcp/Web/Kb/Cron/Tui/Io и пр. |
| `llm.rs` | Контракт провайдеров: трейт `LlmProvider`, `LlmRegistry`, типы `ChatMessage`, `ToolCall`, `ToolSpec`, `ChatRequest`, `Usage`, `LlmEvent`. |
| `llm/openai_compat.rs` | Общий OpenAI-совместимый клиент `/chat/completions`: SSE-стриминг, инкрементальная сборка `tool_calls`, один retry на 429/5xx; `generic_provider` для произвольных endpoint'ов. |
| `llm/{deepseek,kimi,glm}.rs` | Тонкие фабрики-пресеты над `openai_compat`. |
| `tool.rs` | Контракт инструментов: трейт `Tool`, `ToolRegistry`, `ToolContext`, `ToolOutput`. |
| `tools.rs`, `tools/{bash,fs}.rs` | Ядро: `bash` (таймаут, лимит вывода), `read_file`, `write_file`, `edit_file`, `glob`, `grep`; `core_registry()` / `full_registry()`. |
| `agent.rs` | Агентный цикл `AgentSession`, события `AgentEvent`, JSONL-журнал сессии. |
| `agent/slash.rs` | Слэш-команды TUI (`docs/slash_commands.md`). |
| `agent/prompts.rs` | Библиотека промптов `assets/prompts/*.md`; плейсхолдеры `{{var}}`. |
| `mermaid.rs`, `mermaid/{parse,model,layout,draw}.rs` | Подмножество mermaid (flowchart, sequenceDiagram) → символьная сетка; layered layout (Sugiyama-lite). |
| `rubric.rs` | Движок рубрик: якорные/динамические, LLM-судья, `RubricReport`. |
| `bench.rs` | Бенчмарки: YAML-сценарий → ответ модели → оценка рубрикой → отчёты md+json. |
| `web.rs` | DuckDuckGo-поиск, site:-ограничение по кураторским сайтам, fetch html→text. |
| `kb.rs` | Локальная база знаний: walkdir + fuzzy-скоринг + сниппеты. |
| `mcp.rs` | MCP-клиент: stdio NDJSON JSON-RPC, `McpManager`, `McpToolAdapter`. |
| `harness.rs` | Handoff-пакеты `.arch-handoff/` и запуск кодовых харнессов. |
| `control.rs` | Архитектурный контроль: score, линтер spine, сенсоры, fitness, ADR. |
| `cron.rs` | Планировщик md-задач: `cron.toml`, дюжность, прогон, отчёт. |
| `tui.rs`, `tui/{app,render,text,theme}.rs` | TUI: event loop, состояние, отрисовка, markdown-lite, палитра Tokyo Night. |
| `assets.rs` | Встроенные ассеты (`include_str!` из `assets/`, `examples/`); `write_defaults` для `arch init` (не затирает существующие файлы). |

Каждый доменный модуль экспортирует `tools() -> Vec<Arc<dyn Tool>>`;
`tools::full_registry()` собирает ядро + домены (mermaid, rubric, web, kb,
control, harness).

## Контракты

### `Tool` (`src/tool.rs`)

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;                              // имя, описание, JSON Schema
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput>;
}
```

- `ToolOutput { content, is_error }` — `ok()` / `err()`; `truncated(max)`
  усекает с пометкой.
- `ToolRegistry::dispatch` превращает ошибку инструмента в
  `ToolOutput::err(...)` — агентный цикл не рвётся на сбое одного вызова.
- `ToolContext { cwd, config, llm }` — рабочий каталог (все относительные
  пути инструментов резолвятся от него), конфиг, опциональный реестр LLM
  (нужен рубрикам и динамическим проверкам).

### `LlmProvider` (`src/llm.rs`)

```rust
#[async_trait]
pub trait LlmProvider: Send + Sync + fmt::Debug {
    fn name(&self) -> &str;
    fn model(&self) -> &str;
    async fn complete(&self, req: ChatRequest) -> Result<ChatMessage>;
    async fn stream(&self, req: ChatRequest, tx: mpsc::Sender<LlmEvent>)
        -> Result<ChatMessage>;  // дефолт — обёртка над complete
}
```

- `LlmRegistry::from_config` строит провайдеров из `[models]`: имена
  `deepseek*`/`kimi*`/`glm*` → фабрики модулей, любое другое имя →
  `openai_compat::generic_provider`. `default_model` обязан присутствовать.
- API-ключ читается лениво из `ModelConfig::api_key_env` на запросе:
  отсутствие ключа одного провайдера не роняет реестр. Ключ не выводится
  в `Debug` и не логируется.

## Поток данных агентного цикла (`src/agent.rs`)

`AgentSession::send(input, events)`:

1. `input` пушится в историю и в журнал (`user`).
2. До `agent.max_tool_turns` итераций (дефолт 4800):
   - `compact_history()` — при превышении `context_budget_tokens`
     (дефолт 6 000 000 — «лимит задаёт провайдер»: компактификация на практике
     отложена до HTTP 413, который покрывает on-error compact&resubmit;
     оценка 4 символа ≈ 1 токен): сначала усечение старых
     tool-сообщений до 500 символов (кроме 4 последних), затем удаление
     старейших сообщений (хвост из 6 не трогается); факт пишется в журнал.
   - `build_request()` — системный промпт + история + `tools.specs()`;
     `temperature`/`max_tokens` из `ModelConfig` активного провайдера;
     `thinking` — из сессионного переключателя `/think`.
   - Запрос: стриминг (`stream` + канал событий) или `complete`.
   - Ответ без `tool_calls` → пуш в историю/журнал, `AgentEvent::TurnDone`,
     возврат текста.
   - Иначе для каждого вызова: `AgentEvent::ToolStart` →
     `ToolRegistry::dispatch` с таймаутом 300 с → результат усекается до
     8192 символов → `ChatMessage::tool_result` в историю, `AgentEvent::ToolEnd`.
3. Лимит исчерпан → ход НЕ падает: финальный запрос с пустым `tools`
   и служебной репликой (только в запрос) «дай ответ по собранному»;
   заметка в UI, событие `tool_turn_limit` в журнале.

Журнал: append-only JSONL `sessions/session-<yyyymmdd-hhmmss>.jsonl`,
события `system`/`user`/`assistant`/`tool`/`event` с ISO-метками; каждая
запись флашится (журнал переживает падение). Недоступность журнала — не
фатальна (warn в tracing).

## Акторная модель TUI (`src/tui.rs`, `src/tui/app.rs`)

- `App` владеет всем состоянием в event loop — **без `Arc<Mutex>`**.
- Ход агента и слэш-команды выполняются в `tokio::spawn`; сессия
  (`AgentSession` — не `Send`-ресурс в UI) возвращается владельцу сообщением
  `AppMessage` по bounded-каналу `mpsc(256)` (backpressure).
- Дельты/статусы инструментов форвардятся как `AgentEvent` по `mpsc(64)`.
- Цикл (`tui::run`): отрисовка кадра → `tokio::select!` по crossterm
  `EventStream` (клавиши), `AppMessage`, `Ctrl-C`, тикер спиннера 120 мс.
- Терминал: RAII-гард `TerminalGuard` (raw mode + alternate screen, Drop
  восстанавливает) + panic hook, восстанавливающий терминал перед печатью
  паники. Выход: `q` / `Ctrl-C` / `Esc`.
- Экраны: `Splash` (ASCII-баннер из `assets::BANNER`) → `Chat`; фатальная
  ошибка инициализации → экран `Fatal`.

## Как добавить свой инструмент

1. Реализуйте `Tool` (см. образцы: `src/tools/fs.rs` — файловые,
   `src/kb.rs::KbSearchTool` — доменный):
   - `spec()`: `ToolSpec { name: "my_tool" (snake_case), description
     (для модели: что делает и когда вызывать), parameters: json!({...JSON Schema}) }`;
   - `call()`: разбор аргументов, работа, `Ok(ToolOutput::ok(...))`;
     ожидаемые сбои — `ToolOutput::err(...)` (модель увидит и скорректирует).
   - Пути резолвите через `ctx.resolve(path)`, вывод усекайте
     `ToolOutput::truncated`.
2. Зарегистрируйте: своя доменная функция `tools()` + строка в
   `tools::domain_tools()` (`src/tools.rs`), либо в `core_registry()`,
   если инструмент ядерный.
3. Проверка: `/tools` в TUI, `arch run "…"` с задачей, требующей вызова.
   Тесты — по образцу `agent.rs::tests::EchoTool`.

## Как добавить свой LLM-провайдер

Провайдеры — OpenAI-совместимые endpoint'ы; в большинстве случаев код не
нужен: добавьте секцию в `config.toml`:

```toml
[models.my-llm]
base_url = "https://llm.example.com/v1"
model = "my-model"
api_key_env = "MY_LLM_API_KEY"
```

Имя не из пресетов → `openai_compat::generic_provider` (`src/llm.rs::build`).
Особый транспорт: фабрика по образцу `src/llm/deepseek.rs` + ветка в
`LlmRegistry::build`. Подробности и сетевые замечания — `docs/models.md`.
