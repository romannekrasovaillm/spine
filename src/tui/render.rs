//! Отрисовка TUI: сплэш с градиентным баннером, чат (диалог | вкладки),
//! строка ввода, статус-бар. Все блоки — со скруглёнными рамками.
//!
//! Правило ASCII-арта: строки mermaid-рендеров (box-drawing U+2500–257F,
//! геометрия U+25A0–25FF) НИКОГДА не переносятся — разрыв box-линий убивает
//! диаграмму; длинное клипается по ширине панели.

use std::fmt::Write as _;
use std::path::Path;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
    Wrap,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::assets;

use super::app::{App, ChatBlock, Panels, RightTab, SPINNER, Screen, ToolState};
use super::text::{markdown_lines, wrap_line};
use super::theme::Theme;

/// Ширина правой колонки (вкладки).
const RIGHT_WIDTH: u16 = 34;

/// Ширина правой колонки: под широкий mermaid-арт панель растёт (до 60%
/// терминала), чтобы схема помещалась целиком, без горизонтального клипа.
/// На остальных вкладках — фиксированная [`RIGHT_WIDTH`].
fn right_panel_width(app: &App, term_w: u16) -> u16 {
    if app.right_tab() != RightTab::Mermaid {
        return RIGHT_WIDTH;
    }
    let art_w = app
        .panels
        .mermaid
        .lines()
        .map(UnicodeWidthStr::width)
        .max()
        .unwrap_or(0);
    // +2 — рамка панели; диалог остаётся главным экраном (кап 60%).
    let want = u16_sat(art_w.saturating_add(2));
    let cap = u16_sat(usize::from(term_w) * 3 / 5).max(RIGHT_WIDTH);
    want.clamp(RIGHT_WIDTH, cap)
}

/// usize → u16 с насыщением (без паник на гигантских терминалах).
fn u16_sat(v: usize) -> u16 {
    u16::try_from(v).unwrap_or(u16::MAX)
}

/// Текст похож на ASCII-арт (box-линии/геометрические фигуры)?
/// Гуттеры чата (▎ U+258E) и галки (✓ U+2713) вне диапазонов — не арт.
fn is_art(text: &str) -> bool {
    text.chars()
        .any(|c| ('\u{2500}'..='\u{257f}').contains(&c) || ('\u{25a0}'..='\u{25ff}').contains(&c))
}

/// Строка (со спанами) — часть диаграммы?
fn line_is_art(line: &Line<'_>) -> bool {
    line.spans.iter().any(|s| is_art(&s.content))
}

/// Рисует текущий экран приложения.
pub(crate) fn draw(f: &mut Frame, app: &mut App) {
    let theme = app.theme;
    f.render_widget(Block::default().style(theme.base()), f.area());
    match &app.screen {
        Screen::Splash => draw_splash(f, f.area(), &theme),
        Screen::Fatal(error) => {
            let error = error.clone();
            draw_fatal(f, f.area(), &theme, &error);
        }
        Screen::Chat => draw_chat(f, app),
    }
}

/// Стартовый экран: градиентный баннер (cyan→purple по столбцам),
/// подпись, версия, подсказки клавиш.
fn draw_splash(f: &mut Frame, area: Rect, theme: &Theme) {
    let banner_lines: Vec<&str> = assets::BANNER
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect();
    let chunks = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(u16_sat(banner_lines.len())),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .split(area);

    // Градиент по столбцам: figlet-строки баннера красятся посимвольно,
    // слоган внутри ассета — приглушённо.
    let max_w = banner_lines
        .iter()
        .map(|l| UnicodeWidthStr::width(*l))
        .max()
        .unwrap_or(1)
        .max(1);
    let banner: Vec<Line> = banner_lines
        .iter()
        .map(|l| {
            if l.contains('█') {
                let spans: Vec<Span> = l
                    .chars()
                    .enumerate()
                    .map(|(i, ch)| {
                        #[expect(clippy::cast_precision_loss, reason = "ширина баннера мала")]
                        let t = i as f32 / max_w as f32;
                        Span::styled(
                            ch.to_string(),
                            Style::default().fg(theme.gradient(t)).bg(theme.bg),
                        )
                    })
                    .collect();
                Line::from(spans)
            } else {
                Line::from(Span::styled((*l).to_string(), theme.muted()))
            }
        })
        .collect();
    f.render_widget(
        Paragraph::new(banner).alignment(Alignment::Center),
        chunks[1],
    );

    let subtitle = Line::from(Span::styled(
        "доменный харнесс solution-архитектора",
        Style::default()
            .fg(theme.purple)
            .bg(theme.bg)
            .add_modifier(Modifier::BOLD),
    ));
    f.render_widget(
        Paragraph::new(subtitle).alignment(Alignment::Center),
        chunks[2],
    );

    let version = Line::from(Span::styled(
        concat!(
            "v",
            env!("CARGO_PKG_VERSION"),
            " · DeepSeek / Kimi / GLM · Tokyo Night"
        ),
        theme.muted(),
    ));
    f.render_widget(
        Paragraph::new(version).alignment(Alignment::Center),
        chunks[3],
    );

    let key = |k: &str| Span::styled(k.to_string(), theme.heading());
    let sep = |t: &str| Span::styled(t.to_string(), theme.muted());
    let hints = Line::from(vec![
        key("/help"),
        sep(" — команды · "),
        key("Enter"),
        sep(" — чат · "),
        key("q"),
        sep(" — выход"),
    ]);
    f.render_widget(
        Paragraph::new(hints).alignment(Alignment::Center),
        chunks[4],
    );
}

/// Экран фатальной ошибки инициализации (модель/конфиг).
fn draw_fatal(f: &mut Frame, area: Rect, theme: &Theme, error: &str) {
    let width = area.width.saturating_sub(4).clamp(20, 72);
    let cols = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(width),
        Constraint::Min(0),
    ])
    .split(area);
    let rows = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(9),
        Constraint::Min(0),
    ])
    .split(cols[1]);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.error())
        .title(Span::styled(
            " Ошибка инициализации ",
            Style::default()
                .fg(theme.red)
                .bg(theme.bg)
                .add_modifier(Modifier::BOLD),
        ));
    let text = vec![
        Line::from(Span::styled(error.to_string(), theme.error())),
        Line::default(),
        Line::from(Span::styled(
            "Проверьте config.toml (команда `arch init`) и переменные окружения \
             с API-ключами (api_key_env у моделей).",
            theme.muted(),
        )),
        Line::default(),
        Line::from(Span::styled("q — выход", theme.muted())),
    ];
    f.render_widget(
        Paragraph::new(text).block(block).wrap(Wrap { trim: false }),
        rows[1],
    );
}

/// Основной экран: диалог | вкладки; снизу ввод и статус-бар.
/// Поверх — модалка выбора вариантов (`propose_options`), если она открыта.
fn draw_chat(f: &mut Frame, app: &mut App) {
    let theme = app.theme;
    // Поле ввода многострочное: высота растёт с текстом (перенос по ширине
    // и явные '\n'), максимум MAX_INPUT_ROWS видимых строк + рамка.
    let input_h = {
        let w = usize::from(f.area().width.saturating_sub(2)).max(1);
        let (lines, _, _) = wrap_input(app.input.text(), app.input.cursor(), w);
        u16_sat(lines.len().min(MAX_INPUT_ROWS)) + 2
    };
    let rows = Layout::vertical([
        Constraint::Min(6),
        Constraint::Length(input_h),
        Constraint::Length(1),
    ])
    .split(f.area());
    let right_w = right_panel_width(app, f.area().width);
    let cols =
        Layout::horizontal([Constraint::Min(24), Constraint::Length(right_w)]).split(rows[0]);

    draw_dialog(f, cols[0], app, &theme);
    // Очередь сообщений — плавающей карточкой внизу окна логов (над вводом).
    if !app.queue.is_empty() {
        draw_queue_overlay(f, cols[0], app, &theme);
    }
    draw_right(f, cols[1], app, &theme);
    draw_input(f, rows[1], app, &theme);
    draw_status(f, rows[2], app, &theme);
    // Просмотрщик вкладки — под модалкой выбора (она блокирующая).
    if app.viewer.is_some() {
        draw_viewer(f, f.area(), app, &theme);
    }
    if app.ask.is_some() {
        draw_ask(f, f.area(), app, &theme);
    }
}

/// Полноэкранный просмотрщик активной вкладки (F4): вся ширина терминала,
/// вертикальный скролл и ГОРИЗОНТАЛЬНАЯ панорама для широкого mermaid-арта.
/// Основной экран не перестраивается — viewer накрывает его слоем.
fn draw_viewer(f: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    let tab = app.right_tab();
    let inner_w = usize::from(area.width.saturating_sub(2)).max(1);
    let view_h = usize::from(area.height.saturating_sub(2)).max(1);
    let content = app.panels.content(tab).to_string();

    let mut lines: Vec<Line<'static>> = Vec::new();
    if content.is_empty() {
        lines.push(Line::from(Span::styled(
            Panels::placeholder(tab),
            theme.muted(),
        )));
    } else if tab == RightTab::Mermaid {
        // Арт не переносим: клип по ширине окна (дальше — горизонтальный сдвиг).
        for l in content.lines() {
            let style = if is_art(l) { theme.art() } else { theme.base() };
            lines.push(Line::from(Span::styled(l.to_string(), style)));
        }
    } else {
        for l in content.lines() {
            lines.extend(wrap_line(
                &Line::from(Span::styled(l.to_string(), theme.base())),
                inner_w,
            ));
        }
    }

    // Клампы скролла по фактическим размерам содержимого.
    let mut v = app.viewer.unwrap_or_default();
    v.scroll_y = v.scroll_y.min(lines.len().saturating_sub(view_h));
    let max_line_w = lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
                .sum::<usize>()
        })
        .max()
        .unwrap_or(0);
    v.scroll_x = v.scroll_x.min(max_line_w.saturating_sub(inner_w));
    app.viewer = Some(v);

    let visible: Vec<Line> = lines
        .into_iter()
        .skip(v.scroll_y)
        .take(view_h)
        .map(|l| {
            if v.scroll_x == 0 {
                l
            } else {
                hclip_line(&l, v.scroll_x)
            }
        })
        .collect();

    let icon = match tab {
        RightTab::Mermaid => "◇",
        RightTab::Rubric => "✓",
        RightTab::Knowledge => "◈",
    };
    let pos = if v.scroll_y > 0 || v.scroll_x > 0 {
        format!(" · +{} строк, →{} кол.", v.scroll_y, v.scroll_x)
    } else {
        String::new()
    };
    let title = format!(
        " {icon} {} — на весь экран{pos} · ↑↓/PgUp/PgDn · ←→ панорама · F4/Esc — назад ",
        tab.title()
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.cyan).bg(theme.bg))
        .title(Span::styled(
            title,
            Style::default()
                .fg(theme.cyan)
                .bg(theme.bg)
                .add_modifier(Modifier::BOLD),
        ))
        .style(theme.base());
    f.render_widget(ratatui::widgets::Clear, area);
    f.render_widget(Paragraph::new(visible).block(block), area);
}

/// Горизонтальный срез строки (по спанам) с display-колонки `offset`:
/// unicode-width безопасно; box-линии и кириллица арта — width 1.
/// Широкий глиф, разрезанный границей оффсета, пропускается целиком.
fn hclip_line(line: &Line<'_>, offset: usize) -> Line<'static> {
    use unicode_width::UnicodeWidthChar;
    let mut col = 0usize;
    let mut spans: Vec<Span<'static>> = Vec::new();
    for span in &line.spans {
        let mut buf = String::new();
        for ch in span.content.chars() {
            let w = UnicodeWidthChar::width(ch).unwrap_or(0);
            let next = col + w;
            if next <= offset || col < offset {
                col = next;
                continue;
            }
            buf.push(ch);
            col = next;
        }
        if !buf.is_empty() {
            spans.push(Span::styled(buf, span.style));
        }
    }
    Line::from(spans)
}

/// Модальная панель выбора вариантов (инструмент `propose_options)`:
/// центрированное окно поверх чата — вопрос, варианты, курсор, подсказки.
fn draw_ask(f: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let Some(ask) = &app.ask else {
        return;
    };
    let width = area.width.saturating_sub(6).clamp(40, 76).min(area.width);
    let inner_w = usize::from(width.saturating_sub(4)).max(1);
    // Высота: вопрос (с переносом) + варианты (по 2 строки: label + описание)
    // + разделители/подсказка + рамка.
    let question_h = wrap_line(
        &Line::from(Span::styled(ask.question.clone(), theme.base())),
        inner_w,
    )
    .len();
    let height = u16_sat(question_h + ask.options.len() * 2 + 5)
        .min(area.height.saturating_sub(2))
        .max(6);
    let cols = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(width),
        Constraint::Min(0),
    ])
    .split(area);
    let rows = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(height),
        Constraint::Min(0),
    ])
    .split(cols[1]);
    let panel = rows[1];

    let (title, esc_hint) = match ask.kind {
        crate::tui::app::AskKind::Tool => (" ◈ решение за вами ", " решить агенту"),
        crate::tui::app::AskKind::ModelPicker => (" ◈ выбор модели ", " отмена"),
        crate::tui::app::AskKind::SessionPicker => (" ◈ выбор сессии ", " отмена"),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.cyan).bg(theme.bg))
        .title(Span::styled(
            title,
            Style::default()
                .fg(theme.cyan)
                .bg(theme.bg)
                .add_modifier(Modifier::BOLD),
        ))
        .style(theme.base());
    f.render_widget(ratatui::widgets::Clear, panel);
    f.render_widget(block, panel);
    let inner = Rect {
        x: panel.x + 2,
        y: panel.y + 1,
        width: panel.width.saturating_sub(4),
        height: panel.height.saturating_sub(2),
    };

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.extend(wrap_line(
        &Line::from(Span::styled(
            ask.question.clone(),
            Style::default()
                .fg(theme.fg)
                .bg(theme.bg)
                .add_modifier(Modifier::BOLD),
        )),
        inner_w,
    ));
    lines.push(Line::default());
    for (i, opt) in ask.options.iter().enumerate() {
        let current = i == ask.selected;
        let recommended = ask.recommended.as_deref() == Some(opt.label.as_str());
        let (mark, num_style) = if current {
            (
                "❯ ",
                Style::default()
                    .fg(theme.cyan)
                    .bg(theme.bg)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            ("  ", theme.muted())
        };
        let star = if recommended { " ★" } else { "" };
        lines.push(Line::from(vec![
            Span::styled(mark, num_style),
            Span::styled(format!("{}. ", i + 1), num_style),
            Span::styled(opt.label.clone(), num_style),
            Span::styled(star, Style::default().fg(theme.orange).bg(theme.bg)),
        ]));
        if !opt.description.is_empty() {
            for extra in wrap_line(
                &Line::from(Span::styled(
                    format!("    {}", opt.description),
                    theme.muted(),
                )),
                inner_w,
            ) {
                lines.push(extra);
            }
        }
    }
    lines.push(Line::default());
    let key = |k: &str| Span::styled(k.to_string(), theme.heading());
    let sep = |t: &str| Span::styled(t.to_string(), theme.muted());
    lines.push(Line::from(vec![
        key("↑/↓"),
        sep(" выбор · "),
        key("Enter"),
        sep(" подтвердить · "),
        key("1-4"),
        sep(" быстро · "),
        key("Esc"),
        sep(esc_hint),
    ]));
    f.render_widget(Paragraph::new(lines), inner);
}

/// Центральная колонка: блоки диалога с переносом и прокруткой.
/// Арт-строки (mermaid) не переносятся — клипаются.
fn draw_dialog(f: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    let inner_w = usize::from(area.width.saturating_sub(2)).max(1);
    let mut lines: Vec<Line<'static>> = Vec::new();
    if app.blocks.is_empty() {
        lines.push(Line::from(Span::styled(
            "Пусто. Напишите сообщение — или /help для списка команд.",
            theme.muted(),
        )));
    }
    for block in &app.blocks {
        for line in block_lines(block, theme) {
            if line_is_art(&line) {
                lines.push(line);
            } else {
                lines.extend(wrap_line(&line, inner_w));
            }
        }
        lines.push(Line::default());
    }

    let view_h = usize::from(area.height.saturating_sub(2));
    app.viewport = view_h;
    let total = lines.len();
    let max_scroll = total.saturating_sub(view_h);
    if app.scroll > max_scroll {
        app.scroll = max_scroll;
    }
    let skip = total.saturating_sub(view_h + app.scroll);
    // Состояние для выделения мышью: внутренняя область, plain-строки, сдвиг.
    app.dialog_inner = Some(Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    });
    app.dialog_lines = lines.iter().map(line_to_plain).collect();
    app.dialog_skip = skip;
    let visible: Vec<Line> = lines.into_iter().skip(skip).take(view_h).collect();

    let title = if app.scroll > 0 {
        format!(" Диалог · прокрутка +{} (PgDn — вниз) ", app.scroll)
    } else {
        " Диалог ".to_string()
    };
    let border_style = if app.thinking() {
        Style::default().fg(theme.purple).bg(theme.bg)
    } else {
        theme.border()
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style)
        .title(Span::styled(title, theme.heading()));
    f.render_widget(Paragraph::new(visible).block(block), area);

    // Скроллбар на правой кромке рамки: видно, где мы в истории диалога.
    // Трек повторяет глиф рамки (│), бегунок █ — акцентный.
    if max_scroll > 0 && area.height > 3 {
        let sb_area = Rect {
            x: area.x + area.width.saturating_sub(1),
            y: area.y + 1,
            width: 1,
            height: area.height.saturating_sub(2),
        };
        let mut sb_state = ScrollbarState::new(max_scroll)
            .position(skip)
            .viewport_content_length(view_h);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(Some("│"))
            .thumb_symbol("█")
            .track_style(theme.muted())
            .thumb_style(Style::default().fg(theme.cyan).bg(theme.bg));
        f.render_stateful_widget(scrollbar, sb_area, &mut sb_state);
    }

    // Кнопка «▼» в правом нижнем углу: прыжок к свежему ответу.
    // Показывается, только когда пользователь поднялся выше дна.
    if app.scroll > 0 && area.width > 6 && area.height > 3 {
        let btn = Rect {
            x: area.x + area.width.saturating_sub(4),
            y: area.y + area.height.saturating_sub(2),
            width: 3,
            height: 1,
        };
        app.jump_btn = Some(btn);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(" ▼ ", theme.badge()))),
            btn,
        );
    } else {
        app.jump_btn = None;
    }

    // Выделение мышью: пост-пасс поверх виджета — инверсия фона ячеек.
    // Строки считает app.selection_rows() в координатах контента.
    if app.selection.is_some() {
        if let Some(inner) = app.dialog_inner {
            let buf = f.buffer_mut();
            for (idx, c0, c1) in app.selection_rows() {
                let row = inner.y + u16_sat(idx.saturating_sub(app.dialog_skip));
                for col in c0..c1 {
                    let x = inner.x + u16_sat(col);
                    if x < inner.x + inner.width {
                        buf[(x, row)].set_style(theme.selection());
                    }
                }
            }
        }
    }
}

/// Plain-текст строки ratatui (конкатенация спанов) — для выделения мышью.
fn line_to_plain(line: &Line<'_>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

/// Блок чата → стилизованные строки (до переноса по ширине).
fn block_lines(block: &ChatBlock, theme: &Theme) -> Vec<Line<'static>> {
    match block {
        ChatBlock::User(text) => {
            let mut out = vec![Line::from(vec![
                Span::styled("● ", theme.heading()),
                Span::styled("вы", theme.heading()),
            ])];
            for l in text.lines() {
                out.push(Line::from(vec![
                    Span::styled("▎ ", theme.accent()),
                    Span::styled(l.to_string(), theme.base()),
                ]));
            }
            out
        }
        ChatBlock::Assistant(text) => {
            let mut out = vec![Line::from(vec![
                Span::styled("◆ ", theme.purple().add_modifier(Modifier::BOLD)),
                Span::styled("арх", theme.purple().add_modifier(Modifier::BOLD)),
            ])];
            out.extend(markdown_lines(text, theme));
            out
        }
        ChatBlock::Tool {
            name,
            state,
            summary,
        } => {
            let (mark, color) = match state {
                ToolState::Running => ("◌", theme.orange),
                ToolState::Ok => ("✓", theme.green),
                ToolState::Error => ("✗", theme.red),
            };
            let mark_style = Style::default().fg(color).bg(theme.bg);
            let mut out = vec![Line::from(vec![
                Span::styled(format!("{mark} "), mark_style),
                Span::styled(name.clone(), mark_style.add_modifier(Modifier::BOLD)),
            ])];
            if !matches!(state, ToolState::Running) && !summary.is_empty() {
                // Упрощённо: последние 3 строки вывода, приглушённо.
                let summary_lines: Vec<&str> = summary.lines().collect();
                let start = summary_lines.len().saturating_sub(3);
                for l in &summary_lines[start..] {
                    out.push(Line::from(Span::styled(
                        format!("  {}", l.trim_end()),
                        theme.muted(),
                    )));
                }
            }
            out
        }
        ChatBlock::System { command, text } => {
            let mut out = vec![Line::from(vec![
                Span::styled("» ", theme.purple()),
                Span::styled(command.clone(), theme.purple().add_modifier(Modifier::BOLD)),
            ])];
            for l in text.lines() {
                out.push(Line::from(Span::styled(l.to_string(), theme.base())));
            }
            out
        }
        ChatBlock::Error(text) => {
            let mut out = vec![Line::from(Span::styled(
                "✗ ошибка",
                theme.error().add_modifier(Modifier::BOLD),
            ))];
            for l in text.lines() {
                out.push(Line::from(Span::styled(l.to_string(), theme.error())));
            }
            out
        }
    }
}

/// Правая колонка: вкладки Mermaid / Рубрика / Знания.
/// Mermaid — без переносов (арт клипается), остальные — с переносом.
fn draw_right(f: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let inner_w = usize::from(area.width.saturating_sub(2)).max(1);
    // Арт шире даже расширенной панели (кап 60%)? Подскажем путь: F4.
    let mermaid_clipped = app
        .panels
        .mermaid
        .lines()
        .map(UnicodeWidthStr::width)
        .max()
        .unwrap_or(0)
        > inner_w;
    let mut title_spans = Vec::new();
    for tab in &RightTab::ALL {
        let style = if *tab == app.right_tab() {
            Style::default()
                .fg(theme.bg)
                .bg(theme.cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            theme.muted()
        };
        let icon = match tab {
            RightTab::Mermaid => "◇",
            RightTab::Rubric => "✓",
            RightTab::Knowledge => "◈",
        };
        let label = if *tab == RightTab::Mermaid && mermaid_clipped {
            format!(" {icon} {} ·F4 — вся схема ", tab.title())
        } else {
            format!(" {icon} {} ", tab.title())
        };
        title_spans.push(Span::styled(label, style));
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.border())
        .title(Line::from(title_spans));

    let tab = app.right_tab();
    let content = app.panels.content(tab);
    let mut lines: Vec<Line<'static>> = Vec::new();
    if content.is_empty() {
        lines.extend(wrap_line(
            &Line::from(Span::styled(Panels::placeholder(tab), theme.muted())),
            inner_w,
        ));
    } else if tab == RightTab::Mermaid {
        // Арт не переносим: клип по ширине, зелёный оттенок для схемы.
        for l in content.lines() {
            let style = if is_art(l) { theme.art() } else { theme.base() };
            lines.push(Line::from(Span::styled(l.to_string(), style)));
        }
    } else {
        for l in content.lines() {
            lines.extend(wrap_line(
                &Line::from(Span::styled(l.to_string(), theme.base())),
                inner_w,
            ));
        }
    }
    f.render_widget(Paragraph::new(lines).block(block), area);
}

/// Максимум видимых строк поля ввода (дальше окно следует за курсором).
const MAX_INPUT_ROWS: usize = 8;

/// Prompt первой строки ввода; продолжение — отступ той же ширины (2).
const PROMPT: &str = "❯ ";
const PROMPT_CONT: &str = "  ";

/// Раскладывает текст поля ввода на визуальные строки ширины `width`:
/// перенос по ширине + жёсткие '\n'. Возвращает (строки, строка курсора,
/// x курсора); x включает prompt/отступ (2 ячейки).
fn wrap_input(text: &str, cursor: usize, width: usize) -> (Vec<String>, usize, usize) {
    let avail = width.saturating_sub(2).max(1);
    let mut rows: Vec<String> = Vec::new();
    let mut row = String::new();
    let mut row_w = 0usize;
    let mut cur: Option<(usize, usize)> = None;
    for (i, c) in text.char_indices() {
        if i == cursor {
            cur = Some((rows.len(), 2 + row_w));
        }
        if c == '\n' {
            rows.push(std::mem::take(&mut row));
            row_w = 0;
            continue;
        }
        let cw = UnicodeWidthChar::width(c).unwrap_or(0);
        if row_w + cw > avail {
            rows.push(std::mem::take(&mut row));
            row_w = 0;
        }
        row.push(c);
        row_w += cw;
    }
    rows.push(row);
    let (cur_row, cur_x) = cur.unwrap_or((rows.len() - 1, 2 + row_w));
    (rows, cur_row, cur_x)
}

/// Плавающая карточка очереди сообщений внизу окна логов: следующее на
/// запуск сообщение подсвечено (▶), остальные — приглушены (•); не более
/// трёх строк + «ещё N». Нижний ряд диалога оставлен кнопке «▼».
fn draw_queue_overlay(f: &mut Frame, dialog: Rect, app: &App, theme: &Theme) {
    if dialog.width < 24 || dialog.height < 8 {
        return;
    }
    let shown = app.queue.len().min(3);
    let more = usize::from(app.queue.len() > 3);
    let h = u16_sat(shown + more + 2);
    let area = Rect {
        x: dialog.x + 1,
        y: dialog.y + dialog.height.saturating_sub(h + 2),
        width: dialog.width - 2,
        height: h,
    };
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.purple).bg(theme.bg))
        .style(Style::default().bg(theme.bg))
        .title(Span::styled(
            format!(" ⏷ очередь · {} ", app.queue.len()),
            theme.purple(),
        ));
    let inner_w = usize::from(area.width.saturating_sub(6)).max(1);
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (i, q) in app.queue.iter().take(3).enumerate() {
        // Многострочное сообщение показываем первой строкой с маркером ↵.
        let first = q.lines().next().unwrap_or("");
        let fold = if q.contains('\n') { " ↵" } else { "" };
        let text: String = first.chars().take(inner_w).collect();
        let style = if i == 0 {
            Style::default()
                .fg(theme.green)
                .bg(theme.bg)
                .add_modifier(Modifier::BOLD)
        } else {
            theme.muted()
        };
        let marker = if i == 0 { "▶" } else { "•" };
        lines.push(Line::from(Span::styled(
            format!("{marker} {}. {text}{fold}", i + 1),
            style,
        )));
    }
    if more == 1 {
        lines.push(Line::from(Span::styled(
            format!("… ещё {}", app.queue.len() - 3),
            theme.muted(),
        )));
    }
    f.render_widget(Paragraph::new(lines).block(block), area);
}

/// Нижнее поле ввода: многострочное (перенос по ширине; перевод строки —
/// Shift+Enter / Alt+Enter / Ctrl+J), растёт до `MAX_INPUT_ROWS` строк, дальше
/// видимое окно следует за курсором. Плюс ghost-подсказка автодополнения.
fn draw_input(f: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let thinking = app.thinking();
    let title = if thinking {
        let mut t = format!(" {} модель думает…", SPINNER[app.spinner_frame()]);
        if app.queue.is_empty() {
            t.push_str(" · Enter — в очередь · Alt+Enter — срочно · Esc — прервать ");
        } else {
            let _ = write!(
                t,
                " · очередь: {} · Alt+Enter — срочно · Esc — прервать ",
                app.queue.len()
            );
        }
        t
    } else if !app.queue.is_empty() {
        format!(" очередь: {} ", app.queue.len())
    } else {
        String::new()
    };
    let border = if thinking {
        Style::default().fg(theme.purple).bg(theme.bg)
    } else {
        theme.accent()
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border)
        .title(Span::styled(title, theme.purple()));

    let inner_w = usize::from(area.width.saturating_sub(2)).max(1);
    let (rows, cur_row, cur_x) = wrap_input(app.input.text(), app.input.cursor(), inner_w);
    let visible = usize::from(area.height.saturating_sub(2)).max(1);
    let skip = cur_row.saturating_sub(visible.saturating_sub(1));
    let multiline = app.input.text().contains('\n');
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (i, row) in rows.iter().enumerate().skip(skip).take(visible) {
        let mut spans = vec![
            Span::styled(if i == 0 { PROMPT } else { PROMPT_CONT }, theme.heading()),
            Span::styled(row.clone(), theme.base()),
        ];
        // Ghost-подсказка — только у однострочного ввода, на последней строке.
        if !multiline && i == rows.len() - 1 {
            if let Some(hint) = app.input.ghost_hint() {
                spans.push(Span::styled(hint, theme.muted()));
            }
        }
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines).block(block), area);

    // Ввод активен и во время хода (очередь) — курсор нужен всегда.
    let x = area.x.saturating_add(1).saturating_add(u16_sat(cur_x));
    let max_x = area.x.saturating_add(area.width.saturating_sub(2));
    let y = area
        .y
        .saturating_add(1)
        .saturating_add(u16_sat(cur_row - skip));
    let max_y = area.y.saturating_add(area.height.saturating_sub(2));
    f.set_cursor_position((x.min(max_x), y.min(max_y)));
}

/// Статус-бар: бейдж модели | индикатор контекста | cwd | заметки | подсказки.
fn draw_status(f: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    // Хинт клавиш: полный на широком терминале, компактный на узком —
    // иначе он вытесняет индикаторы модели, контекста и субагентов.
    let hint = if area.width >= 125 {
        "F1–F3 вкладки · F4 — на весь экран · Ctrl+J — новая строка · q — выход"
    } else {
        "F1–F3 · F4 · Ctrl+J · q — выход"
    };
    let hint_w = u16_sat(UnicodeWidthStr::width(hint) + 1);
    let cols = Layout::horizontal([Constraint::Min(0), Constraint::Length(hint_w)]).split(area);

    let mut spans = vec![Span::styled(format!(" {} ", app.model_name), theme.badge())];
    // Индикатор заполнения контекста — сразу после бейджа модели.
    spans.extend(context_spans(app, theme));
    spans.push(Span::styled(
        format!("  {}", shorten_path(&app.tool_ctx.cwd)),
        theme.muted(),
    ));
    if let Some(extra) = app.status_extra() {
        spans.push(Span::styled(
            format!("  · {extra}"),
            Style::default().fg(theme.orange).bg(theme.bg),
        ));
    }
    // Фоновые субагенты/ralph-циклы: индикатор со спиннером и счётчиком.
    let running = app.subagents_running();
    if running > 0 {
        spans.push(Span::styled(
            format!("  · {} субагенты: {running} ", SPINNER[app.spinner_frame()]),
            Style::default().fg(theme.green).bg(theme.bg),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), cols[0]);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(hint, theme.muted()))).alignment(Alignment::Right),
        cols[1],
    );
}

/// Индикатор заполнения контекста: «◈ 12.3k/1.0M ▰▰▱▱▱▱▱▱ 1%».
/// Цвет шкалы — по порогам компактификации из конфига: зелёный до L1,
/// оранжевый до L3, дальше красный (авто-компактификация уже близко/идёт).
fn context_spans(app: &App, theme: &Theme) -> Vec<Span<'static>> {
    const WIDTH: usize = 8;
    let used = app.history_tokens();
    let budget = app.context_budget();
    if budget == 0 {
        return vec![Span::styled(format!(" ◈ ~{used} ток."), theme.muted())];
    }
    let pct = used.saturating_mul(100) / budget;
    let agent_cfg = &app.tool_ctx.config.agent;
    let color = if pct >= agent_cfg.compact_l3_pct {
        theme.red
    } else if pct >= agent_cfg.compact_l1_pct {
        theme.orange
    } else {
        theme.green
    };
    let filled = (used.saturating_mul(WIDTH) / budget).min(WIDTH);
    let bar = format!("{}{}", "▰".repeat(filled), "▱".repeat(WIDTH - filled));
    vec![
        Span::styled(
            format!(" ◈ {}/{} ", fmt_tokens(used), fmt_tokens(budget)),
            theme.muted(),
        ),
        Span::styled(
            format!("{bar} {pct}%"),
            Style::default().fg(color).bg(theme.bg),
        ),
    ]
}

/// Человекочитаемый размер токенов: 999 → «999», `12_345` → «12.3k»,
/// `1_000_000` → «1.0M».
#[allow(clippy::cast_precision_loss)]
fn fmt_tokens(n: usize) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Схлопывает домашний каталог в `~` для компактного статус-бара.
fn shorten_path(path: &Path) -> String {
    let s = path.to_string_lossy();
    if let Some(home) = dirs::home_dir() {
        let h = home.to_string_lossy();
        if let Some(rest) = s.strip_prefix(h.as_ref()) {
            return format!("~{rest}");
        }
    }
    s.into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::testing::test_app;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// Текстовое содержимое буфера `TestBackend` (построчно).
    fn buffer_text(term: &Terminal<TestBackend>) -> String {
        let buf = term.backend().buffer();
        let area = buf.area;
        let mut s = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                s.push_str(buf[(x, y)].symbol());
            }
            s.push('\n');
        }
        s
    }

    #[test]
    fn splash_shows_banner_subtitle_and_hints() {
        let mut app = test_app();
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("terminal");
        terminal.draw(|f| app.render(f)).expect("draw");
        let text = buffer_text(&terminal);
        // Содержимое баннера — зона агента `content`; сверяемся с самим ассетом.
        let banner_line = assets::BANNER
            .lines()
            .find(|l| !l.trim().is_empty())
            .expect("в баннере есть непустая строка");
        assert!(
            text.contains(banner_line.trim_end()),
            "баннер не найден:\n{text}"
        );
        assert!(
            text.contains("доменный харнесс solution-архитектора"),
            "подпись не найдена:\n{text}"
        );
        assert!(text.contains("/help — команды"), "подсказки:\n{text}");
    }

    #[test]
    fn chat_shows_panels_statusbar_and_input() {
        let mut app = test_app();
        app.screen = Screen::Chat;
        app.model_name = "test:model".into();
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("terminal");
        terminal.draw(|f| app.render(f)).expect("draw");
        let text = buffer_text(&terminal);
        for needle in [
            "Диалог",
            "Mermaid",
            "Рубрика",
            "Знания",
            "❯",
            "test:model",
            "q — выход",
        ] {
            assert!(text.contains(needle), "не найдено «{needle}»:\n{text}");
        }
        // Левая колонка с каталогом команд убрана: заголовка быть не должно.
        assert!(
            !text.contains("Команды"),
            "панель команд не скрыта:\n{text}"
        );
    }

    #[test]
    fn chat_renders_user_assistant_and_tool_blocks() {
        let mut app = test_app();
        app.screen = Screen::Chat;
        app.push_block(ChatBlock::User("привет, арх".into()));
        app.push_block(ChatBlock::Assistant("# Ответ\nтекст ответа".into()));
        app.push_block(ChatBlock::Tool {
            name: "kb_search".into(),
            state: ToolState::Ok,
            summary: "найдено 3 фрагмента".into(),
        });
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("terminal");
        terminal.draw(|f| app.render(f)).expect("draw");
        let text = buffer_text(&terminal);
        for needle in [
            "● вы",
            "привет, арх",
            "◆ арх",
            "Ответ",
            "✓ kb_search",
            "найдено 3 фрагмента",
        ] {
            assert!(text.contains(needle), "не найдено «{needle}»:\n{text}");
        }
    }

    #[test]
    fn ascii_art_is_not_wrapped_in_dialog() {
        let mut app = test_app();
        app.screen = Screen::Chat;
        // Арт шире диалога: при терминале 60 диалог ≈ 24 колонки внутри.
        let art = "┌──────────┐     ┌──────────┐     ┌──────────┐xxxx";
        app.push_block(ChatBlock::System {
            command: "mermaid".into(),
            text: art.to_string(),
        });
        let mut terminal = Terminal::new(TestBackend::new(60, 14)).expect("terminal");
        terminal.draw(|f| app.render(f)).expect("draw");
        let text = buffer_text(&terminal);
        // Клип: начало строки арта сохраняет форму без разрыва переносом.
        assert!(
            text.contains("┌──────────┐     ┌───"),
            "арт не порван переносом:\n{text}"
        );
    }

    #[test]
    fn mermaid_tab_widens_to_fit_wide_art() {
        let mut app = test_app();
        app.screen = Screen::Chat;
        // Арт 70 колонок: при терминале 150 панель растёт до 72 (кап 90).
        let wide = format!("┌{}┐", "─".repeat(68));
        app.panels.mermaid = format!("{wide}\n│{:^68}│\n└{}┘", "схема", "─".repeat(68));
        let mut terminal = Terminal::new(TestBackend::new(150, 30)).expect("terminal");
        terminal.draw(|f| app.render(f)).expect("draw");
        let text = buffer_text(&terminal);
        assert!(
            text.contains(&wide),
            "широкая схема показана целиком (панель расширилась):\n{text}"
        );
        assert!(
            !text.contains("F4 — вся схема"),
            "клипа нет — хинт не нужен"
        );
    }

    #[test]
    fn mermaid_tab_width_capped_with_f4_hint() {
        let mut app = test_app();
        app.screen = Screen::Chat;
        // Арт 120 колонок при терминале 100: панель упирается в кап 60% (60),
        // остаток клипается, но вкладка подсказывает F4.
        let wide = format!("┌{}┐", "─".repeat(118));
        app.panels.mermaid = format!("{wide}\n└{}┘", "─".repeat(118));
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("terminal");
        terminal.draw(|f| app.render(f)).expect("draw");
        let text = buffer_text(&terminal);
        assert!(!text.contains(&wide), "за капом — клип:\n{text}");
        assert!(
            text.contains("F4 — вся схема"),
            "хинт про полноэкранный просмотр:\n{text}"
        );
    }

    #[test]
    fn mermaid_tab_clips_instead_of_wrapping() {
        let mut app = test_app();
        app.screen = Screen::Chat;
        // Строка арта 41 колонка — шире вкладки даже после авто-расширения
        // (терминал 60 → кап 36, внутренняя ширина 34).
        app.panels.mermaid =
            "┌──────────┐     ┌──────────┐     ┌─────┐\n│    A     │─────▶│    B     │     │  C  │"
                .into();
        let mut terminal = Terminal::new(TestBackend::new(60, 14)).expect("terminal");
        terminal.draw(|f| app.render(f)).expect("draw");
        let text = buffer_text(&terminal);
        // Перенос порвал бы строку и вынес бы третий блок «┌─────┐»
        // на отдельную строку; при клипе он не появляется никогда.
        assert!(
            text.contains("┌──────────┐     ┌──────────┐"),
            "начало строки арта целое:\n{text}"
        );
        assert!(
            !text.contains("┌─────┐"),
            "третий блок клипнут, а не перенесён:\n{text}"
        );
        assert!(text.contains("─▶"), "стрелка цела:\n{text}");
    }

    #[test]
    fn fatal_screen_shows_error_and_hint() {
        let mut app = test_app();
        app.screen = Screen::Fatal("default_model отсутствует".into());
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("terminal");
        terminal.draw(|f| app.render(f)).expect("draw");
        let text = buffer_text(&terminal);
        assert!(text.contains("Ошибка инициализации"));
        assert!(text.contains("default_model отсутствует"));
        assert!(text.contains("q — выход"));
    }

    #[test]
    fn narrow_terminal_does_not_panic() {
        let mut app = test_app();
        app.screen = Screen::Chat;
        let mut terminal = Terminal::new(TestBackend::new(20, 6)).expect("terminal");
        terminal.draw(|f| app.render(f)).expect("draw");
    }

    #[test]
    fn art_detection_ranges() {
        assert!(is_art("┌──┐"));
        assert!(is_art("─▶"));
        assert!(!is_art("обычный текст"));
        assert!(!is_art("▎ гуттер пользователя"));
        assert!(!is_art("✓ галка инструмента"));
    }

    #[test]
    fn status_bar_shows_running_subagents_count() {
        use crate::subagent::{SubagentRegistry, SubagentTask, TaskStatus};
        let _tmp = tempfile::tempdir().expect("tmp");
        let registry = SubagentRegistry::new();
        registry.insert(SubagentTask {
            id: "sa-t-00".into(),
            agent: "general".into(),
            task: "разведка".into(),
            status: TaskStatus::Running,
            report: String::new(),
            started_at: "2026-08-15".into(),
            finished_at: None,
        });
        let ctx = test_app().tool_ctx.clone().with_subagents(registry);
        let mut app = test_app();
        app.tool_ctx = ctx;
        app.screen = Screen::Chat;
        let mut terminal = Terminal::new(TestBackend::new(110, 24)).expect("terminal");
        terminal.draw(|f| app.render(f)).expect("draw");
        let text = buffer_text(&terminal);
        assert!(
            text.contains("субагенты: 1"),
            "индикатор в статус-баре:\n{text}"
        );
        assert!(app.needs_tick(), "тики идут, пока крутятся субагенты");
    }

    /// Цвет заливки первой ячейки шкалы контекста в буфере (None — не найдена).
    fn gauge_color(term: &Terminal<TestBackend>) -> Option<ratatui::style::Color> {
        let buf = term.backend().buffer();
        let area = buf.area;
        for y in 0..area.height {
            for x in 0..area.width {
                let cell = &buf[(x, y)];
                if cell.symbol() == "▰" {
                    return Some(cell.fg);
                }
            }
        }
        None
    }

    #[test]
    fn status_bar_shows_context_gauge() {
        use crate::tui::app::testing::set_context_usage;
        let mut app = test_app();
        app.screen = Screen::Chat;
        // Половина окна 1M: зелёная шкала, подпись «500.0k/1.0M … 50%».
        set_context_usage(&mut app, 500_000, 1_000_000);
        let mut terminal = Terminal::new(TestBackend::new(120, 24)).expect("terminal");
        terminal.draw(|f| app.render(f)).expect("draw");
        let text = buffer_text(&terminal);
        assert!(text.contains("500.0k/1.0M"), "токены окна:\n{text}");
        assert!(text.contains("▰▰▰▰▱▱▱▱ 50%"), "шкала 50%:\n{text}");
        assert_eq!(
            gauge_color(&terminal),
            Some(ratatui::style::Color::Rgb(0x9e, 0xce, 0x6a)),
            "до L1 шкала зелёная"
        );
    }

    #[test]
    fn status_bar_gauge_turns_red_near_l3_threshold() {
        use crate::tui::app::testing::set_context_usage;
        let mut app = test_app();
        app.screen = Screen::Chat;
        // 96% окна: за порогом L3 (95%) — шкала красная.
        set_context_usage(&mut app, 960_000, 1_000_000);
        let mut terminal = Terminal::new(TestBackend::new(120, 24)).expect("terminal");
        terminal.draw(|f| app.render(f)).expect("draw");
        let text = buffer_text(&terminal);
        assert!(text.contains("96%"), "процент заполнения:\n{text}");
        assert_eq!(
            gauge_color(&terminal),
            Some(ratatui::style::Color::Rgb(0xf7, 0x76, 0x8e)),
            "за L3 шкала красная"
        );
    }

    #[test]
    fn status_bar_gauge_orange_between_l1_and_l3() {
        use crate::tui::app::testing::set_context_usage;
        let mut app = test_app();
        app.screen = Screen::Chat;
        // 80% окна: между L1 (70%) и L3 (95%) — шкала оранжевая.
        set_context_usage(&mut app, 800_000, 1_000_000);
        let mut terminal = Terminal::new(TestBackend::new(120, 24)).expect("terminal");
        terminal.draw(|f| app.render(f)).expect("draw");
        assert_eq!(
            gauge_color(&terminal),
            Some(ratatui::style::Color::Rgb(0xff, 0x9e, 0x64)),
            "между L1 и L3 шкала оранжевая"
        );
    }

    #[test]
    fn queue_overlay_in_dialog_area_above_input() {
        use crate::tui::app::testing::set_thinking;
        let mut app = test_app();
        app.screen = Screen::Chat;
        set_thinking(&mut app, true);
        app.queue.push_back("первое в очереди".into());
        app.queue.push_back("второе\nмногострочное".into());
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("terminal");
        terminal.draw(|f| app.render(f)).expect("draw");
        let text = buffer_text(&terminal);
        assert!(text.contains("очередь · 2"), "заголовок карточки:\n{text}");
        assert!(
            text.contains("▶ 1. первое в очереди"),
            "первое — следующее на запуск:\n{text}"
        );
        // Многострочное сообщение — первой строкой с маркером ↵.
        assert!(
            text.contains("• 2. второе ↵"),
            "вторая строка с маркером переноса:\n{text}"
        );
        // Карточка очереди — В окне логов: выше поля ввода (строки с «❯»).
        let q_row = text.lines().position(|l| l.contains("▶ 1.")).expect("▶");
        let i_row = text.lines().position(|l| l.contains('❯')).expect("❯");
        assert!(
            q_row < i_row,
            "очередь ({q_row}) над вводом ({i_row}):\n{text}"
        );
        // В поле ввода строк очереди больше нет — оно однострочное по дефолту.
        let input_rows = text
            .lines()
            .skip(i_row)
            .take_while(|l| !l.contains("F1"))
            .count();
        assert!(
            input_rows <= 4,
            "поле ввода не раздувается очередью:\n{text}"
        );
    }

    #[test]
    fn wrap_input_cases() {
        // Пустой ввод: одна строка, курсор за prompt'ом (ширина 2).
        let (rows, cr, cx) = wrap_input("", 0, 20);
        assert_eq!(rows, vec![String::new()]);
        assert_eq!((cr, cx), (0, 2));
        // Жёсткий перенос: две строки, курсор в конце второй.
        let (rows, cr, cx) = wrap_input("ab\ncd", 5, 20);
        assert_eq!(rows, vec!["ab".to_string(), "cd".to_string()]);
        assert_eq!((cr, cx), (1, 4));
        // Курсор в середине первой строки.
        let (_, cr, cx) = wrap_input("ab\ncd", 1, 20);
        assert_eq!((cr, cx), (0, 3));
        // Перенос по ширине: avail = 10-2 = 8, «0123456789» → 8 + 2.
        let (rows, cr, cx) = wrap_input("0123456789", 10, 10);
        assert_eq!(rows, vec!["01234567".to_string(), "89".to_string()]);
        assert_eq!((cr, cx), (1, 4));
    }

    #[test]
    fn input_grows_with_multiline_text() {
        let mut app = test_app();
        app.screen = Screen::Chat;
        app.input.set_text("первая строка\nвторая строка".into());
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("terminal");
        terminal.draw(|f| app.render(f)).expect("draw");
        let text = buffer_text(&terminal);
        assert!(text.contains("первая строка"), "первая:\n{text}");
        assert!(text.contains("вторая строка"), "вторая:\n{text}");
        // Обе строки — внутри поля ввода: над статус-баром не более 4 строк.
        let first = text
            .lines()
            .position(|l| l.contains("первая строка"))
            .unwrap();
        let second = text
            .lines()
            .position(|l| l.contains("вторая строка"))
            .unwrap();
        assert_eq!(second, first + 1, "строки ввода идут подряд:\n{text}");
    }

    #[test]
    fn selection_highlights_cells_with_selection_style() {
        let mut app = test_app();
        app.screen = Screen::Chat;
        app.push_block(ChatBlock::User("скопируй меня мышкой".into()));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("terminal");
        terminal.draw(|f| app.render(f)).expect("draw");
        let inner = app.dialog_inner.expect("inner после рендера");
        // Выделяем 5 ячеек на строке сообщения (вторая строка контента —
        // после заголовка «вы»): колонки 2..6 от начала области.
        let row = inner.y + 1;
        app.selection = Some(((inner.x + 2, row), (inner.x + 6, row)));
        terminal.draw(|f| app.render(f)).expect("draw");
        let buf = terminal.backend().buffer();
        let sel_bg = ratatui::style::Color::Rgb(0x28, 0x34, 0x57);
        assert_eq!(
            buf[(inner.x + 2, row)].bg,
            sel_bg,
            "первая ячейка подсвечена"
        );
        assert_eq!(buf[(inner.x + 6, row)].bg, sel_bg, "пятая подсвечена");
        assert_ne!(
            buf[(inner.x + 7, row)].bg,
            sel_bg,
            "шестая — уже вне выделения"
        );
    }

    #[test]
    fn fmt_tokens_human_readable() {
        assert_eq!(fmt_tokens(0), "0");
        assert_eq!(fmt_tokens(999), "999");
        assert_eq!(fmt_tokens(1_500), "1.5k");
        assert_eq!(fmt_tokens(12_345), "12.3k");
        assert_eq!(fmt_tokens(1_000_000), "1.0M");
        assert_eq!(fmt_tokens(6_000_000), "6.0M");
    }

    /// Длинный диалог: 20 блоков × ~3 строки — прокрутка гарантирована.
    fn long_dialog_app() -> App {
        let mut app = test_app();
        app.screen = Screen::Chat;
        for i in 0..20 {
            app.push_block(ChatBlock::User(format!("сообщение {i}")));
        }
        app
    }

    #[test]
    fn dialog_shows_scrollbar_and_jump_button_when_scrolled() {
        let mut app = long_dialog_app();
        app.scroll_by(10);
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("terminal");
        terminal.draw(|f| app.render(f)).expect("draw");
        let text = buffer_text(&terminal);
        assert!(text.contains('█'), "бегунок скроллбара:\n{text}");
        assert!(text.contains(" ▼ "), "кнопка к свежему ответу:\n{text}");
        assert!(app.jump_btn.is_some(), "область кнопки выставлена рендером");
        // Кнопка — в правом нижнем углу диалога (диалог 66 колонок при 100).
        let btn = app.jump_btn.expect("кнопка");
        assert_eq!(btn.width, 3);
        assert!(btn.x >= 60 && btn.y >= 20, "позиция кнопки: {btn:?}");
    }

    #[test]
    fn no_jump_button_at_bottom_and_no_scrollbar_when_short() {
        // Диалог короче экрана: ни скроллбара, ни кнопки.
        let mut app = test_app();
        app.screen = Screen::Chat;
        app.push_block(ChatBlock::User("привет".into()));
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("terminal");
        terminal.draw(|f| app.render(f)).expect("draw");
        let text = buffer_text(&terminal);
        assert!(!text.contains('█'), "бегунка быть не должно:\n{text}");
        assert!(!text.contains(" ▼ "), "кнопки быть не должно:\n{text}");
        assert!(app.jump_btn.is_none());

        // Длинный диалог, но у дна: скроллбар есть, кнопки нет.
        let mut app = long_dialog_app();
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("terminal");
        terminal.draw(|f| app.render(f)).expect("draw");
        let text = buffer_text(&terminal);
        assert!(text.contains('█'), "бегунок виден у дна:\n{text}");
        assert!(!text.contains(" ▼ "), "у дна кнопка скрыта:\n{text}");
        assert!(app.jump_btn.is_none());
    }

    #[test]
    fn ask_modal_shows_question_options_and_hints() {
        let mut app = test_app();
        app.screen = Screen::Chat;
        app.ask = Some(crate::tui::app::AskState {
            question: "Какой брокер сообщений выбрать?".into(),
            options: vec![
                crate::tool::AskOption {
                    label: "Kafka".into(),
                    description: "масштаб, но тяжёлый".into(),
                },
                crate::tool::AskOption {
                    label: "NATS".into(),
                    description: "лёгкий, без хранения".into(),
                },
            ],
            recommended: Some("Kafka".into()),
            selected: 0,
            reply: None,
            kind: crate::tui::app::AskKind::Tool,
        });
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("terminal");
        terminal.draw(|f| app.render(f)).expect("draw");
        let text = buffer_text(&terminal);
        for needle in [
            "решение за вами",
            "Какой брокер сообщений выбрать?",
            "1. Kafka",
            "★",
            "2. NATS",
            "лёгкий, без хранения",
            "Enter",
            "Esc",
        ] {
            assert!(text.contains(needle), "не найдено «{needle}»:\n{text}");
        }
    }

    #[test]
    fn viewer_fullscreen_pans_wide_art_horizontally() {
        let mut app = test_app();
        app.screen = Screen::Chat;
        // Арт 5+100+5 колонок — заведомо шире терминала.
        let art = format!("LEFT{}RIGHT", "─".repeat(100));
        app.panels.mermaid = art;
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).expect("terminal");

        // Без панорамы: левый край виден, правый — клипнут.
        app.viewer = Some(crate::tui::app::ViewerState {
            scroll_x: 0,
            scroll_y: 0,
        });
        terminal.draw(|f| app.render(f)).expect("draw");
        let text = buffer_text(&terminal);
        assert!(
            text.contains("на весь экран"),
            "заголовок просмотрщика:\n{text}"
        );
        assert!(text.contains("LEFT"), "левый край при scroll_x=0:\n{text}");
        assert!(!text.contains("RIGHT"), "правый край клипнут:\n{text}");

        // Панорама вправо: левый край ушёл, правый показался
        // (scroll_x=60 клампится к max 109-78=31 — проверяем индикатор без числа).
        app.viewer = Some(crate::tui::app::ViewerState {
            scroll_x: 60,
            scroll_y: 0,
        });
        terminal.draw(|f| app.render(f)).expect("draw");
        let text = buffer_text(&terminal);
        assert!(!text.contains("LEFT"), "левый край за окном:\n{text}");
        assert!(
            text.contains("RIGHT"),
            "правый край показан панорамой:\n{text}"
        );
        assert!(text.contains("→"), "индикатор позиции:\n{text}");
    }

    #[test]
    fn hclip_drops_columns_unicode_safely() {
        let line = Line::from(Span::styled("┌──┐abcdef".to_string(), Style::default()));
        let clipped = hclip_line(&line, 4);
        let text: String = clipped.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "abcdef", "срезаны ровно 4 display-колонки");
        let clipped = hclip_line(&line, 0);
        let text: String = clipped.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "┌──┐abcdef", "нулевой оффсет — строка цела");
    }
}
