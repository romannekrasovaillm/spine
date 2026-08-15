# Управление: автономия, доказательства, метрики, дельты

Четыре механизма губернанса из обзоров `_24_августа` (`docs/SOURCE_BRIEF.md`: AI-Disrupt PDLC,
OpenSpec), реализованные в харнессе детерминированным слоем.

## R-уровни автономности (policy)

Автономия калибруется **риском действия** (обратимость, blast radius), а не брендом модели.
Каждый вызов инструмента проходит проверку в `ToolRegistry::dispatch`:

| Класс риска | Примеры | R0–R1 | R2 (дефолт) | R4 | R5 |
|---|---|---|---|---|---|
| ReadOnly | read_file, grep, `cat`, `ls`, kb/web/skill-search | ALLOW | ALLOW | ALLOW | ALLOW |
| Mutating | write_file, edit_file, `cargo test`, `git commit` | confirm | ALLOW | ALLOW | ALLOW |
| Destructive | `rm -rf`, `git push --force`, `kubectl delete` | deny | **DENY** | confirm | ALLOW |

```toml
[policy]
autonomy = "R2"
```

- `arch policy` — текущий уровень; `arch policy --check "rm -rf /"` — класс риска и вердикт.
- Отказы журналируются в сессионном JSONL — это материал для детектора approval theater
  и аудита («кто и что пытался»).
- RequireConfirm в неинтерактивном режиме = отказ с текстом эскалации (модель корректно
  останавливается и просит человека — проверено живым прогоном).

## Evidence Bundle — условие выпуска

Аудиторский след собирается ДО релиза, а не после. Профиль полноты — по маршруту
значимости (`arch control score`):

- **Fast** (5): problem, spec_or_delta, risk_level, acceptance, rollback.
- **Standard** (+2): adr_or_pattern, validation, fitness_report.
- **Critical** (+4): spine, decision_a3 (choice/rationale/rejected/expiry),
  walking_skeleton, adversarial_review.

```bash
arch evidence pack <dir> --route critical   # EVIDENCE.yaml + хэши артефактов
arch evidence verify <dir>                  # полнота + целостность; exit 1 при FAIL
```

`verify` ловит подмену артефакта после упаковки (хэши не сошлись) — «спека,
дописанная задним числом» больше не проходит. Артефакты ищутся по каноническим
именам (SPEC.md, docs/adr/, DECISION.md, reports/fitness.md…).

## Метрики харнесса

`arch metrics` — из локальных журналов сессий (`sessions/*.jsonl`) и отчётов (`reports/`):

- сессии/сообщения/вызовы инструментов, доля ошибок инструментов (first-pass proxy);
- оценка токенов и стоимости;
- рубричные отчёты: число и средний взвешенный балл;
- бенчмарки: pass rate; крон-отчёты.

Рост доли ошибок инструментов или падение среднего балла рубрик — красные флаги
процесса (см. SOURCE_BRIEF «Красные флаги при внедрении»).

## Дельта-спеки (state machine OpenSpec)

Для brownfield-потока (Fast/Standard): изменение = дельта относительно живой истины.

```bash
arch delta new payment-timeout        # каркас changes/payment-timeout/DELTA.md
arch delta validate payment-timeout   # секции ADDED/MODIFIED/REMOVED, EARS, заглушки
arch delta list                       # предложенные vs архивные
arch delta archive payment-timeout    # после apply: валидация + перенос в archive/
```

Имена дельт не переиспользуются; архивация без валидации невозможна (error-находки
блокируют). Critical Path дельтой не закрывается — там полный Solutioning
(скилл `significance-routing`).
