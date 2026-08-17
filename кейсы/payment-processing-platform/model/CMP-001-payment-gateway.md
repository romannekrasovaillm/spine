---
id: CMP-001
type: cmp
title: "Payment Gateway"
status: "designed"
depends_on: [CMP-002]
implements: [CAP-001, REQ-002, AD-5]
---

Приём запросов, идемпотентность, валидация схемы. Зона DMZ→App, БД pgw_db.
