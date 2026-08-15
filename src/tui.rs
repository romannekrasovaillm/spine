//! TUI харнесса: ratatui + crossterm, акторная архитектура.
//!
//! КОНТРАКТ (владелец: агент `tui-cron`):
//! - [`run`] — полноэкранный TUI: левая колонка (команды/сессия), центр —
//!   чат с агентом (стриминг дельт, markdown-lite подсветка, статус вызовов
//!   инструментов), правая колонка с вкладками (Mermaid / Рубрика / Знания),
//!   статус-бар (модель, токены, cwd); ASCII-баннер при старте
//!   (assets::BANNER);
//! - палитра Tokyo Night: bg #1a1b26, fg #c0caf5, cyan #7dcfff,
//!   purple #bb9af7, green #9ece6a, orange #ff9e64, red #f7768e, muted #565f89;
//! - событийная модель: crossterm EventStream + mpsc-каналы (агентные
//!   события AgentEvent), bounded-каналы, graceful shutdown по Ctrl-C/q/Esc,
//!   восстановление терминала при панике (RAII-гард + panic hook);
//! - ввод: история команд (Up/Down), автодополнение слэш-команд по Tab.
//!
//! Реализация: [`app::App`] владеет всем состоянием в event loop (без
//! `Arc<Mutex>`); ход агента и слэш-команды выполняются в `tokio::spawn`,
//! сессия возвращается сообщением [`app::AppMessage`]; события
//! [`crate::agent::AgentEvent`] форвардятся через bounded mpsc(64).

use std::io;
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::event::{Event, EventStream, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;

use crate::config::Config;
use crate::error::{HarnessError, Result};

pub(crate) mod app;
mod render;
#[cfg(test)]
pub(crate) mod shot;
mod text;
mod theme;

use app::{App, AppMessage};

/// Ёмкость канала сообщений приложения (bounded — backpressure).
const APP_CHANNEL_CAP: usize = 256;
/// Период перерисовки спиннера ожидания модели.
const SPINNER_INTERVAL: Duration = Duration::from_millis(120);

/// Запускает TUI. Блокируется до выхода пользователя (`q`, Ctrl-C, Esc).
///
/// # Errors
/// Терминал недоступен (нет TTY), ошибка отрисовки кадра.
pub async fn run(cfg: Arc<Config>) -> Result<()> {
    let _guard = TerminalGuard::enter()?;
    install_panic_hook();
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))
        .map_err(|e| HarnessError::Tui(format!("инициализация терминала: {e}")))?;
    terminal
        .clear()
        .map_err(|e| HarnessError::Tui(format!("очистка экрана: {e}")))?;

    let mut app = App::build(cfg).await;
    let (msg_tx, mut msg_rx) = mpsc::channel::<AppMessage>(APP_CHANNEL_CAP);
    app.attach(msg_tx);

    let mut keys = EventStream::new();
    let mut ticker = tokio::time::interval(SPINNER_INTERVAL);
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    loop {
        if app.should_quit {
            break;
        }
        terminal
            .draw(|f| app.render(f))
            .map_err(|e| HarnessError::Tui(format!("отрисовка: {e}")))?;
        tokio::select! {
            maybe_event = keys.next() => {
                match maybe_event {
                    Some(Ok(Event::Key(key))) => {
                        if key.kind != KeyEventKind::Release {
                            app.handle_key(key);
                        }
                    }
                    // Колесо мыши — прокрутка диалога.
                    Some(Ok(Event::Mouse(mouse))) => app.handle_mouse(mouse),
                    // Resize/фокус — просто перерисовываемся на следующем круге.
                    Some(Ok(_)) => {}
                    // Поток ввода закрылся или сломался — выходим.
                    Some(Err(_)) | None => app.should_quit = true,
                }
            }
            msg = msg_rx.recv() => {
                if let Some(m) = msg {
                    app.handle_message(m);
                }
                // None: отправителей нет и не будет — ждём только клавиши.
            }
            _ = &mut ctrl_c => app.should_quit = true,
            _ = ticker.tick(), if app.needs_tick() => app.tick(),
        }
    }

    app.shutdown().await;
    Ok(())
}

/// RAII-гард терминала: при Drop покидает alternate screen и выключает
/// raw mode — терминал восстанавливается при любом выходе из [`run`].
struct TerminalGuard;

impl TerminalGuard {
    /// Входит в raw mode + alternate screen + захват мыши (колесо — скролл).
    ///
    /// # Errors
    /// Терминал недоступен (нет TTY, запуск в пайпе/CI).
    fn enter() -> Result<Self> {
        enable_raw_mode().map_err(|e| {
            HarnessError::Tui(format!("raw mode недоступен (нужен TTY): {e}"))
        })?;
        if let Err(e) = execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture) {
            let _ = disable_raw_mode();
            return Err(HarnessError::Tui(format!("alternate screen: {e}")));
        }
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
        let _ = disable_raw_mode();
    }
}

/// Panic hook: восстанавливает терминал перед печатью паники,
/// затем передаёт управление исходному обработчику.
fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
        let _ = disable_raw_mode();
        original(info);
    }));
}
