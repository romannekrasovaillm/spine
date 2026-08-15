//! Палитра Tokyo Night и базовые стили TUI.

use ratatui::style::{Color, Modifier, Style};

/// Цветовая тема TUI (Tokyo Night).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Theme {
    /// Фон (#1a1b26).
    pub(crate) bg: Color,
    /// Основной текст (#c0caf5).
    pub(crate) fg: Color,
    /// Акцент: cyan (#7dcfff).
    pub(crate) cyan: Color,
    /// Акцент: purple (#bb9af7).
    pub(crate) purple: Color,
    /// Успех: green (#9ece6a).
    pub(crate) green: Color,
    /// Предупреждение: orange (#ff9e64).
    pub(crate) orange: Color,
    /// Ошибка: red (#f7768e).
    pub(crate) red: Color,
    /// Приглушённый текст: muted (#565f89).
    pub(crate) muted: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            bg: Color::Rgb(0x1a, 0x1b, 0x26),
            fg: Color::Rgb(0xc0, 0xca, 0xf5),
            cyan: Color::Rgb(0x7d, 0xcf, 0xff),
            purple: Color::Rgb(0xbb, 0x9a, 0xf7),
            green: Color::Rgb(0x9e, 0xce, 0x6a),
            orange: Color::Rgb(0xff, 0x9e, 0x64),
            red: Color::Rgb(0xf7, 0x76, 0x8e),
            muted: Color::Rgb(0x56, 0x5f, 0x89),
        }
    }
}

impl Theme {
    /// Базовый стиль: основной текст на фоне.
    pub(crate) fn base(&self) -> Style {
        Style::default().fg(self.fg).bg(self.bg)
    }

    /// Рамки блоков.
    pub(crate) fn border(&self) -> Style {
        Style::default().fg(self.muted).bg(self.bg)
    }

    /// Приглушённый текст (подсказки, сводки инструментов).
    pub(crate) fn muted(&self) -> Style {
        Style::default().fg(self.muted).bg(self.bg)
    }

    /// Акцент cyan (команды, prompt).
    pub(crate) fn accent(&self) -> Style {
        Style::default().fg(self.cyan).bg(self.bg)
    }

    /// Заголовок (markdown `#`, имена блоков): жирный cyan.
    pub(crate) fn heading(&self) -> Style {
        self.accent().add_modifier(Modifier::BOLD)
    }

    /// Акцент purple (команды system-блоков, буллеты).
    pub(crate) fn purple(&self) -> Style {
        Style::default().fg(self.purple).bg(self.bg)
    }

    /// Код-блок markdown: мягкая панель Tokyo Night (bg_highlight #24283b,
    /// текст #a9b1d6) — читается как редактор, без жёсткой инверсии.
    pub(crate) fn code(&self) -> Style {
        Style::default()
            .fg(Color::Rgb(0xa9, 0xb1, 0xd6))
            .bg(Color::Rgb(0x24, 0x28, 0x3b))
    }

    /// Текст ошибки.
    pub(crate) fn error(&self) -> Style {
        Style::default().fg(self.red).bg(self.bg)
    }

    /// Градиент cyan→purple по `t ∈ [0,1]` (баннер сплэш-экрана).
    pub(crate) fn gradient(&self, t: f32) -> Color {
        let t = t.clamp(0.0, 1.0);
        let lerp = |a: u8, b: u8| {
            (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8
        };
        // Токены палитры: cyan (#7dcfff) → purple (#bb9af7).
        Color::Rgb(
            lerp(0x7d, 0xbb),
            lerp(0xcf, 0x9a),
            lerp(0xff, 0xf7),
        )
    }

    /// Бейдж (инверсия на акценте): имя модели в статус-баре.
    pub(crate) fn badge(&self) -> Style {
        Style::default()
            .fg(self.bg)
            .bg(self.cyan)
            .add_modifier(Modifier::BOLD)
    }

    /// ASCII-арт (mermaid-рендеры): зелёный отделяет схему от прозы.
    pub(crate) fn art(&self) -> Style {
        Style::default().fg(self.green).bg(self.bg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokyo_night_palette_is_exact() {
        let t = Theme::default();
        assert_eq!(t.bg, Color::Rgb(0x1a, 0x1b, 0x26));
        assert_eq!(t.fg, Color::Rgb(0xc0, 0xca, 0xf5));
        assert_eq!(t.cyan, Color::Rgb(0x7d, 0xcf, 0xff));
        assert_eq!(t.purple, Color::Rgb(0xbb, 0x9a, 0xf7));
        assert_eq!(t.green, Color::Rgb(0x9e, 0xce, 0x6a));
        assert_eq!(t.orange, Color::Rgb(0xff, 0x9e, 0x64));
        assert_eq!(t.red, Color::Rgb(0xf7, 0x76, 0x8e));
        assert_eq!(t.muted, Color::Rgb(0x56, 0x5f, 0x89));
    }

    #[test]
    fn gradient_endpoints_and_middle() {
        let t = Theme::default();
        assert_eq!(t.gradient(0.0), t.cyan, "левый конец — cyan");
        assert_eq!(t.gradient(1.0), t.purple, "правый конец — purple");
        assert_eq!(t.gradient(-1.0), t.cyan, "кламп снизу");
        assert_eq!(t.gradient(2.0), t.purple, "кламп сверху");
        let mid = t.gradient(0.5);
        let Color::Rgb(r, g, b) = mid else {
            panic!("градиент — Rgb");
        };
        assert!((r, g, b) != (0x7d, 0xcf, 0xff) && (r, g, b) != (0xbb, 0x9a, 0xf7));
    }
}
