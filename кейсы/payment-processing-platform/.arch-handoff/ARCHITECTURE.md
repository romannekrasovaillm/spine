# Архитектурный контекст (epic-context)

Собран: 2026-08-15T16:15:35.749046248+00:00

Источники:
- ARCHITECTURE.md
- ARCHITECTURE-SPINE.md
- CONSTRAINTS.yaml
- docs/adr/ADR-001-saga-orchestration.md
- docs/adr/ADR-002-transactional-outbox.md
- docs/adr/ADR-003-database-per-service-postgresql.md
- docs/adr/ADR-004-4.md
- docs/adr/ADR-005-adr.md
- docs/adr/ADR-006-adr.md
- docs/adr/ADR-007-dlq.md
- docs/adr/ADR-008-backoff-jitter.md
- docs/adr/ADR-009-reconciliation.md
- docs/adr/ADR-010-disaster-recovery-active-passive-rto-15min-rpo-0.md
- docs/adr/ADR-011-secrets-management-vault-hsm.md
- docs/adr/ADR-012-api-versioning-backward-compatibility.md
- docs/adr/ADR-013-canary-zero-downtime-db-migrations.md
- docs/adr/ADR-014-async.md
- docs/adr/ADR-015-data-lifecycle-152-vs-append-only-audit.md
- docs/adr/ADR-016-chaos-engineering-game-days.md

<!-- источник: ARCHITECTURE.md -->

# ARCHITECTURE.md — Платформа процессинга платежей

> Greenfield, Critical-маршрут (Significance Score: 13). Документ — целевая архитектура для handoff кодовым командам.

---

## 1. Бизнес-контекст

Платформа процессинга платежей банка — центральный узел, проводящий платежи через множество рельсов:

| Рельс | Тип | Latency-бюджет | Регулятор |
|-------|-----|----------------|-----------|
| Карты (MIR/Visa/MC) | Acquiring + Issuing | p99 < 2s | PCI DSS, NSPK |
| СБП (Cистема быстрых платежей) | C2C, C2B, B2C | p99 < 500ms | НСПК, 161-ФЗ |
| SWIFT | Cross-border | p99 < 30s | SWIFT GPI, 115-ФЗ |
| БЭСП (ЦБ РФ) | Large-value | p99 < 30s | ЦБ РФ |
| Внутренние переводы | Intra-bank | p99 < 1s | — |

## 2. Bounded Contexts (DDD)

| Контекст | Зона | Ответственность | БД |
|----------|------|----------------|-----|
| **Payment Gateway** | DMZ→App | Приём запросов, идемпотентность, валидация схемы | PG (pgw_db) |
| **Payment Orchestrator** | App | Saga-координация, state machine, маршрутизация на рельс | PG (orc_db) |
| **Authorization** | App | Лимиты, холды, проверка баланса, взаимодействие с ядром | PG (auth_db) + Redis |
| **Fraud Screening** | App | Правила, скоринг, блокировки | PG (fraud_db) + Redis |
| **AML / Sanctions** | App | Санкционный скрининг, ПФЛ/ФОТ, отчётность 115-ФЗ | PG (aml_db) |
| **Rail Connectors** | Int | Адаптеры к NSPK, CBR, SWIFT, Core Banking | PG (per-rail) |
| **Ledger** | Data | Двойная запись, проводки, гласный журнал | PG (ledger_db) |
| **Clearing & Settlement** | Data | Батч-клиринг, сверка, settlement files | PG (clearing_db) |
| **Notification** | App | SMS, push, email — идемпотентный consumer | PG (notif_db) |
| **Reporting** | Data | Отчёты ЦБ, операционные дашборды | PG read-replica |
| **Reconciliation** | Data | End-of-day сверка с рельсами, exception queue | PG (recon_db) |
| **Fee Engine** | App | Тарифные планы, расчёт комиссий, распределение | PG (fee_db) |
| **Dispute Service** | App | Chargeback flow (ISO 8583), арбитраж, сроки | PG (dispute_db) |
| **FX Service** | App | Курсы валют, конвертация, rate lock, spread | PG (fx_db) + Redis |
| **Feature Flags** | Infra | Конфигурация фич, canary, kill switch | Unleash (PG) |
| **Secrets (Vault)** | Infra | Dynamic credentials, key rotation, audit | Vault + HSM |

---

## 3. Архитектурные решения (краткая сводка)

| AD | Решение | ADR |
|----|---------|-----|
| AD-1 | Saga Orchestration (не хореография) | ADR-001 |
| AD-2 | Transactional Outbox + Debezium CDC | ADR-002 |
| AD-3 | Database-per-service (PostgreSQL) | ADR-003 |
| AD-4 | Сильная консистентность для фин. состояния (single-writer per payment_id) | AD-4 (spine) |
| AD-5 | 3-уровневая идемпотентность (Redis + inbox + state machine) | ADR-005 |
| AD-6 | 4 доверенные зоны (DMZ / App / Data / Integration) | ADR-004 |
| AD-7 | Append-only аудит-лог платежа (payment_events) | AD-7 (spine) |
| AD-8 | Circuit breaker на всех внешних вызовах | AD-8 (spine) |
| AD-9 | DLQ + стратегия ядовитых сообщений | ADR-007 |
| AD-10 | Ретраи: backoff+jitter, один слой | ADR-008 |
| AD-11 | Сверка (reconciliation) с рельсами | ADR-009 |
| AD-12 | Disaster Recovery (active-passive, RTO=15min, RPO=0) | ADR-010 |
| AD-13 | Secrets management (Vault + HSM) | ADR-011 |
| AD-14 | API versioning + backward compatibility | ADR-012 |
| AD-15 | Schema Registry для Kafka (Avro) | AD-15 (spine) |
| AD-16 | Canary/blue-green + zero-downtime DB migrations | ADR-013 |
| AD-17 | Queue Load Leveling для пиков | AD-17 (spine) |
| AD-18 | Fee/Commission engine | AD-18 (spine) |
| AD-19 | Dispute/Chargeback flow | AD-19 (spine) |
| AD-20 | Вебхук/колбэк для async рельсов | ADR-014 |
| AD-21 | Data lifecycle: 152-ФЗ vs append-only | ADR-015 |
| AD-22 | Rate limiting per channel/partner | AD-22 (spine) |
| AD-23 | Контрактное тестирование (Pact) | AD-23 (spine) |
| AD-24 | Feature flags | AD-24 (spine) |
| AD-25 | Chaos engineering (game days) | ADR-016 |
| AD-26 | Operator actions audit | AD-26 (spine) |
| AD-27 | Multi-currency / FX | AD-27 (spine) |

---

## 4. NFR-цели (измеримые)

| NFR | Цель | Метод измерения |
|-----|------|----------------|
| Latency (card auth) | p99 < 2s, p99.9 < 5s | Histogram: `payment_auth_duration_s

> **Контекст усечён** до 6000 символов; полные тексты — в файлах-источниках (см. MANIFEST.json).
