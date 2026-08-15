# MCP-интеграции

MCP-клиент харнесса (`src/mcp.rs`): подключение внешних MCP-серверов,
их инструменты доступны агенту и из CLI. Формат конфигурации — как у
Claude Code.

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
