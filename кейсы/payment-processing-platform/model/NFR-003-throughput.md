---
id: NFR-003
type: nfr
title: "Пропускная способность"
status: "accepted"
verification: "Counter: payments_processed_total"
affects: [CMP-001, CMP-002]
rps_target: 5000
currency: RUB
---

Цель: 5000 TPS peak. Ёмкость компонентов: `arch nfr capacity`; стоимость контура: `arch nfr cost`.
