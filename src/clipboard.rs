//! Копирование в системный буфер обмена без новых зависимостей.
//!
//! КОНТРАКТ (владелец: агент `tui`):
//! - [`copy`] пытается механизмы по порядку: `wl-copy` (Wayland), `xclip`,
//!   `xsel` (X11), и в конце — OSC 52 (escape-последовательность в /dev/tty,
//!   работает в kitty/alacritty/wezterm/foot и поверх SSH);
//! - возвращает имя сработавшего механизма для статус-сообщения;
//! - ошибка — только если недоступны ВСЕ механизмы (подсказка, что установить).

use std::io::Write;
use std::process::{Command, Stdio};

use crate::error::{HarnessError, Result};

/// Копирует текст в буфер обмена; возвращает имя механизма.
///
/// # Errors
/// Ни один механизм недоступен/не сработал (перечень попыток в тексте).
pub fn copy(text: &str) -> Result<&'static str> {
    let mut tried = Vec::new();
    // Внешние утилиты: stdin → буфер. `wl-copy` живёт в Wayland-сессиях,
    // `xclip`/`xsel` — в X11; просто пробуем по очереди.
    for (prog, argv, name) in [
        ("wl-copy", &[][..], "wl-copy"),
        ("xclip", &["-selection", "clipboard"][..], "xclip"),
        ("xsel", &["--clipboard", "--input"][..], "xsel"),
    ] {
        match pipe_to(prog, argv, text) {
            Ok(()) => return Ok(name),
            Err(_) => tried.push(name),
        }
    }
    // OSC 52: последовательность в /dev/tty (не stdout — TUI владеет экраном).
    if osc52(text).is_ok() {
        return Ok("OSC 52");
    }
    tried.push("OSC 52");
    Err(HarnessError::Tui(format!(
        "буфер обмена недоступен: не сработали {}. \
         Установите wl-copy (Wayland) или xclip/xsel (X11)",
        tried.join(", ")
    )))
}

/// Пишет текст в stdin утилиты буфера; Ok при коде возврата 0.
fn pipe_to(prog: &str, args: &[&str], text: &str) -> std::io::Result<()> {
    let mut child = Command::new(prog)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        // EPIPE не страшен: утилита вправе закрыть stdin раньше.
        let _ = stdin.write_all(text.as_bytes());
    }
    let status = child.wait()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!("{prog}: код {status}")))
    }
}

/// OSC 52: `\x1b]52;c;<base64>\x07` в /dev/tty (clipboard-селекция `c`).
fn osc52(text: &str) -> std::io::Result<()> {
    let mut tty = std::fs::OpenOptions::new().write(true).open("/dev/tty")?;
    let seq = format!("\x1b]52;c;{}\x07", base64_encode(text.as_bytes()));
    tty.write_all(seq.as_bytes())?;
    tty.flush()
}

/// Минимальный base64 (RFC 4648, с паддингом) — без внешней зависимости.
fn base64_encode(data: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = u32::from(*chunk.get(1).unwrap_or(&0));
        let b2 = u32::from(*chunk.get(2).unwrap_or(&0));
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ABC[(n >> 18) as usize & 63] as char);
        out.push(ABC[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { ABC[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { ABC[n as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_vectors_rfc4648() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        // UTF-8 (кириллица) кодируется побайтово.
        assert_eq!(base64_encode("привет".as_bytes()), "0L/RgNC40LLQtdGC");
    }

    #[test]
    fn copy_reports_missing_mechanisms_gracefully() {
        // В тестовой среде хоть один механизм может и сработать — важно,
        // что вызов не паникует и завершается (Ok или понятная ошибка).
        match copy("тест буфера") {
            Ok(mech) => assert!(
                ["wl-copy", "xclip", "xsel", "OSC 52"].contains(&mech),
                "неизвестный механизм {mech}"
            ),
            Err(e) => assert!(e.to_string().contains("буфер обмена недоступен")),
        }
    }
}
