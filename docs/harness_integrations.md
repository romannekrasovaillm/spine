# Интеграция кодовых харнессов

Передача архитектуры в код: arch-harness готовит **handoff-пакет** для
кодового агента (Claude Code, Qwen Code, OpenClaw, Hermes, Theseus,
CodeWhale), прогоняет его и контролирует результат. Реализация —
`src/harness.rs`; идеи — BMAD epic-context и headless-контракты
(`docs/SOURCE_BRIEF.md` §A.3).

## Handoff-пакет `.arch-handoff/`

`arch handoff` создаёт в корне репозитория каталог `.arch-handoff/`:

| Файл | Содержимое | При повторной генерации |
|---|---|---|
| `TASK.md` | Задача + контракт результата (ниже). | Перезаписывается. |
| `ARCHITECTURE.md` | **Epic-context** — дистиллят спек/спайна. | Перезаписывается. |
| `CONSTRAINTS.yaml` | Fitness-правила для `arch control check`. | Не затирается (пишется дефолт при отсутствии). |
| `RUBRIC.yaml` | Рубрика приёмки (копия якорной `handoff_quality`). | Не затирается. |
| `adr/` | Копии переданных ADR-файлов (`--spec`, признак: `ADR` в имени или `/adr/` в пути). | Существующие копии не затираются. |
| `MANIFEST.json` | Мета: `created_at` (UTC), `task`, `model` (default_model), `sources`, `epic_context_chars`, `epic_context_tokens`. | Перезаписывается. |

### TASK.md и headless JSON-контракт

Финальный ответ кодового харнесса обязан завершаться JSON-объектом
(после него — ни символа):

```json
{"status": "complete|partial|blocked", "assumptions": [], "open_questions": [], "conflicts_with_prior_decisions": []}
```

- `status`: `complete` — выполнено полностью; `partial` — частично;
  `blocked` — заблокировано.
- `assumptions` — допущения, принятые при реализации.
- `open_questions` — вопросы к архитектору.
- `conflicts_with_prior_decisions` — расхождения с принятыми решениями
  (ADR, spine); конфликт со spine обязан останавливать работу, а не
  «решаться на месте».

### ARCHITECTURE.md (epic-context, 800–1500 токенов)

Компиляция из `--spec`-файлов (`compile_epic_context`): секции с полями
`Binds:`/`Prevents:`/`Rule:` (ADR-блоки spine) включаются **целиком**,
прочие секции — заголовок + первые абзацы. Глубина адаптивная: старт — 2
абзаца на секцию, но если итог недобирает до ~800 токенов (низ окна рубрики
handoff_quality), спеки перерендериваются глубже (до 8 абзацев) — сценарий
«реализация без доступа к источникам» требует массы. Сверху итог усечён до
6000 символов (≈1500 токенов при оценке 4 символа/токен) с пометкой об
усечении. `handoff_create` предупреждает в выводе, если epic-context всё же
ниже окна (мало источников — добавьте `--spec`). Смысл: агент реализует без
доступа к исходным документам и без архитектурных изобретений.

## Цикл «передача → прогон → контроль»

```bash
# 1. Передача: генерация пакета (спеки/спайн/ADR — через --spec)
arch handoff claude-code --repo ~/work/payment-svc \
  --task "Реализуй платёжный оркестратор по ADR-001 и ADR-002" \
  --spec docs/ARCHITECTURE-SPINE.md --spec docs/adr/ADR-001-orchestrator.md

# 2. Прогон: бинарь харнесса с задачей (по умолчанию — текст .arch-handoff/TASK.md)
arch harness-run claude-code --repo ~/work/payment-svc

# 3. Контроль: fitness functions по CONSTRAINTS.yaml (exit 1 при FAIL)
arch control check ~/work/payment-svc

# 4. Приёмка пакета/результата рубрикой (LLM-судья)
arch rubric run handoff_quality ~/work/payment-svc/.arch-handoff/TASK.md
```

Известные харнессы — `arch harnesses` (показывает бинарь, режим промпта и
наличие в PATH). В TUI: `/handoff <harness> <repo> [task...]`,
`/control <repo>`.

## Настройка `[harnesses.*]`

```toml
[harnesses.claude-code]
binary = "claude"                 # имя бинаря в PATH
args = ["-p"]                     # дополнительные аргументы
prompt_mode = "stdin"             # positional | flag | stdin
timeout_secs = 1800               # абсолютный потолок прогона
idle_timeout_secs = 600           # таймаут тишины: 0 — выключить
# env = { FOO = "bar" }           # дополнительные переменные окружения
```

`prompt_mode` (`src/config.rs::PromptMode`, сборка argv — `build_argv`):

- `positional` — задача добавляется последним аргументом: `binary args... "задача"`;
- `flag` — в `args` плейсхолдер `{prompt}` заменяется задачей (без
  плейсхолдера задача добавляется последним аргументом);
- `stdin` — задача пишется в stdin (писатель — отдельной задачей, без
  дедлока на pipe-буфере), argv = `binary args...`.

Дефолтные адаптеры (`Config::default` / `config.example.toml`):

| Харнесс | binary | args | prompt_mode |
|---|---|---|---|
| claude-code | `claude` | `["-p", "--dangerously-skip-permissions"]` | stdin |
| qwen-code | `qwen` | `[]` | stdin |
| openclaw | `openclaw` | `["agent", "--message"]` | flag |
| hermes | `hermes` | `["-p"]` | stdin |
| theseus | `theseus` | `["run", "--task"]` | flag |
| codewhale | `codewhale` | `["-p"]` | stdin |

> **Claude Code headless:** `claude -p` без TTY на файловых операциях встаёт
> на permission-промпте и висит до таймаута — поэтому дефолтный адаптер несёт
> `--dangerously-skip-permissions`. Права процесса ограничены каталогом
> репозитория; для интерактивных прогонов флаг уберите в `config.toml`.

Прогон (`run_harness`): `cwd = repo`, env из конфига, захват stdout/stderr
(по 256 КБ хвоста каждый), код возврата, длительность и способ завершения
(`Termination`) в `HarnessRun`; бинарь не найден — ошибка с подсказкой
поправить `[harnesses.<name>]`.

**Умные таймауты.** Прогон прерывается по одному из двух триггеров:
абсолютный потолок `timeout_secs` или таймаут тишины `idle_timeout_secs`
(нет вывода в stdout/stderr И нет свежих изменений файлов репозитория —
процесс завис, например ждёт интерактивного ввода). Молчащий, но пишущий
код харнесс (heartbeat по mtime репо, скан ~4 раза за idle-окно) НЕ
считается зависшим. При прерывании убивается вся процессная группа
(TERM → 3 с → KILL по `-pgid`) — дочерние процессы харнесса не остаются
сиротами; частичный вывод и причина прерывания возвращаются в сводке с
рекомендацией проверить `git status` перед повторным запуском. Аргумент
`timeout_secs` инструмента `harness_run` ограничен снизу 600 с: модели
склонны занижать оценку, а ранний обрыв оставлял репозиторий полусобранным.

Из агентного цикла (TUI/headless) прогон идёт инструментом **`harness_run`**
(не bash!): задача читается из `.arch-handoff/TASK.md` или передаётся явным
аргументом `task`; сводка ответа включает код возврата, длительность и
JSON-контракт результата (`status`, счётчики `assumptions`/`open_questions`/
`conflicts`) — отсутствие контракта помечается предупреждением. Bash-запуск
харнесса — антипаттерн: квотинг длинного TASK.md ломает команду, таймаут
bash (≤600 с) короче харнессового (1800 с), а env-scrub bash-инструмента
прячет `*_KEY`/`*_TOKEN` от команды — харнесс, авторизующийся через
переменную окружения, останется без креденшелов.

## Пример сессии end-to-end

```bash
arch harnesses
#   claude-code    claude (Stdin)      /home/.../bin/claude
#   qwen-code      qwen (Stdin)        MISSING        ← бинаря нет: секция
#   ...                                             настраиваемая, прогон
#                                                   невозможен до установки
mkdir -p ~/work/payment-svc && cd ~/work/payment-svc && git init -q

arch handoff hermes --repo ~/work/payment-svc \
  --task "Скелет платёжного оркестратора: команды списания/возврата, идемпотентность по payment_id+operation_seq" \
  --spec docs/ARCHITECTURE-SPINE.md
# Handoff-пакет: ~/work/payment-svc/.arch-handoff
#   ... TASK.md, ARCHITECTURE.md, MANIFEST.json, CONSTRAINTS.yaml, RUBRIC.yaml
# epic-context ≈ 900 токенов

arch harness-run hermes --repo ~/work/payment-svc
# ── stdout (exit Some(0), 212.4s) ── ... {"status": "complete", ...}

arch control check ~/work/payment-svc
# Правил: 4, нарушений: 1 (error: 0, warn: 1) ... Итог: PASS
```

Замечания:

- `qwen-code` в дефолтной конфигурации есть, но бинарь `qwen` на машине
  отсутствует (`MISSING` в `arch harnesses`) — секция приведена как
  настраиваемый образец: установите Qwen Code или поправьте `binary`/`args`.
- Дефолтный `CONSTRAINTS.yaml` пакета зависит от стека репозитория (маркерные
  файлы): Rust (`cargo check`, `no-dbg-macro`…), Python (`pytest -q`,
  `no-print-in-py`…), Go (`go build`/`go vet`), Node (`npm test`), иначе —
  общий минимум (`readme-exists`). Это заготовка: перед передачей правила
  переписываются под spine-инварианты эпика (см. `assets/prompts/handoff_compile.md`);
  схема правил — в `docs/control.md`.
- Свой `CONSTRAINTS.yaml`/`RUBRIC.yaml` положите в `.arch-handoff/` до
  генерации или правьте после — повторная генерация их не затирает.
