# Handoff: передача контекста кодовому харнессу · Handing context to a coding harness

**🇷🇺 Русский** | [🇬🇧 English below](#english)

Пошаговый разбор того, как архитектор передаёт задачу на исполнение внешнему
кодовому харнессу (Claude Code, Qwen Code, OpenClaw, Hermes, Theseus,
CodeWhale) — без потери архитектурного контекста и без ручного копирования
спек в чужой чат.

<p align="center">
  <img src="screenshots/05-handoff.png" alt="Handoff: handoff_create собирает пакет .arch-handoff/, harness_run прогоняет Claude Code, контракт результата в сводке, control_check подтверждает целостность" width="92%">
</p>

## Поток по кадру

1. **Архитектор формулирует границу работ** в диалоге: «передай статусную
   машину на исполнение Claude Code и проконтролируй результат». К этому
   моменту в сессии уже есть спайн (AD-инварианты), ADR и mermaid-модель.
2. **`handoff_create`** собирает пакет `<repo>/.arch-handoff/`:
   - `TASK.md` — формулировка задачи для кодового харнесса;
   - `ARCHITECTURE.md` — **epic-context** (~800–1500 токенов, окно якорной
     рубрики `handoff_quality`): спайн, затронутые ADR, границы «что нельзя
     трогать»;
   - `CONSTRAINTS.yaml` — fitness-правила под стек репозитория (заготовка,
     переписывается под spine AD-n перед передачей);
   - `RUBRIC.yaml` — якорная рубрика приёмки;
   - `MANIFEST.json` — дата, модель, источники; `adr/` — копии ADR.
3. **`harness_run`** прогоняет пакет адаптером `[harnesses.claude-code]`
   (`claude -p --dangerously-skip-permissions`, задача — в stdin).
   **Умные таймауты**: абсолютный потолок 30 мин + таймаут тишины 10 мин
   (нет вывода и нет изменений файлов репо → завис); молча работающий
   харнесс не трогаем — heartbeat идёт по mtime репозитория. При прерывании
   убивается вся процессная группа (сирот не остаётся), частичный вывод
   возвращается с рекомендацией `git status`. Значения `timeout_secs` ниже
   600 поднимаются автоматически — ранний обрыв оставлял репозиторий
   полусобранным.
4. **Контракт результата**: кодовый харнесс обязан завершить работу
   fenced-блоком ` ```json ` с полем `status` (`complete | partial |
   blocked`) и списками `assumptions`, `open_questions`,
   `conflicts_with_prior_decisions` — харнесс извлекает его в сводку
   (`status=complete · assumptions 3 · open_questions 1`). Отсутствие
   контракта помечается предупреждением: ответ мог быть неполным.
5. **Архитектурный контроль после прогона**: `control_check` гоняет
   fitness-правила пакета против репозитория (`pytest -q` PASS,
   `no-print-in-py` PASS) и проверяет, что spine-инварианты AD-1…AD-3 не
   нарушены. Открытые вопросы архитектор фиксирует в ADR (на кадре —
   ADR-004: таймаут подтверждения НСПК).
6. **Протокол для архкома** — `/export docx` выгружает ход сессии
   (со скриншотами рубрик и контрактом) в Word.

## Почему не bash и не «покажи TASK.md в чат»

- **Квотинг**: длинный `TASK.md` с кавычками/бэктиками ломает команду —
  адаптер передаёт задачу через stdin/argv, а не интерполяцию в shell.
- **Env-scrub**: bash-инструмент прячет `*_KEY`/`*_TOKEN` от команд —
  харнесс, авторизующийся переменной окружения, останется без креденшелов;
  `harness_run` пробрасывает env адаптера как настроено.
- **Таймауты**: у bash-инструмента потолок ~600 с, у прогона харнесса —
  30 мин абсолютных + таймаут тишины; агентный цикл применяет per-tool
  таймаут (у `harness_run` — до 7200 с + запас).
- **Сироты**: bash-обрыв убивал только обёртку, дочерние процессы харнесса
  продолжали работать и конфликтовали с повторным запуском.

CLI-эквивалент вне диалога: `arch handoff --repo <path> --task "…"` и
`arch harness-run <harness> --repo <path> [--task "…"]` — см.
`docs/harness_integrations.md`.

---

<a id="english"></a>

## 🇬🇧 The flow, frame by frame

1. **The architect states the scope** in chat ("hand the state machine over
   to Claude Code and supervise the result"). The session already holds the
   spine (AD invariants), ADRs and a mermaid model.
2. **`handoff_create`** assembles `<repo>/.arch-handoff/`: `TASK.md`
   (the task), `ARCHITECTURE.md` (epic context, ~800–1500 tokens — the
   anchor rubric window), `CONSTRAINTS.yaml` (stack-detected fitness rules,
   a scaffold to be rewritten against the spine), `RUBRIC.yaml` (acceptance
   rubric), `MANIFEST.json` + `adr/` copies.
3. **`harness_run`** executes the package through the configured adapter
   (e.g. `claude -p --dangerously-skip-permissions`, task via stdin).
   **Smart timeouts**: a 30-min absolute ceiling plus a 10-min silence
   timeout (no output and no repo file changes → hung); a quiet but working
   harness is kept alive by the repo mtime heartbeat. On abort the whole
   process group is killed (no orphans) and partial output is returned with
   a `git status` recommendation. `timeout_secs` below 600 is raised
   automatically — early aborts used to leave repos half-built.
4. **Result contract**: the coding harness must finish with a fenced
   ` ```json ` block carrying `status` (`complete | partial | blocked`),
   `assumptions`, `open_questions`, `conflicts_with_prior_decisions`; the
   summary line extracts it, and a missing contract is flagged.
5. **Post-run architectural control**: `control_check` replays the fitness
   rules against the repository and verifies the spine invariants
   (AD-1…AD-3 intact). Open questions are captured as ADRs.
6. **Evidence for the review board** — `/export docx` writes the session
   transcript to Word.

CLI equivalents: `arch handoff --repo <path> --task "…"` and
`arch harness-run <harness> --repo <path>` — see
`docs/harness_integrations.md`.
