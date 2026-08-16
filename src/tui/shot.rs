//! Генератор README-скриншотов: настоящие кадры ratatui → SVG.
//!
//! Идея: TUI рендерится в `TestBackend` (без терминала), буфер ячеек
//! превращается в SVG с «оконным» оформлением (хром с тремя точками,
//! скруглённая рамка, фон Tokyo Night). Шрифт — моноширинный, сетка
//! выровнена абсолютными координатами tspan'ов. Скриншоты — это СНИМКИ
//! РЕАЛЬНЫХ ЭКРАНОВ, а не макеты: сцены собираются тем же `App`, что и в
//! проде, поэтому рассинхрон дизайна и картинок невозможен.
//!
//! Регенерация (после изменений дизайна — обязательна):
//! `cargo test gen_readme_screenshots -- --ignored` → docs/screenshots/*.svg
//!
//! Конвертация в PNG для README — `rsvg-convert -z 2` либо headless-Chromium
//! (см. журнал AGENTS.md). SVG остаются источником истины в репозитории.

use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};

/// Шрифт кадра и метрики ячейки (px). DejaVu Sans Mono: advance 0.602em.
const FONT: &str = "DejaVu Sans Mono, Noto Color Emoji, monospace";
/// Размер шрифта, px.
const FONT_SIZE: f32 = 15.0;
/// Ширина ячейки (0.602 × FONT_SIZE).
const CELL_W: f32 = 9.03;
/// Высота строки.
const ROW_H: f32 = 19.0;
/// Боковой паддинг окна.
const PAD_X: f32 = 14.0;
/// Высота оконного хрома (точки + заголовок).
const CHROME_H: f32 = 26.0;
/// Нижний паддинг.
const PAD_BOTTOM: f32 = 12.0;

/// ANSI-16 в hex (стандартная xterm-палитра; тема Tokyo Night сама в Rgb,
/// это страховка для именованных цветов ratatui).
const ANSI16: [u32; 16] = [
    0x000000, 0xcc0000, 0x4e9a06, 0xc4a000, 0x3465a4, 0x75507b, 0x06989a, 0xd3d7cf, //
    0x555753, 0xef2929, 0x8ae234, 0xfce94f, 0x729fcf, 0xad7fa8, 0x34e2e2, 0xeeeeec,
];

/// Цвет ячейки в CSS; `Reset` — None (подставляется дефолт сцены).
fn css_color(c: Color) -> Option<String> {
    let hex = |v: u32| Some(format!("#{v:06x}"));
    match c {
        Color::Reset => None,
        Color::Rgb(r, g, b) => Some(format!("#{r:02x}{g:02x}{b:02x}")),
        Color::Black => hex(ANSI16[0]),
        Color::Red => hex(ANSI16[1]),
        Color::Green => hex(ANSI16[2]),
        Color::Yellow => hex(ANSI16[3]),
        Color::Blue => hex(ANSI16[4]),
        Color::Magenta => hex(ANSI16[5]),
        Color::Cyan => hex(ANSI16[6]),
        Color::Gray => hex(ANSI16[7]),
        Color::DarkGray => hex(ANSI16[8]),
        Color::LightRed => hex(ANSI16[9]),
        Color::LightGreen => hex(ANSI16[10]),
        Color::LightYellow => hex(ANSI16[11]),
        Color::LightBlue => hex(ANSI16[12]),
        Color::LightMagenta => hex(ANSI16[13]),
        Color::LightCyan => hex(ANSI16[14]),
        Color::White => hex(ANSI16[15]),
        Color::Indexed(i) => {
            let i = i as u32;
            let v = match i {
                0..=15 => ANSI16[i as usize],
                16..=231 => {
                    let n = i - 16;
                    let lv = |k: u32| [0, 95, 135, 175, 215, 255][((n / k) % 6) as usize];
                    (lv(36) << 16) | (lv(6) << 8) | lv(1)
                }
                _ => {
                    let g = 8 + (i - 232) * 10;
                    (g << 16) | (g << 8) | g
                }
            };
            hex(v)
        }
    }
}

/// XML-эскейп текста ячейки.
fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Стиль одного прогона ячеек (одинаковые fg/bg/модификаторы).
struct Run {
    fg: Option<String>,
    bg: Option<String>,
    bold: bool,
    dim: bool,
    text: String,
}

/// Буфер ratatui → SVG с оконным хромом. `title` — заголовок окна.
pub(crate) fn buffer_to_svg(buf: &Buffer, title: &str) -> String {
    let area = buf.area;
    let cols = usize::from(area.width);
    let rows = usize::from(area.height);
    let w = PAD_X.mul_add(2.0, CELL_W * cols as f32);
    let h = (CHROME_H + PAD_BOTTOM).mul_add(1.0, ROW_H * rows as f32 + 6.0);
    let mut svg = String::with_capacity(64 * 1024);
    svg.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w:.0}\" height=\"{h:.0}\" \
         viewBox=\"0 0 {w:.0} {h:.0}\" font-family=\"{FONT}\" font-size=\"{FONT_SIZE}\">\n"
    ));
    // Окно: фон + тонкая рамка + хром.
    svg.push_str("<rect width=\"100%\" height=\"100%\" rx=\"10\" fill=\"#1a1b26\"/>\n");
    svg.push_str(&format!(
        "<rect x=\"0.5\" y=\"0.5\" width=\"{:.0}\" height=\"{:.0}\" rx=\"10\" fill=\"none\" stroke=\"#3b4261\"/>\n",
        w - 1.0,
        h - 1.0
    ));
    for (i, c) in ["#f7768e", "#e0af68", "#9ece6a"].iter().enumerate() {
        svg.push_str(&format!(
            "<circle cx=\"{:.0}\" cy=\"13\" r=\"5\" fill=\"{c}\"/>\n",
            20.0 + i as f32 * 16.0
        ));
    }
    svg.push_str(&format!(
        "<text x=\"{:.0}\" y=\"17\" fill=\"#565f89\" text-anchor=\"middle\">{}</text>\n",
        w / 2.0,
        esc(title)
    ));
    svg.push_str(&format!(
        "<line x1=\"0\" y1=\"{CHROME_H}\" x2=\"{w:.0}\" y2=\"{CHROME_H}\" stroke=\"#3b4261\"/>\n"
    ));

    // Ячейки: прогоны одинакового стиля → rect (фон) + text (глифы).
    let mut y_px = CHROME_H + ROW_H * 0.78 + 3.0;
    for row in 0..rows {
        let mut runs: Vec<Run> = Vec::new();
        let mut col_of_run: Vec<usize> = Vec::new();
        for col in 0..cols {
            let cell = &buf[(col as u16, row as u16)];
            if cell.skip {
                continue; // вторая половина широкого глифа
            }
            let fg = css_color(cell.fg);
            let bg = css_color(cell.bg);
            let bold = cell.modifier.contains(Modifier::BOLD);
            let dim = cell.modifier.contains(Modifier::DIM);
            let sym = cell.symbol();
            let same = runs.last().is_some_and(|r: &Run| {
                r.fg == fg && r.bg == bg && r.bold == bold && r.dim == dim
            });
            if same {
                if let Some(r) = runs.last_mut() {
                    r.text.push_str(sym);
                }
            } else {
                runs.push(Run {
                    fg,
                    bg,
                    bold,
                    dim,
                    text: sym.to_string(),
                });
                col_of_run.push(col);
            }
        }
        let mut out_runs = String::new();
        for (run, &col) in runs.iter().zip(&col_of_run) {
            let text = run.text.trim_end_matches(' ');
            let x = PAD_X + CELL_W * col as f32;
            // Фон рисуем по ПОЛНОЙ ширине прогона (пробелы в конце — тоже фон).
            if let Some(bg) = &run.bg {
                let rw = CELL_W * run.text.chars().count() as f32;
                out_runs.push_str(&format!(
                    "<rect x=\"{x:.1}\" y=\"{:.1}\" width=\"{rw:.1}\" height=\"{ROW_H}\" fill=\"{bg}\"/>",
                    y_px - ROW_H * 0.78
                ));
            }
            if !text.is_empty() {
                let mut style = String::new();
                if let Some(fg) = &run.fg {
                    style.push_str(&format!(" fill=\"{fg}\""));
                }
                if run.bold {
                    style.push_str(" font-weight=\"700\"");
                }
                if run.dim {
                    style.push_str(" opacity=\"0.72\"");
                }
                out_runs.push_str(&format!(
                    "<text x=\"{x:.1}\" y=\"{y_px:.1}\" xml:space=\"preserve\"{style}>{}</text>",
                    esc(text)
                ));
            }
        }
        // Дефолтный фон строки не рисуем — окно уже #1a1b26.
        svg.push_str(&out_runs);
        y_px += ROW_H;
    }
    svg.push_str("</svg>\n");
    svg
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::tui::app::{
        ChatBlock, RightTab, Screen, ToolState,
        testing::{set_context_usage, set_thinking, test_app},
    };

    use super::*;

    /// Снимок `App` в SVG: рендер в TestBackend заданного размера.
    fn snap(app: &mut crate::tui::app::App, w: u16, h: u16, title: &str) -> String {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("terminal");
        terminal.draw(|f| app.render(f)).expect("draw");
        buffer_to_svg(terminal.backend().buffer(), title)
    }

    fn write(out_dir: &std::path::Path, name: &str, svg: &str) {
        let path = out_dir.join(name);
        std::fs::write(&path, svg).expect("запись svg");
        eprintln!("shot: {} ({} КБ)", path.display(), svg.len() / 1024);
    }

    #[test]
    fn gen_readme_screenshots() {
        if std::env::var("ARCH_GEN_SHOTS").is_err() {
            return; // генерация — только по явному запросу
        }
        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/screenshots");
        std::fs::create_dir_all(&out).expect("mkdir");

        // 1. Сплэш: баннер с градиентом, модели, горячие клавиши.
        let mut app = test_app();
        write(&out, "01-splash.svg", &snap(&mut app, 116, 28, "arch"));

        // 2. Чат (hero): сквозной архитектурный ход — скиллы, KB, скоринг,
        //    mermaid-контейнеры на вкладке; статус-бар с живым индикатором
        //    контекста и фоновыми субагентами; скроллбар у рамки диалога.
        let mut app = test_app();
        app.screen = Screen::Chat;
        app.model_name = "deepseek:deepseek-v4-flash".into();
        app.tool_ctx.cwd = std::path::PathBuf::from("/home/user/sbp-gateway");
        app.push_block(ChatBlock::User(
            "спроектируй платёжный шлюз СБП (C2B): контейнеры, паттерны, маршрут".into(),
        ));
        app.push_block(ChatBlock::Tool {
            name: "skill_load".into(),
            state: ToolState::Ok,
            summary: "transactional-outbox [patterns-integration] — скилл в контексте".into(),
        });
        app.push_block(ChatBlock::Tool {
            name: "kb_search".into(),
            state: ToolState::Ok,
            summary: "4 фрагмента: saga-transactions, idempotent-consumer, strangler-acl…".into(),
        });
        app.push_block(ChatBlock::Tool {
            name: "web_search".into(),
            state: ToolState::Ok,
            summary: "AWS Builders' Library: 3 статьи по outbox и сверке".into(),
        });
        app.push_block(ChatBlock::Assistant(
            "## Контур шлюза (C4 — контейнеры)\n\
             **Ядро:** API ТСП → статусная машина платежа (единый источник истины) → outbox.\n\
             - **Паттерны:** сага, transactional outbox, идемпотентный потребитель\n\
             - **Адаптеры:** ОПКЦ (НСПК) и АБС — ядро от них контрактно независимо\n\
             - Решения фиксируем в ADR-001…003, инварианты — в ARCHITECTURE-SPINE.md"
                .into(),
        ));
        app.push_block(ChatBlock::Tool {
            name: "mermaid_render".into(),
            state: ToolState::Ok,
            summary: "flowchart: 6 узлов · рендер на вкладке ◇ Mermaid".into(),
        });
        app.push_block(ChatBlock::Tool {
            name: "control_score".into(),
            state: ToolState::Ok,
            summary: "Score 13 → маршрут Critical · гейты A0–A5, рубрика обязательна".into(),
        });
        app.push_block(ChatBlock::Assistant(
            "Готово к передаче: `/handoff claude-code ./sbp-gateway` — пакет с инвариантами, \
             критериями приёмки и рубрикой.".into(),
        ));
        app.panels.mermaid = crate::mermaid::render(
            "flowchart TD\n  API[API ТСП] --> SM[Ядро: статусная машина]\n  \
             SM --> OB[(Outbox)]\n  SM --> OP[Адаптер ОПКЦ]\n  OB --> ABS[Адаптер АБС]\n  \
             OP --> DLQ[(DLQ)]",
        )
        .expect("рендер mermaid");
        // Живой индикатор контекста + двое фоновых субагентов в статус-баре.
        set_context_usage(&mut app, 61_440, 1_000_000);
        let registry = crate::subagent::SubagentRegistry::new();
        for (id, task) in [
            ("sa-01", "разведка репозитория ТСП"),
            ("sa-02", "черновик ADR-002: статусная машина"),
        ] {
            registry.insert(crate::subagent::SubagentTask {
                id: id.into(),
                agent: "explore".into(),
                task: task.into(),
                status: crate::subagent::TaskStatus::Running,
                report: String::new(),
                started_at: "2026-08-15".into(),
                finished_at: None,
            });
        }
        let ctx = app.tool_ctx.clone().with_subagents(registry);
        app.tool_ctx = ctx;
        // Модель ещё работает: в очереди два сообщения (карточка в окне логов),
        // в поле ввода — многострочный черновик.
        set_thinking(&mut app, true);
        app.queue
            .push_back("а теперь NFR: p95 шлюза и деградация НСПК".into());
        app.queue
            .push_back("потом /handoff claude-code ./sbp-gateway\nс инвариантами и рубрикой".into());
        app.input
            .set_text("покажи fitness-функции для outbox и сверки\nи пороги для p95".into());
        write(&out, "02-chat-mermaid.svg", &snap(&mut app, 150, 40, "arch — чат + mermaid"));

        // 3. Пикер моделей (модалка поверх чата).
        let mut app = test_app();
        app.screen = Screen::Chat;
        app.model_name = "deepseek:deepseek-v4-flash".into();
        app.tool_ctx.cwd = std::path::PathBuf::from("/home/user/sbp-gateway");
        app.push_block(ChatBlock::User(
            "перед ревью ADR переключись на тяжёлую модель с ризонингом".into(),
        ));
        app.push_block(ChatBlock::Assistant(
            "Открываю пикер: у моделей с меткой «ризонинг» доступен `/think on`.".into(),
        ));
        app.open_model_picker();
        // В сцене текущая модель — stub-провайдер тестов; для снимка показываем
        // реалистичное состояние: deepseek — текущая (★ и курсор).
        if let Some(ask) = app.ask.as_mut() {
            ask.question = "Модель для этой сессии (сейчас: deepseek):".into();
            ask.recommended = Some("deepseek".into());
        }
        set_context_usage(&mut app, 12_300, 1_000_000);
        write(&out, "03-model-picker.svg", &snap(&mut app, 122, 32, "arch — /model"));

        // 4. Рубрика: отчёт LLM-судьи на правой вкладке; индикатор контекста
        //    за порогом L1 (оранжевый) — видна семантика цветов.
        let mut app = test_app();
        app.screen = Screen::Chat;
        app.model_name = "deepseek-pro:deepseek-v4-pro 🧠".into();
        app.tool_ctx.cwd = std::path::PathBuf::from("/home/user/sbp-gateway");
        app.push_block(ChatBlock::User(
            "оцени solutioning СБП-шлюза по якорной рубрике и прогоняй бенчмарк".into(),
        ));
        app.push_block(ChatBlock::Tool {
            name: "rubric_evaluate".into(),
            state: ToolState::Ok,
            summary: "solution_architecture: 4.2/5 — отчёт на вкладке ✓ Рубрика".into(),
        });
        app.push_block(ChatBlock::Tool {
            name: "bench_run".into(),
            state: ToolState::Ok,
            summary: "solution-bench: 8/10 сценариев PASS · 2 замечания по сверке".into(),
        });
        app.push_block(ChatBlock::Assistant(
            "**Итог: READY с замечаниями.** Обратимость (3/5) и критерии отката — \
             в доработку; остальное закрыто.".into(),
        ));
        app.push_block(ChatBlock::User(
            "зафиксируй замечания в ADR и подготовь вопросы к архкому".into(),
        ));
        app.push_block(ChatBlock::Tool {
            name: "adr_new".into(),
            state: ToolState::Ok,
            summary: "ADR-002 создан: docs/adr/ADR-002-state-machine.md".into(),
        });
        app.push_block(ChatBlock::Assistant(
            "Замечания внесены в ADR-002. На A3 выносится выбор вендора транспорта — \
             RFP-пакет готов (`docs/rfp/vendor-rfp.md`).".into(),
        ));
        app.panels.rubric = "Оценка: solution_architecture (якорная)\n\n\
             ✓ Контекст и драйверы      5/5\n✓ Альтернативы             4/5\n\
             ✓ Отрицательные следствия  4/5\n◌ Обратимость              3/5\n\
             ✓ Fitness-функции          5/5\n\n\
             Взвешенный итог: 4.2/5 — READY с замечаниями\nБенчмарк: 8/10 PASS"
            .into();
        app.right_tab = RightTab::Rubric;
        set_context_usage(&mut app, 812_000, 1_000_000);
        write(&out, "04-rubric.svg", &snap(&mut app, 134, 30, "arch — рубрика"));

        // 5. Handoff: архитектор передаёт контекст кодовому харнессу —
        //    пакет .arch-handoff/, прогон harness_run с умными таймаутами,
        //    JSON-контракт результата в сводке.
        let mut app = test_app();
        app.screen = Screen::Chat;
        app.model_name = "deepseek:deepseek-v4-flash".into();
        app.tool_ctx.cwd = std::path::PathBuf::from("/home/user/sbp-gateway");
        app.push_block(ChatBlock::User(
            "передай статусную машину на исполнение Claude Code и проконтролируй результат".into(),
        ));
        app.push_block(ChatBlock::Tool {
            name: "handoff_create".into(),
            state: ToolState::Ok,
            summary: ".arch-handoff/: TASK.md · ARCHITECTURE.md · CONSTRAINTS.yaml · \
                 RUBRIC.yaml · MANIFEST.json · adr/ — epic-context ~1 247 токенов"
                .into(),
        });
        app.push_block(ChatBlock::Tool {
            name: "harness_run".into(),
            state: ToolState::Ok,
            summary: "claude-code: код 0 · 1 847 с · контракт status=complete · \
                 assumptions 3 · open_questions 1 · прерываний нет".into(),
        });
        app.push_block(ChatBlock::Assistant(
            "**Исполнение принято.** Claude Code реализовал статусную машину и outbox-\n\
             ретраи: `pytest -q` — 14 passed, fitness `no-print-in-py` — PASS.\n\
             - Открытый вопрос: таймаут подтверждения НСПК — вынес в ADR-004\n\
             - Тишины не было: heartbeat по файлам репо, процессная группа жила весь прогон"
                .into(),
        ));
        app.push_block(ChatBlock::Tool {
            name: "control_check".into(),
            state: ToolState::Ok,
            summary: "fitness: 4/4 PASS · spine-инварианты AD-1…AD-3 не нарушены".into(),
        });
        app.push_block(ChatBlock::Assistant(
            "Контур целостен. Дальше: `/export docx` — протокол прогона для архкома.".into(),
        ));
        app.panels.mermaid = crate::mermaid::render(
            "flowchart LR\n  A[архитектор] -->|handoff-пакет| H(Claude Code)\n  \
             H -->|код + контракт| R[репозиторий]\n  A -->|control check| R",
        )
        .expect("рендер mermaid");
        set_context_usage(&mut app, 96_500, 1_000_000);
        write(&out, "05-handoff.svg", &snap(&mut app, 150, 36, "arch — handoff кодовому харнессу"));
    }
}
