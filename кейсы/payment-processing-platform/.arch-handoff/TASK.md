# Задача для кодового харнесса

Walking Skeleton (H0): реализовать сквозной сценарий внутреннего перевода (intra-bank transfer) от счёта A к счёту B, доказывающий архитектуру до массовой разработки.

Сквозной путь:
1. API Gateway: принять POST /payments с заголовком Idempotency-Key, mTLS, rate limit (AD-22)
2. Payment Gateway: проверка идемпотентности (Redis, AD-5 уровень 1), валидация схемы, генерация payment_id
3. Payment Orchestrator: создание саги (AD-1), state machine (AD-4, ADR-006): INITIATED → VALIDATED → FRAUD_PASSED → AML_PASSED → AUTHORIZED → RAIL_SENT → RAIL_CONFIRMED → LEDGER_POSTED → COMPLETED
4. Authorization: проверка баланса (mock Core Banking), установка холда
5. Fee Engine (AD-18): расчёт комиссии (mock тариф — 0% для internal)
6. Ledger: двойная проводка (Debit A / Credit B) в PostgreSQL (AD-3)
7. Outbox: запись события PaymentCompleted в outbox-таблицу в той же транзакции (AD-2); CDC-ретрансляция в Kafka (или polling publisher как fallback для skeleton)
8. Notification: идемпотентный consumer события (AD-5 уровень 2 — inbox-таблица), mock SMS
9. Повторный POST с тем же Idempotency-Key → возврат того же payment_id и результата

Технологии: Go, PostgreSQL, Redis, Kafka, Kubernetes (minikube для локальной разработки).

Инварианты (27 AD в ARCHITECTURE-SPINE.md):
- AD-1: saga orchestration (не хореография), персистентное состояние
- AD-2: transactional outbox (запись + событие в одной транзакции)
- AD-3: database-per-service (PostgreSQL, отдельная БД per context)
- AD-4: strong consistency, single-writer per payment_id, optimistic locking
- AD-5: 3-уровневая идемпотентность (Redis + inbox + state machine)
- AD-7: append-only audit log (payment_events, без UPDATE/DELETE)
- AD-8: circuit breaker на mock Core Banking
- AD-9: DLQ-топик для каждого consumer'а
- AD-10: ретраи в одном слое, backoff+jitter
- AD-17: queue load leveling (команда через Kafka-топик payments.commands)
- AD-21: data minimization (минимум ПДн в событиях)

Критерии приёмки:
- Повторный запрос с тем же Idempotency-Key возвращает тот же payment_id и статус
- Баланс изменён ровно один раз (проверка: SELECT balance до и после повторного запроса)
- Событие PaymentCompleted получено consumer'ом ровно один раз (проверка: inbox-таблица processed_messages)
- Audit log (payment_events) содержит все переходы статусов в порядке state machine
- Недопустимый переход статуса → 409 Conflict
- Все 8 состояний пройдены для успешного платежа
- Poison message (невалидный payment_id) → DLQ после 3 попыток
- Сборка: go build ./... — OK
- Тесты: go test ./... -count=1 — OK

План отката: skeleton не затрагивает продакшн. Откат = удаление namespace в Kubernetes.

## Контракт результата

Финальный ответ обязан завершаться JSON-объектом (после него — ни символа):

```json
{"status": "complete|partial|blocked", "assumptions": [], "open_questions": [], "conflicts_with_prior_decisions": []}
```

- `status`: `complete` — выполнено полностью; `partial` — частично; `blocked` — заблокировано.
- `assumptions`: допущения, принятые при реализации.
- `open_questions`: вопросы к архитектору.
- `conflicts_with_prior_decisions`: расхождения с принятыми ранее решениями (ADR, spine).

Архитектурный контекст — `ARCHITECTURE.md`, ограничения — `CONSTRAINTS.yaml`, рубрика приёмки — `RUBRIC.yaml` (при наличии).
