# Архитектурный контроль

Детерминированный механический слой харнесса (`src/control.rs`): маршрутизация
изменений по значимости, линтер architecture-spine, сенсоры спецификаций,
fitness functions, генератор ADR. Идеи — `docs/SOURCE_BRIEF.md` §C.3
(триггеры) и §A (AI-Disrupt PDLC, AWS AI-DLC).

## Architecture Significance Score

15 канонических триггеров (`SIGNIFICANCE_TRIGGERS`) — ответы «да/нет» на
вопросы об изменении:

| Триггер | Срабатывает, если изменение… |
|---|---|
| `new_component` | вводит новый компонент/сервис в ландшафт |
| `new_datastore` | вводит новое хранилище данных (СУБД, брокер, объектное) |
| `new_vendor` | добавляет нового вендора/внешнюю зависимость |
| `domain_ownership_change` | меняет владельца домена или границы ответственности |
| `cross_domain_integration` | создаёт интеграцию между доменами |
| `api_contract_change` | меняет существующий API-контракт |
| `data_contract_change` | меняет контракт/схему данных (события, таблицы, CDC) |
| `security_boundary_change` | пересекает или сдвигает границу безопасности (аутентификация, авторизация, шифрование) |
| `trust_zone_change` | меняет зоны доверия (контур КИИ, PCI, DMZ) |
| `consistency_model_change` | меняет модель согласованности (strong → eventual и т.п.) |
| `significant_nfr` | существенно меняет NFR: доступность, latency, пропускную способность |
| `rto_rpo_targets` | задаёт или пересматривает целевые RTO/RPO |
| `irreversible_migration` | необратимая миграция (данные, протоколы без пути отката) |
| `financial_impact` | прямое влияние на деньги: проводки, тарифы, лимиты, штрафы |
| `criticality_or_exception` | затрагивает критичный процесс либо просит architecture exception |

### Маршруты (`significance_score`)

Score — число сработавших триггеров:

| Маршрут | Условие | Режим |
|---|---|---|
| `Fast` | 0–1 триггер | Дельта-спека + авто-валидация. |
| `Standard` | 2–4 триггера | Контракт Spec→Plan→Tasks + Architecture Fit автоматически. |
| `Critical` | ≥5 **или** любой из критических: `security_boundary_change`, `irreversible_migration`, `criticality_or_exception` | Solutioning + human decision (гейт A3). |

```bash
arch control score --trigger new_component=true --trigger trust_zone_change=true
# Score: 2 (new_component, trust_zone_change триггеров) → маршрут Standard
```

В TUI: `/score new_component=true ...` (без аргументов — справка по триггерам).

## Линтер spine (`control spine`)

`ARCHITECTURE-SPINE.md` — позвоночник инвариантов: блоки `AD-<n>` с полями
`Binds:` (кого/что связывает), `Prevents:` (какое расхождение предотвращает),
`Rule:` (машинно-проверяемое правило). Определение блока — заголовок
`### AD-<n> …` или строка `AD-<n>:`/`AD-<n>.`; прочие вхождения — ссылки.
Пример — `examples/specs/ARCHITECTURE-SPINE.example.md`.

| Правило | Severity | Что ловит |
|---|---|---|
| `dup_ad_id` | error | Повторное определение того же AD (со ссылкой на первую строку). |
| `empty_field` | error | У AD-блока отсутствует или пусто поле `Binds`/`Prevents`/`Rule`. |
| `stub_marker` | warn | Заглушки `TODO`, `TBD`, `FIXME`, `XXX`, `???` — заполнить до гейта. |
| `unpinned_version` | warn | Непиннутая версия: `latest`, `*`/`"*"` в зависимости. |
| `broken_ad_ref` | warn | Ссылка на AD, не определённый в файле. |

```bash
arch control spine examples/specs/ARCHITECTURE-SPINE.example.md
# spine: нарушений нет
```

## Сенсоры спецификаций (`control sensors`)

Прогон по каталогу спек (нерекурсивно, `*.md`), на каждый файл — два сенсора:

- `required_sections` — наличие обязательных заголовков: `## Проблема`,
  `## Критерии приёмки`, `## Риски` (`REQUIRED_SECTIONS`);
- `upstream_coverage` — все относительные md-ссылки `[..](path.md)`
  существуют относительно каталога (внешние URL, `mailto:`, якоря `#`
  пропускаются) — спека обязана ссылаться на свои входы живыми ссылками.

```bash
arch control sensors examples/specs
#   [FAIL] required_sections examples/specs/ARCHITECTURE-SPINE.example.md — нет секций: ## Проблема, ## Критерии приёмки, ## Риски
#   [PASS] upstream_coverage examples/specs/ARCHITECTURE-SPINE.example.md — все ссылки валидны (0)
#   [PASS] required_sections examples/specs/SPEC.example.md — все обязательные секции на месте
#   [PASS] upstream_coverage examples/specs/SPEC.example.md — все ссылки валидны (0)
```

(Spine-файл закономерно падает по `required_sections` — он не спека;
сенсоры применяйте к каталогу функциональных спецификаций.)

## Fitness functions (`control check`)

Машинно-проверяемые утверждения о репозитории из `CONSTRAINTS.yaml`
(по умолчанию `<repo>/.arch-handoff/CONSTRAINTS.yaml`, `--constraints` —
другой файл). Итог PASS, если нет находок с severity `error`; **при FAIL —
exit code 1** (годится для CI). Обход пропускает `.git` и `target`;
не-UTF8 файлы читаются с потерями.

Схема правила (поля парсера — `src/control.rs::FitnessRule`):

```yaml
rules:
  - name: no-dbg-macro          # имя → код находки (обязательно)
    type: must_not_contain      # тип проверки (обязательно)
    glob: "src/**"              # набор файлов (content-правила; дефолт **/*)
    pattern: 'dbg!'             # regex (content-правила)
    severity: error             # error | warn (дефолт error)
  - name: cargo-check-passes
    type: command_succeeds
    command: 'cargo check'
    timeout_secs: 120           # дефолт 60
```

Четыре типа правил:

| `type` | Семантика | Обязательные поля |
|---|---|---|
| `must_contain` | `pattern` обязан найтись хотя бы в одном файле по `glob` | `glob`, `pattern` |
| `must_not_contain` | `pattern` не должен встречаться; находка на каждое вхождение (файл:строка + сниппет 120 символов) | `glob`, `pattern` |
| `file_exists` | Файл/каталог существует относительно корня репо | `path` |
| `command_succeeds` | `bash -c <command>` в корне репо завершается кодом 0 до `timeout_secs` (по таймауту процесс убивается) | `command` |

Примеры:

```yaml
rules:
  - name: no-direct-db-from-api
    type: must_not_contain
    glob: "services/api/**/*.rs"
    pattern: "sqlx::|diesel::"
    severity: error
  - name: spec-has-acceptance
    type: must_contain
    glob: "docs/specs/*.md"
    pattern: "^## Критерии приёмки$"
    severity: error
  - name: spine-present
    type: file_exists
    path: "docs/ARCHITECTURE-SPINE.md"
    severity: warn
  - name: unit-tests-pass
    type: command_succeeds
    command: "cargo test --quiet"
    timeout_secs: 900
    severity: error
```

Glob — простой: `**` — любая глубина (включая ноль сегментов), `*` — внутри
сегмента, `?` — один символ. Больше примеров — `examples/CONSTRAINTS.example.yaml`
(внимание: в нём поля названы `id`/`kind`; парсер принимает `name`/`type` —
именно они показаны выше).

```bash
arch control check ~/work/payment-svc
# Правил: 4, нарушений: 0 (error: 0, warn: 0)
# Итог: PASS
```

## ADR-шаблон (`control adr`)

```bash
arch control adr "Оркестрация платежа — отдельный сервис" --dir docs/adr
# ADR создан: docs/adr/ADR-003--------.md
```

Номер — `max(существующие ADR-NNN-*) + 1`; каталог создаётся при отсутствии.
Имя файла — `ADR-NNN-kebab-title.md` (не-ASCII, включая кириллицу, → `-`,
без транслитерации). Шаблон AI-DLC с placeholder-комментариями:

```
# ADR-003. <title>
- Date: <дата>
- Status: Proposed
## Context          — силы, ограничения, цена бездействия
## Decision         — одно явно сформулированное решение
## Alternatives Considered   — таблица «вариант | плюсы | минусы»
## Consequences     — ### Positive / ### Negative (обязательно!)
## Reversibility    — reversible | costly | irreversible + обоснование
## References       — ссылки на spine (AD-n), спеки, обсуждения
```

В TUI: `/adr new <title>` (в `docs/adr` рабочего каталога). Качество
заполненного ADR оценивается рубрикой `adr_quality`
(`docs/rubrics_and_benchmarks.md`).
