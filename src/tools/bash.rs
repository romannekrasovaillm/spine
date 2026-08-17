//! Инструмент `bash`: выполнение shell-команд.
//!
//! КОНТРАКТ (владелец: агент `tools`): структура [`BashTool`] (unit-структура),
//! реализующая [`Tool`]. Аргументы: `command` (string, обяз.), `timeout_secs`
//! (u64, опц., дефолт 120, макс 1800), `workdir` (string, опц. — относительно
//! ctx.cwd). Вывод: stdout+stderr + независимые маркеры исхода
//! (`[код возврата: N]` / `[сигнал: N]` / `[таймаут: …]`); усечение до ~30 КБ.
//!
//! Defensive patterns (`DeepSeek` Harness `docs/defensive-patterns.md`):
//! - **scrub окружения**: дочерний процесс получает копию env БЕЗ
//!   секретоподобных переменных (токены имени KEY/SECRET/TOKEN/PASSWORD/…,
//!   см. [`is_secret_env_name`]) — команда, которую написала модель, не
//!   увидит ключи провайдеров и не сольёт их в лог/curl/spill-файл.
//!   Управляется `[bash] env_scrub` / `env_allow` в конфиге;
//! - **ортогональные исходы сообщаются независимо**: код возврата, сигнал
//!   и таймаут — отдельные маркеры, никогда не вложены друг в друга
//!   (процесс может поймать сигнал И выйти с кодом 0 — оба факта видны);
//! - **dispose до квиэссенции**: по таймауту процесс убивается явным
//!   `kill().await` (SIGKILL + ожидание), частичный вывод собирается и
//!   отдаётся модели — видно, на чём зависла команда.

use std::fmt::Write as _;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::error::{HarnessError, Result};
use crate::llm::ToolSpec;
use crate::tool::{Tool, ToolContext, ToolOutput};

/// Дефолтный таймаут выполнения команды, секунды.
const DEFAULT_TIMEOUT_SECS: u64 = 120;
/// Жёсткий максимум таймаута, секунды (защита от «вечных» команд). 30 минут
/// хватает длинным сборкам; прогоны кодовых харнессов (claude/qwen/…) сюда
/// не кладём — для них `harness_run` (потолок 7200 + таймаут тишины).
const MAX_TIMEOUT_SECS: u64 = 1800;
/// Лимит размера вывода (stdout + stderr), символов; дальше — усечение.
const MAX_OUTPUT_CHARS: usize = 30 * 1024;

/// Секретоподобные токены в имени переменной окружения (имя режется по
/// `_`, совпадение точное: `MONKEY` не задевается, `DEEPSEEK_API_KEY` — да).
const SECRET_NAME_TOKENS: [&str; 9] = [
    "KEY",
    "SECRET",
    "TOKEN",
    "PASSWORD",
    "PASSWD",
    "PASS",
    "CREDENTIAL",
    "CREDENTIALS",
    "AUTH",
];

/// Похоже ли имя переменной окружения на секрет (по токенам имени).
fn is_secret_env_name(name: &str) -> bool {
    name.split('_')
        .any(|part| SECRET_NAME_TOKENS.contains(&part))
}

/// Фильтрует окружение для дочернего процесса.
///
/// `scrub=false` — окружение проходит как есть; `allow` — точные имена,
/// которые пропускаются даже при включённом scrub. Возвращает пары
/// (имя, значение) для `Command::envs` и число скрытых переменных.
fn scrub_env<I>(vars: I, scrub: bool, allow: &[String]) -> (Vec<(String, String)>, usize)
where
    I: IntoIterator<Item = (String, String)>,
{
    let mut dropped = 0usize;
    let mut keep = Vec::new();
    for (name, value) in vars {
        let hidden =
            scrub && is_secret_env_name(&name.to_uppercase()) && !allow.iter().any(|a| a == &name);
        if hidden {
            dropped += 1;
        } else {
            keep.push((name, value));
        }
    }
    (keep, dropped)
}

/// Исход выполнения команды — ортогональные факты без вложенности.
enum Outcome {
    /// Процесс завершился (код возврата или сигнал — извлекаются из статуса).
    Exited(std::process::ExitStatus),
    /// Таймаут: процесс убит после N секунд, вывод частичный.
    TimedOut(u64),
}

/// Инструмент выполнения bash-команд.
///
/// Команда запускается через `bash -c` в `workdir` (по умолчанию —
/// [`ToolContext::cwd`]); захватываются stdout, stderr и исход.
/// Ненулевой код возврата, сигнал и таймаут помечаются ошибкой (`is_error`),
/// чтобы модель увидела сбой и скорректировала план. Вывод усекается
/// до 30 КБ.
pub struct BashTool;

#[async_trait]
impl Tool for BashTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "bash".into(),
            description: "Выполнить bash-команду и вернуть stdout, stderr и маркеры исхода \
                ([код возврата: N] / [сигнал: N] / [таймаут: …]). \
                Вызывать для действий, которым место в shell: сборка и тесты проекта, git, \
                поиск утилит, конвейеры обработки текста, проверка процессов и портов. \
                Для чтения/записи файлов предпочитайте read_file/write_file/edit_file, \
                для поиска файлов — glob, по содержимому — grep. \
                Долгие команды ограничивайте timeout_secs (дефолт 120, максимум 1800): \
                по таймауту процесс убивается, но частичный вывод возвращается. \
                Прогоны кодовых харнессов (claude, qwen, openclaw, hermes, theseus, \
                codewhale) через bash ЗАПРЕЩЕНЫ — только harness_run: у него потолок \
                7200 с, таймаут тишины с heartbeat по файлам и убийство всей группы \
                процессов; здесь прогон обрежется по timeout_secs без heartbeat. \
                Крупные heredoc-записи файлов разбивайте на части по 8–12 КБ \
                (дозапись `>>`), иначе вызов упирается в потолок max_tokens и \
                обрезается на середине — харнесс такие вызовы отклоняет. \
                Окружение команды очищено от секретоподобных переменных \
                (*_KEY, *_TOKEN, *_SECRET, …) — ключи API здесь недоступны."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "Команда для `bash -c` (конвейеры и перенаправления допустимы)"
                    },
                    "timeout_secs": {
                        "type": "integer",
                        "description": "Таймаут выполнения в секундах (дефолт 120, максимум 1800)",
                        "default": DEFAULT_TIMEOUT_SECS,
                        "minimum": 1,
                        "maximum": MAX_TIMEOUT_SECS
                    },
                    "workdir": {
                        "type": "string",
                        "description": "Рабочий каталог команды; относительный — от текущего каталога харнесса"
                    }
                },
                "required": ["command"]
            }),
        }
    }

    /// Агентный цикл не должен обрывать вызов раньше собственного таймаута
    /// команды: берём жёсткий максимум плюс запас на завершение процесса
    /// (инцидент 12:32 — агент резал bash на 300 с при внутренних 1800).
    fn timeout_secs(&self) -> u64 {
        MAX_TIMEOUT_SECS + 60
    }

    /// Выполняет команду с таймаутом и scrub окружения.
    ///
    /// # Errors
    /// Возвращает `Err` при нарушении контракта аргументов (нет `command`)
    /// или при системной ошибке запуска/ожидания процесса. Сбои самой
    /// команды (ненулевой код, сигнал, таймаут) — `Ok(ToolOutput::err(..))`.
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let command = args
            .get("command")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| {
                HarnessError::Tool(
                    "bash: обязательный аргумент `command` отсутствует или пуст".into(),
                )
            })?;
        let timeout_secs = args
            .get("timeout_secs")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .clamp(1, MAX_TIMEOUT_SECS);
        let workdir = args
            .get("workdir")
            .and_then(Value::as_str)
            .map_or_else(|| ctx.cwd.clone(), |w| ctx.resolve(w));

        let mut cmd = tokio::process::Command::new("bash");
        cmd.arg("-c")
            .arg(command)
            .current_dir(&workdir)
            .kill_on_drop(true)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        // Scrub окружения: модельные команды не получают секреты харнесса.
        // vars_os + lossy: vars() паникует на не-UTF8 значениях окружения.
        let (vars, dropped) = scrub_env(
            std::env::vars_os()
                .filter(|(k, _)| k != "PWD")
                .map(|(k, v)| {
                    (
                        k.to_string_lossy().into_owned(),
                        v.to_string_lossy().into_owned(),
                    )
                }),
            ctx.config.bash.env_scrub,
            &ctx.config.bash.env_allow,
        );
        cmd.env_clear().envs(vars);

        let mut child = cmd.spawn().map_err(|e| {
            HarnessError::Tool(format!(
                "bash: не удалось запустить процесс в {}: {e}",
                workdir.display()
            ))
        })?;

        // Читатели stdout/stderr живут отдельно от ожидания: так по таймауту
        // сохраняется частичный вывод (wait_with_output его бы потерял).
        let stdout_pipe = child.stdout.take();
        let stderr_pipe = child.stderr.take();
        let out_task = tokio::spawn(async move { read_pipe(stdout_pipe).await });
        let err_task = tokio::spawn(async move { read_pipe(stderr_pipe).await });

        let outcome =
            match tokio::time::timeout(Duration::from_secs(timeout_secs), child.wait()).await {
                Ok(Ok(status)) => Outcome::Exited(status),
                Ok(Err(e)) => {
                    return Err(HarnessError::Tool(format!(
                        "bash: ошибка ожидания процесса: {e}"
                    )));
                }
                Err(_) => {
                    // Dispose до квиэссенции: SIGKILL и ожидание смерти процесса,
                    // затем читатели до EOF (pipe закрывается смертью процесса).
                    let _ = child.kill().await;
                    Outcome::TimedOut(timeout_secs)
                }
            };
        let stdout = out_task.await.unwrap_or_default();
        let stderr = err_task.await.unwrap_or_default();
        Ok(format_result(&stdout, &stderr, &outcome, dropped).truncated(MAX_OUTPUT_CHARS))
    }
}

/// Читает pipe процесса до EOF; ошибка чтения означает, что процесс уже
/// убит, — возвращаем то, что успели накопить (пустой буфер в худшем случае).
async fn read_pipe(pipe: Option<impl tokio::io::AsyncRead + Unpin>) -> Vec<u8> {
    use tokio::io::AsyncReadExt;
    match pipe {
        Some(mut p) => {
            let mut buf = Vec::new();
            // Ошибка чтения = убитый процесс; частичный буфер сохраняем.
            let _ = p.read_to_end(&mut buf).await;
            buf
        }
        None => Vec::new(),
    }
}

/// Собирает текстовый результат: вывод + независимые маркеры исхода.
fn format_result(
    stdout: &[u8],
    stderr: &[u8],
    outcome: &Outcome,
    dropped_env: usize,
) -> ToolOutput {
    let mut content = String::from_utf8_lossy(stdout).into_owned();
    let stderr_text = String::from_utf8_lossy(stderr);
    if !stderr_text.is_empty() {
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str("--- stderr ---\n");
        content.push_str(&stderr_text);
    }
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    if dropped_env > 0 {
        let _ = writeln!(
            content,
            "[env scrub: {dropped_env} секретоподобных переменных скрыто от команды]"
        );
    }
    let is_error = match outcome {
        Outcome::Exited(status) => {
            if let Some(code) = status.code() {
                let _ = writeln!(content, "[код возврата: {code}]");
            } else {
                let _ = writeln!(content, "[сигнал: {}]", signal_of(*status));
            }
            !status.success()
        }
        Outcome::TimedOut(secs) => {
            let _ = writeln!(
                content,
                "[таймаут: процесс убит после {secs} сек; вывод частичный]"
            );
            true
        }
    };
    ToolOutput { content, is_error }
}

/// Номер сигнала, которым завершился процесс (Unix); на прочих
/// платформах — текстовая пометка без номера.
#[cfg(unix)]
fn signal_of(status: std::process::ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt;
    status
        .signal()
        .map_or_else(|| "?".to_string(), |s| s.to_string())
}

/// Заглушка для не-Unix платформ.
#[cfg(not(unix))]
fn signal_of(_status: std::process::ExitStatus) -> String {
    "?".to_string()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tempfile::TempDir;

    use super::*;
    use crate::config::Config;

    fn test_ctx(dir: &TempDir) -> ToolContext {
        ToolContext::new(dir.path().to_path_buf(), Arc::new(Config::default()))
    }

    #[tokio::test]
    async fn echo_returns_stdout_and_zero_code() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let out = BashTool
            .call(json!({"command": "echo hello"}), &test_ctx(&dir))
            .await?;
        assert!(!out.is_error, "output: {}", out.content);
        assert!(out.content.contains("hello"), "output: {}", out.content);
        assert!(
            out.content.contains("[код возврата: 0]"),
            "output: {}",
            out.content
        );
        Ok(())
    }

    #[tokio::test]
    async fn nonzero_exit_is_error_with_stderr() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let out = BashTool
            .call(json!({"command": "echo oops >&2; exit 3"}), &test_ctx(&dir))
            .await?;
        assert!(out.is_error, "output: {}", out.content);
        assert!(out.content.contains("oops"), "output: {}", out.content);
        assert!(
            out.content.contains("[код возврата: 3]"),
            "output: {}",
            out.content
        );
        Ok(())
    }

    #[tokio::test]
    async fn timeout_kills_long_command_and_keeps_partial_output() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let out = BashTool
            .call(
                json!({"command": "echo до-сна; sleep 5; echo после-сна", "timeout_secs": 1}),
                &test_ctx(&dir),
            )
            .await?;
        assert!(out.is_error, "output: {}", out.content);
        assert!(out.content.contains("[таймаут:"), "output: {}", out.content);
        assert!(
            out.content.contains("до-сна"),
            "частичный вывод сохранён: {}",
            out.content
        );
        assert!(
            !out.content.contains("после-сна"),
            "хвост после убийства недостижим: {}",
            out.content
        );
        Ok(())
    }

    #[test]
    fn tool_timeout_covers_max_command_time() {
        // Агентный цикл берёт Tool::timeout_secs: он обязан покрывать
        // максимальный timeout_secs команды (+ запас на убийство процесса),
        // иначе цикл оборвёт вызов раньше собственного таймаута bash.
        assert_eq!(BashTool.timeout_secs(), MAX_TIMEOUT_SECS + 60);
        assert!(BashTool.timeout_secs() > MAX_TIMEOUT_SECS);
    }

    #[tokio::test]
    async fn large_timeout_secs_is_accepted_and_clamped() -> Result<()> {
        // Длинные сборки: 1500 с — валидное значение; сверхмаксимум — кламп,
        // не ошибка (команда быстрая, проверяем приём аргумента).
        let dir = tempfile::tempdir()?;
        let out = BashTool
            .call(
                json!({"command": "echo ok", "timeout_secs": 1500}),
                &test_ctx(&dir),
            )
            .await?;
        assert!(!out.is_error, "output: {}", out.content);
        assert!(out.content.contains("ok"), "output: {}", out.content);
        let out = BashTool
            .call(
                json!({"command": "echo ok", "timeout_secs": 99999}),
                &test_ctx(&dir),
            )
            .await?;
        assert!(!out.is_error, "output: {}", out.content);
        Ok(())
    }

    #[tokio::test]
    async fn signal_is_reported_orthogonally() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let out = BashTool
            .call(json!({"command": "kill -9 $$"}), &test_ctx(&dir))
            .await?;
        assert!(out.is_error, "output: {}", out.content);
        assert!(
            out.content.contains("[сигнал: 9]"),
            "output: {}",
            out.content
        );
        Ok(())
    }

    #[tokio::test]
    async fn workdir_is_resolved_against_cwd() -> Result<()> {
        let dir = tempfile::tempdir()?;
        std::fs::create_dir(dir.path().join("sub"))?;
        let out = BashTool
            .call(json!({"command": "pwd", "workdir": "sub"}), &test_ctx(&dir))
            .await?;
        assert!(!out.is_error, "output: {}", out.content);
        assert!(out.content.contains("sub"), "output: {}", out.content);
        Ok(())
    }

    #[tokio::test]
    async fn missing_command_is_err() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let res = BashTool.call(json!({}), &test_ctx(&dir)).await;
        assert!(res.is_err());
        Ok(())
    }

    #[test]
    fn secret_env_names_detected_by_name_tokens() {
        for name in [
            "DEEPSEEK_API_KEY",
            "GH_TOKEN",
            "AWS_SECRET_ACCESS_KEY",
            "POSTGRES_PASSWORD",
            "SSH_AUTH_SOCK",
            "NPM_CREDENTIAL",
        ] {
            assert!(is_secret_env_name(name), "{name} должно скрываться");
        }
        for name in ["PATH", "HOME", "MONKEY", "KEYBOARD_LAYOUT", "USER", "LANG"] {
            assert!(!is_secret_env_name(name), "{name} должно остаться");
        }
    }

    #[test]
    fn scrub_env_drops_secrets_and_honours_allow_list() {
        let vars = vec![
            ("PATH".to_string(), "/usr/bin".to_string()),
            ("MY_API_KEY".to_string(), "supersecret".to_string()),
            ("GH_TOKEN".to_string(), "ghp_x".to_string()),
        ];
        let (kept, dropped) = scrub_env(vars.clone(), true, &[]);
        assert_eq!(dropped, 2);
        assert!(kept.iter().all(|(k, _)| k == "PATH"));
        // scrub выключен — всё проходит.
        let (kept, dropped) = scrub_env(vars.clone(), false, &[]);
        assert_eq!(dropped, 0);
        assert_eq!(kept.len(), 3);
        // allow-лист возвращает точное имя.
        let (kept, dropped) = scrub_env(vars, true, &["GH_TOKEN".to_string()]);
        assert_eq!(dropped, 1);
        assert!(kept.iter().any(|(k, _)| k == "GH_TOKEN"));
        assert!(kept.iter().all(|(k, _)| k != "MY_API_KEY"));
    }

    #[tokio::test]
    async fn spawned_command_does_not_see_provider_keys() -> Result<()> {
        // DEEPSEEK_API_KEY живёт в env пользователя; модельный процесс
        // его не получает (если переменной нет — тест тривиально зелёный,
        // проверку фильтра несут юнит-тесты выше).
        let dir = tempfile::tempdir()?;
        let out = BashTool
            .call(
                json!({"command": "env | grep -cE '_(KEY|TOKEN|SECRET|PASSWORD)=' || true"}),
                &test_ctx(&dir),
            )
            .await?;
        assert!(!out.is_error, "output: {}", out.content);
        let count: usize = out
            .content
            .lines()
            .next()
            .and_then(|l| l.trim().parse().ok())
            .unwrap_or(0);
        assert_eq!(
            count, 0,
            "секретоподобные переменные видны: {}",
            out.content
        );
        Ok(())
    }
}
