//! Текстовые утилиты TUI: markdown-lite, перенос строк по unicode-ширине,
//! кандидаты автодополнения слэш-команд.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::agent::slash;

use super::theme::Theme;

/// Разбирает текст в markdown-lite линии: заголовки `#`..`####`, код-блоки
/// `` ``` ``, буллеты `- `/`* `, инлайн `**жирный**`.
pub(crate) fn markdown_lines(text: &str, theme: &Theme) -> Vec<Line<'static>> {
    let base = theme.base();
    let mut out = Vec::new();
    let mut in_code = false;
    for raw in text.lines() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if line.trim_start().starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            out.push(Line::from(Span::styled(format!(" {line}"), theme.code())));
            continue;
        }
        let trimmed = line.trim_start();
        let indent = &line[..line.len() - trimmed.len()];
        if let Some(heading) = strip_heading(trimmed) {
            out.push(Line::from(Span::styled(
                heading.to_string(),
                theme.heading(),
            )));
        } else if let Some(rest) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            let mut spans = vec![Span::styled(format!("{indent}• "), theme.purple())];
            spans.extend(inline_spans(rest, base));
            out.push(Line::from(spans));
        } else if let Some(rest) = trimmed.strip_prefix("> ") {
            // Цитата: фиолетовый гуттер + приглушённый курсив.
            let mut spans = vec![Span::styled(format!("{indent}▌ "), theme.purple())];
            spans.extend(inline_spans(
                rest,
                theme.muted().add_modifier(Modifier::ITALIC),
            ));
            out.push(Line::from(spans));
        } else if trimmed.starts_with('|') && trimmed.ends_with('|') {
            // Таблица: разделительные строки пропускаем, данные — с тонкими
            // приглушёнными разделителями ячеек.
            if is_table_separator(trimmed) {
                continue;
            }
            let mut spans = Vec::new();
            let cells: Vec<&str> = trimmed
                .trim_matches('|')
                .split('|')
                .map(str::trim)
                .collect();
            for (i, cell) in cells.iter().enumerate() {
                if i > 0 {
                    spans.push(Span::styled(" │ ".to_string(), theme.muted()));
                }
                spans.extend(inline_spans(cell, base));
            }
            out.push(Line::from(spans));
        } else {
            out.push(Line::from(inline_spans(line, base)));
        }
    }
    out
}

/// Строка-разделитель markdown-таблицы (`|---|---|`, `|:-|:-:|`)?
fn is_table_separator(line: &str) -> bool {
    line.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ')) && line.contains('-')
}

/// Срезает маркер заголовка `# `…`#### `, возвращает текст заголовка.
fn strip_heading(line: &str) -> Option<&str> {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    if (1..=4).contains(&hashes) && line[hashes..].starts_with(' ') {
        Some(line[hashes + 1..].trim_start())
    } else {
        None
    }
}

/// Инлайн-разбор `**жирный**` (простой парсер: чередование по `**`).
fn inline_spans(text: &str, base: Style) -> Vec<Span<'static>> {
    let bold = base.add_modifier(Modifier::BOLD);
    let mut spans = Vec::new();
    let mut rest = text;
    let mut is_bold = false;
    while let Some(pos) = rest.find("**") {
        if pos > 0 {
            spans.push(Span::styled(
                rest[..pos].to_string(),
                if is_bold { bold } else { base },
            ));
        }
        is_bold = !is_bold;
        rest = &rest[pos + 2..];
    }
    if !rest.is_empty() || spans.is_empty() {
        spans.push(Span::styled(
            rest.to_string(),
            if is_bold { bold } else { base },
        ));
    }
    spans
}

/// Переносит стилизованную линию по ширине с разрывом на границах слов
/// (ширины — по unicode-width; широкие символы CJK/эмодзи учитываются).
/// Слово длиннее строки разрывается жёстко, по символам.
pub(crate) fn wrap_line(line: &Line<'static>, width: usize) -> Vec<Line<'static>> {
    let mut w = Wrapper {
        lines: vec![Vec::new()],
        width: width.max(1),
        cur: 0,
    };
    for span in &line.spans {
        for piece in span.content.split_inclusive(' ') {
            w.push_piece(piece, span.style);
        }
    }
    w.lines.into_iter().map(Line::from).collect()
}

/// Накопитель перенесённых строк.
struct Wrapper {
    /// Готовые строки (последняя — накапливаемая).
    lines: Vec<Vec<Span<'static>>>,
    /// Целевая ширина строки.
    width: usize,
    /// Текущая ширина последней строки.
    cur: usize,
}

impl Wrapper {
    /// Начинает новую строку.
    fn new_line(&mut self) {
        self.lines.push(Vec::new());
        self.cur = 0;
    }

    /// Добавляет кусок текста в текущую строку.
    fn push_span(&mut self, text: String, style: Style) {
        if text.is_empty() {
            return;
        }
        self.cur += UnicodeWidthStr::width(text.as_str());
        if let Some(last) = self.lines.last_mut() {
            last.push(Span::styled(text, style));
        }
    }

    /// Добавляет слово (с возможным хвостовым пробелом), перенося при нехватке места.
    fn push_piece(&mut self, piece: &str, style: Style) {
        let pw = UnicodeWidthStr::width(piece);
        if pw == 0 {
            return;
        }
        if self.cur > 0 && self.cur + pw > self.width {
            self.new_line();
            // Ведущий пробел новой строки не нужен.
            if piece.trim().is_empty() {
                return;
            }
        }
        if pw <= self.width {
            self.push_span(piece.to_string(), style);
        } else {
            self.push_long(piece, style);
        }
    }

    /// Жёсткий разрыв слова длиннее строки (по символам с учётом ширины).
    fn push_long(&mut self, piece: &str, style: Style) {
        debug_assert_eq!(self.cur, 0, "длинное слово начинается с новой строки");
        let mut buf = String::new();
        let mut bw = 0usize;
        for ch in piece.chars() {
            let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
            if bw + cw > self.width && !buf.is_empty() {
                self.push_span(std::mem::take(&mut buf), style);
                self.new_line();
                bw = 0;
            }
            buf.push(ch);
            bw += cw;
        }
        self.push_span(buf, style);
    }
}

/// Кандидаты автодополнения слэш-команды по префиксу (`/me` → `/mermaid`).
/// Дополняется только первое слово ввода; не-слэш ввод не дополняется.
pub(crate) fn completion_candidates(input: &str) -> Vec<&'static str> {
    if !input.starts_with('/') || input.chars().any(char::is_whitespace) {
        return Vec::new();
    }
    slash::catalog()
        .into_iter()
        .filter_map(|(usage, _desc)| usage.split_whitespace().next())
        .filter(|cmd| cmd.starts_with(input) && *cmd != input)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    /// Собирает текст линии из спанов (для сравнений).
    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn wrap_breaks_at_word_boundaries() {
        let line = Line::from("привет мир это тест");
        let wrapped = wrap_line(&line, 11);
        let texts: Vec<String> = wrapped
            .iter()
            .map(|l| line_text(l).trim_end().to_string())
            .collect();
        assert_eq!(texts, vec!["привет мир", "это тест"]);
    }

    #[test]
    fn wrap_hard_splits_long_words() {
        let line = Line::from("абвгдежзикл");
        let wrapped = wrap_line(&line, 4);
        let texts: Vec<String> = wrapped.iter().map(line_text).collect();
        assert_eq!(texts, vec!["абвг", "дежз", "икл"]);
    }

    #[test]
    fn wrap_respects_wide_chars() {
        // Каждый иероглиф — ширина 2: «日本語» = 6 колонок.
        let line = Line::from("日本語");
        let wrapped = wrap_line(&line, 4);
        let texts: Vec<String> = wrapped.iter().map(line_text).collect();
        assert_eq!(texts, vec!["日本", "語"]);
    }

    #[test]
    fn wrap_preserves_span_styles() {
        let line = Line::from(Span::styled("раз два", Style::default().fg(Color::Red)));
        let wrapped = wrap_line(&line, 4);
        assert_eq!(wrapped.len(), 2);
        assert!(
            wrapped
                .iter()
                .all(|l| l.spans.iter().all(|s| s.style.fg == Some(Color::Red)))
        );
    }

    #[test]
    fn wrap_empty_line_stays_single() {
        let wrapped = wrap_line(&Line::from(""), 10);
        assert_eq!(wrapped.len(), 1);
    }

    #[test]
    fn markdown_heading_is_bold_cyan() {
        let theme = Theme::default();
        let lines = markdown_lines("# Заголовок\nтекст", &theme);
        assert_eq!(lines.len(), 2);
        let head = &lines[0].spans[0];
        assert_eq!(head.content.as_ref(), "Заголовок");
        assert_eq!(head.style.fg, Some(theme.cyan));
        assert!(head.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn markdown_code_block_is_soft_panel() {
        let theme = Theme::default();
        let lines = markdown_lines("до\n```\nlet x = 1;\n```\nпосле", &theme);
        assert_eq!(lines.len(), 3);
        let code = &lines[1].spans[0];
        // Мягкая панель Tokyo Night, не инверсия fg/bg.
        assert_eq!(code.style.bg, Some(Color::Rgb(0x24, 0x28, 0x3b)));
        assert_eq!(code.style.fg, Some(Color::Rgb(0xa9, 0xb1, 0xd6)));
        assert_eq!(line_text(&lines[0]), "до");
        assert_eq!(line_text(&lines[2]), "после");
    }

    #[test]
    fn markdown_table_renders_cells_and_skips_separator() {
        let lines = markdown_lines(
            "| Паттерн | Когда |\n|---|---|\n| сага | распределённая транзакция |",
            &Theme::default(),
        );
        assert_eq!(lines.len(), 2, "разделитель пропущен: {lines:?}");
        assert_eq!(line_text(&lines[0]), "Паттерн │ Когда");
        assert_eq!(line_text(&lines[1]), "сага │ распределённая транзакция");
    }

    #[test]
    fn markdown_quote_gets_gutter() {
        let lines = markdown_lines("> важная мысль", &Theme::default());
        let text = line_text(&lines[0]);
        assert!(text.starts_with("▌ "), "{text}");
        assert!(text.contains("важная мысль"));
    }

    #[test]
    fn markdown_bullet_becomes_dot() {
        let lines = markdown_lines("- пункт списка\n* ещё пункт", &Theme::default());
        assert!(line_text(&lines[0]).starts_with("• "));
        assert!(line_text(&lines[1]).starts_with("• "));
    }

    #[test]
    fn markdown_bold_inline_splits_spans() {
        let lines = markdown_lines("это **жирный** текст", &Theme::default());
        let spans = &lines[0].spans;
        assert_eq!(spans.len(), 3);
        assert!(!spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(spans[1].content.as_ref(), "жирный");
        assert!(spans[1].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(spans[2].content.as_ref(), " текст");
        assert!(!spans[2].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn completion_finds_mermaid() {
        assert_eq!(completion_candidates("/me"), vec!["/mermaid", "/memory"]);
    }

    #[test]
    fn completion_ignores_non_slash_and_args() {
        assert!(completion_candidates("привет").is_empty());
        assert!(completion_candidates("/rubric run").is_empty());
        assert!(completion_candidates("").is_empty());
    }

    #[test]
    fn completion_lists_all_on_bare_slash() {
        assert!(completion_candidates("/").len() >= 15);
    }
}
