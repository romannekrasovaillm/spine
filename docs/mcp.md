# MCP-интеграции

Два режима: **клиент** (`src/mcp.rs`) — подключение внешних MCP-серверов,
их инструменты доступны агенту и из CLI; **сервер** (`src/mcp_server.rs`,
`arch mcp serve`) — архитектурный контроль Spine наружу кодовым агентам
(Claude Code и др.) в момент написания кода (ADR-008). Формат конфигурации
клиента — как у Claude Code.

## MCP-сервер: `arch mcp serve`

Stdio-сервер JSON-RPC 2.0 (NDJSON, как у клиента): кодовый агент получает
architectural verdict (`passed` + находки) **в момент написания кода**, а не
на приёмке handoff-пакета. Сервер read-only: ничего не пишет в репозиторий
клиента, все цели — аргументами вызова.

### Подключение (Claude Code)

```bash
claude mcp add arch-spine -- arch mcp serve
```

либо вручную в `~/.claude.json` / `.mcp.json` проекта (тот же формат, что
и у нашего клиента):

```json
{
  "mcpServers": {
    "arch-spine": {
      "command": "arch",
      "args": ["mcp", "serve"]
    }
  }
}
```

Запуск из каталога целевого проекта: относительные пути аргументов
(`model`, `model/` для `model_query` по умолчанию) резолвятся от cwd
процесса сервера, который задаёт клиент.

### Инструменты

| Инструмент | Аргументы | Verdict |
|---|---|---|
| `spine_lint` | `path` | линтер ARCHITECTURE-SPINE.md: `passed=false` при находках error (дубли AD-id, пустые Binds/Prevents/Rule, заглушки, непиннутые версии, битые ссылки AD) |
| `fitness_check` | `repo`, `constraints?` | прогон CONSTRAINTS.yaml (дефолт `<repo>/.arch-handoff/CONSTRAINTS.yaml`): must_contain / must_not_contain / file_exists / command_succeeds; `passed=false` — правила нарушены |
| `significance_score` | `triggers` | маршрут значимости Fast/Standard/Critical по 15 триггерам (информационный, без `passed`) |
| `trace_check` | `case` | позвенная трассируемость `REQ → NFR → AD/ADR → CMP → правило`: AD без правила и без `unverifiable` — error; verdict + `report_markdown` для evidence bundle |
| `model_query` | `dir?`, `id?`, `type?` | список сущностей модели (фильтр по типу) или карточка сущности со связями и обратными ссылками |
| `rubric_run` | `rubric`, `target` \| `target_text`, `model?` | оценка документа рубрикой LLM-судьёй (ADR-004; нужен API-ключ из конфига arch) |

Контракт ответа контрольных инструментов (`structuredContent` и тот же
объект pretty-JSON в `content[0].text`):

```json
{
  "passed": false,
  "issue_count": 2,
  "error_count": 2,
  "warn_count": 0,
  "issues": [{"severity": "error", "rule": "spine_present", "file": "…", "line": 0, "message": "…"}],
  "summary": "Правил: 1, нарушений: 2 (error: 2, warn: 0)"
}
```

`passed: false` — основание **отказать изменению**, нарушающему `AD-*`,
перечислив находки (эта инструкция отдаётся клиенту и в `initialize.instructions`).

### Ошибки

- Битая JSON-строка → `-32700`; неизвестный метод → `-32601`; битые
  аргументы / неизвестный инструмент → `-32602`. Сервер не падает ни на
  каком вводе, цикл живёт до EOF stdin.
- Доменный сбой выполнения (файл не читается, сущность не найдена) —
  `result` с `isError: true` и текстом причины (не protocol error).
- `rubric_run` без API-ключа провайдера — понятная JSON-RPC ошибка
  `-32603` (какой env/файл настроить; содержимое ключа не читается).

### Проверка без клиента

```bash
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' \
  '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"fitness_check","arguments":{"repo":"/path/to/repo"}}}' \
  | arch mcp serve
```

## MCP-клиент: подключение внешних серверов

## Файл серверов `mcp.json`

Путь — `mcp.servers_file` (дефолт `~/.arch-harness/mcp.json`; `arch init`
кладёт образец). Формат:

```json
{
  "mcpServers": {
    "filesystem": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "~/Документы"]
    },
    "fetch": {
      "command": "uvx",
      "args": ["mcp-server-fetch"]
    },
    "memory": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-memory"]
    }
  }
}
```

(это `examples/mcp.example.json`: filesystem — файловое дерево, fetch —
загрузка URL, memory — граф памяти). На сервер: `command` + опциональные
`args` и `env`.

## Транспорт и протокол

- **stdio**: сервер запускается дочерним процессом (`tokio::process`),
  обмен — **NDJSON**: сообщения JSON-RPC 2.0 строками в stdin/stdout, без
  `Content-Length`-фрейминга (стиль MCP).
- Handshake: `initialize` (`protocolVersion: "2025-06-18"`, clientInfo
  `arch-harness`; при протокольной ошибке — один fallback на `"2024-11-05"`)
  + `notifications/initialized`; далее `tools/list` и `tools/call`.
- Подключения и опросы серверов — конкурентно, лимит 4
  (`CONNECT_CONCURRENCY`); сбой одного сервера не роняет остальные.
- Таймаут — `mcp.timeout_secs` (дефолт 60 с) на вызов/подключение.
- `McpManager::shutdown` — graceful: `kill` дочерних процессов.

## Именование и вызов

Инструменты всех серверов сливаются с составными именами **`server__tool`**
(например, `filesystem__read_file`, `fetch__fetch`):

```bash
arch mcp list                                   # серверы + инструменты
arch mcp call fetch__fetch '{"url": "https://example.com/spec"}'
```

В TUI: `/mcp list` — подключение по требованию и список; сбой (нет файла,
сервер не поднялся) показывается текстом, не роняя сессию.

## Ленивый режим (`connect_on_start`)

Дефолт — `connect_on_start = false` (`src/config.rs::McpSettings`):

- **старт TUI не ждёт серверы** — никаких пауз на `npx`/`uvx`-загрузки;
- `/mcp list` подключается по требованию, показывает инструменты и отключается;
- **инструменты MCP доступны модели только при `connect_on_start = true`**:
  тогда при старте TUI `McpManager` поднимается, и каждый MCP-инструмент
  регистрируется в реестре агента через `McpToolAdapter`
  (`src/mcp.rs` → `Tool`) с именем `server__tool`.

```toml
[mcp]
servers_file = "~/.arch-harness/mcp.json"
connect_on_start = true    # модель увидит MCP-инструменты; старт TUI медленнее
timeout_secs = 60
```

Компромисс: `true` — MCP в агентном цикле ценой задержки старта;
`false` — мгновенный старт, MCP только по явному `/mcp list`/`arch mcp call`.

## Свой MCP-сервер

Сервер — любой процесс, говорящий JSON-RPC 2.0 строками по stdio:

1. На `initialize` ответить возможностями (`tools` — если есть инструменты);
   принять `notifications/initialized`.
2. На `tools/list` вернуть список `{name, description, inputSchema}`.
3. На `tools/call` — выполнить и вернуть `content: [{type: "text", text: ...}]`
   (клиент разбирает text-части).

Спецификация: <https://modelcontextprotocol.io> (transport «stdio»).
Готовые серверы — пакеты `@modelcontextprotocol/server-*` (npx) и
`mcp-server-*` (uvx/pip). Проверка без TUI:

```bash
arch mcp list
arch mcp call myserver__mytool '{"arg": 1}'
```

Замечания:

- Файл отсутствует — ошибка с подсказкой создать `~/.arch-harness/mcp.json`
  по образцу `examples/mcp.example.json`.
- Таймауты сетевых MCP-серверов регулируйте `timeout_secs`; тяжёлые
  `npx -y`-первые-запуски могут не уложиться в 60 с — поднимите до 120–180.
- Ключи/токены серверам передавайте через `env` в `mcp.json`; сам файл
  не коммитьте наружу, если в нём секреты.
