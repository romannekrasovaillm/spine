# Spine

**Spine** — доменный харнесс solution-архитектора (банковский корпоративный
контур): тонкий агентный харнесс с TUI, библиотекой архитектурных скиллов
и контуром архитектурного контроля. Rust, edition 2024, один бинарь `arch`:
TUI, CLI, библиотека. *English description: [README_EN.md](README_EN.md).*

> [!WARNING]
> **Исследовательский проект.** Харнесс создан для изучения особенностей
> доменных агентных харнессов (архитектурные инструменты, рубрики, губернанс,
> плагины-скиллы, политики автономии). **Не предназначен для применения в
> промышленной среде** — только для исследования и экспериментов.

<p align="center">
  <img src="docs/screenshots/02-chat-mermaid.png" alt="arch: диалог с агентом и живой рендер mermaid-диаграммы в Unicode-арт" width="88%">
</p>

<p align="center">
  <img src="docs/screenshots/01-splash.png" alt="Заставка Tokyo Night" width="32%">
  <img src="docs/screenshots/03-model-picker.png" alt="Пикер моделей (/model)" width="32%">
  <img src="docs/screenshots/04-rubric.png" alt="Якорная рубрика с LLM-судьёй" width="32%">
</p>

<p align="center">
  <sub>Кадры — снимки реальных экранов TUI (рендерятся из кода тестом
  <code>gen_readme_screenshots</code>, не макеты): заставка Tokyo Night ·
  пикер моделей с ризонинг-переключателем · оценка ADR по якорной рубрике.</sub>
</p>

Харнесс намеренно **тонкий**: ядерные инструменты (bash, файлы) плюс
специализированные инструменты архитектора. Тяжёлая часть — не код, а
дисциплина артефактов: спецификации, architecture-spine, ADR, рубрики,
fitness functions, handoff-пакеты кодовым агентам.

Идеи — разбор SDD-харнессов и корпоративных агентных фреймворков
(`docs/SOURCE_BRIEF.md`, август 2026: AI-Disrupt PDLC, AWS AI-DLC/Kiro,
BMAD, Spec Kit, OpenSpec и др.):

- **Маршрутизация изменений Fast/Standard/Critical** по Architecture
  Significance Score — 15 триггеров значимости (`src/control.rs`).
- **Architecture-spine**: позвоночник инвариантов с полями
  `Binds` / `Prevents` / `Rule` — фиксируется только то, в чём независимые
  исполнители могут разойтись несовместимо.
- **ADR-дисциплина**: решение записывается до реализации, с альтернативами,
  отрицательными последствиями и оценкой обратимости.
- **Evidence-подход**: оценка LLM-судьёй только со свидетельствами-цитатами;
  fitness functions — машинно-проверяемые утверждения о репозитории;
  headless JSON-контракт результата у кодовых харнессов и cron-задач.

## Пример: mermaid → ASCII в терминале

`arch mermaid examples/mermaid/flow.mmd`:

```
       ┌──────────────────────────────┐
       │ Каналы: мобильный банк и веб │
       └──────────────────────────────┘
                      ┌┘
                      ▼ REST JSON
               ┌─────────────┐
               │ API Gateway │
               └─────────────┘
                      │
                      ▼ маршрутизация и аутентификация
          ┌───────────────────────┐
          │ Платёжный оркестратор │
          └───────────────────────┘
             ├────────┴статус-колбэк─┐
             ▼ команда списания      ▼ ISO 20022 платёжное поручение
┌────────────────────────┐   ┌───────────────┐
│ Ядро: счета и проводки │   │ Платёжный хаб │
└────────────────────────┘   └───────────────┘
             ┌авторизация─и─токены───┤
             ▼                       ▼ событие PaymentStatus
┌────────────────────────┐   ┌──────────────┐
│ Внешний платёжный шлюз │   │ Шина событий │
└────────────────────────┘   └──────────────┘
                       ┌─────────────┘
                       ▼ подписка на статусы
            ┌────────────────────┐
            │ Сервис уведомлений │
            └────────────────────┘
```

## Возможности

### Библиотека скиллов и плагины (agent-plugins.org)

- Пять встроенных плагинов (`arch init` раскладывает в `~/.arch-harness/plugins`):
  **arch-core** (13 методических скиллов архитектора, вкл. мета-скилл `skill-authoring`),
  **patterns-integration** (сага, outbox, CQRS, strangler+ACL, идемпотентность — дистилляты
  microservices.io), **patterns-resilience** (circuit breaker+retry, bulkhead, load leveling,
  cache-aside, throttling — дистилляты Azure Well-Architected), **aws-builders-library**
  (9 дистиллятов Amazon Builders' Library: таймауты/джиттер, отказ от fallback, load shedding,
  статическая стабильность, лидер-выборы, бэклоги, fairness) и **aws-agentic-ai**
  (агентные AI-паттерны AWS PG 2026: 10 паттернов агентов, LLM-workflows, сага-оркестрация,
  multi-agent, reflect-refine).
- Подключены внешние библиотеки плагинов (в конфиге `[plugins] dirs`): скиллы + MCP
  в одном пакете; поиск `arch skills search`, показ `arch skills show`; в TUI —
  `/skills`, `/skill` (в контекст сессии), `/plugins`; модель вызывает инструменты
  `skill_search`/`skill_load` сама.
- Сессии: `/sessions`, `/resume last` — восстановление диалога из append-only журнала.

Подробности: `docs/plugins_and_skills.md`.

### AGENTS.md для команд репозиториев

- `arch agents-md refresh <repo>` — компилирует AGENTS.md из spine-инвариантов,
  CONSTRAINTS.yaml и манифестов; рукописная зона команды не затирается.
- `arch agents-md lint <repo>` / `lint-all` — дрейф-контроль по хэшу входов
  (CI/крон по флоту репозиториев); рубрика `agents_md_quality`; скилл `agents-md-authoring`.

Подробности: `docs/agents_md.md`.

### Губернанс: автономия, доказательства, метрики, дельты

- **R-уровни автономности** (`[policy] autonomy = "R2"`): каждый вызов инструмента
  классифицируется по риску — `rm -rf` получает DENY на уровне R2, журнал фиксирует
  попытки (AI-Disrupt PDLC). `arch policy --check "<cmd>"`.
- **Evidence Bundle** — аудиторский след как гейт выпуска: `arch evidence pack/verify`
  с профилями Fast/Standard/Critical и детекцией подмены артефактов по хэшам.
- **Метрики**: `arch metrics` — сессии, инструменты, ошибки, токены/₽, баллы рубрик,
  pass rate бенчей — из локальных журналов и отчётов.
- **Дельта-спеки**: `arch delta new|validate|archive` — state machine OpenSpec
  (propose → apply → archive) для brownfield-потока.

Подробности: `docs/governance.md`.



- **TUI** (ratatui, Tokyo Night): чат с агентом со стримингом, скруглённые
  рамки, градиентный баннер сплэша (cyan→purple), иконки вкладок, бейдж модели,
  брайлевский спиннер, прокрутка колесом мыши и PgUp/PgDn, вкладки
  Mermaid/Рубрика/Знания; список команд — через `/help` и автодополнение.
  Markdown в ответах: заголовки, буллеты, таблицы, цитаты, мягкие код-панели.
  Mermaid-арт не переносится по ширине (клип вместо разрыва box-линий).
  Полноэкранный просмотр вкладки — `F4`: вертикальная прокрутка и
  горизонтальная панорама широких диаграмм (`←/→`, Shift+колесо), `Esc` —
  назад; основной экран не перестраивается.
  Экспорт экрана в Word/Excel — `/export word|excel` (и `arch export` для
  журналов сессий без TUI).
- **Агентный цикл** с function calling: лимит итераций, append-only JSONL-журнал
  сессии, промышленная закалка по опыту Theseus/тройки лидеров
  (`docs/theseus_hardening.md`):
  трёхуровневая компактификация (маскирование → прунинг → LLM-саммари L3,
  on-error compact&resubmit при HTTP 413), retry-матрица по классам ошибок
  (429/5xx/транспорт с джиттером; 4xx/авторизация — без повторов),
  детекторы циклов (doom-loop по fingerprint вызовов, exploration spiral,
  doom-text), редакция секретов в выводе инструментов и журнале,
  хуки жизненного цикла (`[hooks]`, exit 2 = блок), fuzzy-каскад правок
  `edit_file` (точный → терпимый к пробелам/отступам).
- **Диагностика окружения** `arch doctor` (и `/doctor` в TUI): ключи, каталоги,
  плагины, кодовые харнессы, MCP, крон — 10 проверок с ✓/⚠/✗.
- **Mermaid-рендер** в Unicode/ASCII (flowchart + sequenceDiagram) — диаграммы
  C4 прямо в терминале и в отчётах (`src/mermaid.rs`).
- **Рубрики архитектурного контроля**: 5 якорных YAML + динамическая генерация
  под предмет; LLM-судья с обязательными цитатами-свидетельствами
  (`docs/rubrics_and_benchmarks.md`).
- **Бенчмарки solution architecture**: 3 банковских сценария с проходным
  порогом и отчётами (`docs/rubrics_and_benchmarks.md`).
- **Архитектурный контроль**: score значимости, линтер spine, сенсоры спек,
  fitness functions, генератор ADR (`docs/control.md`).
- **Handoff кодовым харнессам**: Claude Code, Qwen Code, OpenClaw, Hermes,
  Theseus, CodeWhale — пакет `.arch-handoff/` с headless-контрактом
  (`docs/harness_integrations.md`).
- **MCP-клиент** (формат `mcp.json` как у Claude Code, ленивый режим)
  — `docs/mcp.md`.
- **Веб-доступ** (DuckDuckGo, 11 кураторских сайтов архитектора) и **локальная
  база знаний** (`docs/web_kb.md`).
- **Планировщик md-задач** «md + cron + LLM + баш-пайпы» (`docs/cron_and_md_pipes.md`).
- **Библиотека промптов**: architect, adr, spine, review_adversarial,
  readiness_gate, handoff_compile, reverse_discovery, nfr_design
  (`assets/prompts/`, команда `arch prompts`).

## Быстрый старт

```bash
cargo build --release          # бинарь: target/release/arch
ln -sf "$PWD/target/release/arch" ~/.local/bin/arch   # запуск одним словом `arch`
arch init                      # конфиг + ассеты в ~/.arch-harness и
                               # ~/.config/arch-harness/config.toml

# API-ключи — только через окружение (в конфиг пишутся лишь ИМЕНА переменных):
export DEEPSEEK_API_KEY=...    # deepseek (v4-flash, по умолчанию), deepseek-pro (v4-pro)
export MOONSHOT_API_KEY=...    # kimi (kimi-k3, 1M контекст)
export ZHIPU_API_KEY=...       # glm (glm-4.7, резерв)

arch tui                       # интерактивный TUI (или просто `arch`)
arch models                    # проверить настроенные модели
```

Смоук без LLM: `arch mermaid examples/mermaid/flow.mmd`,
`arch control score --trigger new_component=true`,
`arch control spine examples/specs/ARCHITECTURE-SPINE.example.md`.

## CLI

```
arch [--config <path>] <command>   # без команды — TUI
```

| Команда | Назначение |
|---|---|
| `tui` | Интерактивный TUI (действие по умолчанию) |
| `init` | Инициализация `~/.arch-harness`: конфиг, ассеты, примеры |
| `run [prompt] [--model] [--no-stream] [--quiet]` | Headless-прогон агента; `-` или пайп — читать stdin. `-q`: строгий контракт как `dsh --profile headless` — stdout только финальный ответ (для скриптов: `arch run -q "…" > answer.md`), stderr пуст при успехе, сбой → stderr + exit 1 |
| `models` | Список настроенных моделей |
| `prompts [name]` | Библиотека промптов: список или показ шаблона |
| `mermaid <file>` | Рендер mermaid-файла в Unicode/ASCII-арт (`-` — stdin) |
| `rubric list` / `rubric run <rubric> <target> [--model] [--dynamic-subject]` | Рубрики: список / оценка файла LLM-судьёй |
| `bench list` / `bench run <name> [--model]` | Архитектурные бенчмарки |
| `kb <query> [--limit]` | Поиск по локальной базе знаний |
| `web search <query> [--arch]` / `web fetch <url>` / `web sites` | Веб: поиск, фетч, кураторские сайты |
| `mcp list` / `mcp call <server__tool> [args-json]` | MCP-серверы и вызовы инструментов |
| `handoff <harness> --repo <path> --task <text> [--spec ...]` | Сформировать handoff-пакет `.arch-handoff/` |
| `harness-run <harness> --repo <path> [--task]` | Прогнать кодовый харнесс по пакету |
| `harnesses` | Список известных кодовых харнессов (и их доступность в PATH) |
| `control check <repo>` | Fitness-контроль по CONSTRAINTS.yaml (exit 1 при FAIL) |
| `control spine <file>` | Линтер ARCHITECTURE-SPINE.md |
| `control sensors <dir>` | Сенсоры спецификаций (required_sections, upstream_coverage) |
| `control score --trigger имя=true ...` | Architecture Significance Score → маршрут |
| `control adr <title> [--dir]` | Новый ADR по шаблону |
| `cron list` / `cron run <name>` / `cron tick` | Планировщик md-задач |
| `worktree new <name>` / `list` / `diff` / `accept` / `drop` | Worktree-фабрика: изоляция агентной работы, review/accept человеком |

## Документация

- `docs/plugins_and_skills.md` — плагины (скиллы+MCP+субагенты+хуки), библиотека скиллов, восстановление сессий

- `docs/architecture.md` — устройство, контракты, как расширять.
- `docs/slash_commands.md` — справочник слэш-команд TUI.
- `docs/tools.md` — справочник инструментов агента (параметры, примеры, лимиты).
- `docs/models.md` — подключение LLM (DeepSeek/Kimi/GLM, свои endpoint'ы).
- `docs/rubrics_and_benchmarks.md` — движок рубрик и бенчмарков.
- `docs/harness_integrations.md` — handoff-пакеты и кодовые харнессы.
- `docs/control.md` — архитектурный контроль: score, spine, сенсоры, fitness, ADR.
- `docs/mcp.md` — MCP-интеграции.
- `docs/cron_and_md_pipes.md` — планировщик и баш-пайпы.
- `docs/web_kb.md` — веб-доступ и локальная база знаний.
- `docs/SOURCE_BRIEF.md` — источник идей (разбор фреймворков).
- `docs/RUST_CONVENTIONS.md` — конвенции кода.

Конфигурация: `config.example.toml`, `cron.example.toml` — полные
прокомментированные образцы. Тесты: `cargo test` (live-тесты LLM — `#[ignore]`).
