//! Общие хелперы интеграционных тестов CLI.
//!
//! Каталог `common/` (а не `common.rs`) — иначе файл стал бы отдельной
//! тестовой целью.

use std::path::Path;

use assert_cmd::Command;

/// Команда `arch` с домом, перенаправленным в `home` (tempdir теста).
///
/// Изоляция (конвенция AGENTS.md: без сети и без реального дома):
/// - `HOME` и `XDG_CONFIG_HOME` — внутрь tempdir: `dirs::home_dir()` /
///   `dirs::config_dir()` резолвятся туда, `arch init` пишет только
///   в tempdir;
/// - `ARCH_HOME` снимается — иначе он переопределяет `~/.arch-harness`
///   (`Config::home_dir`) и файлы ушли бы мимо tempdir;
/// - API-ключи LLM-провайдеров снимаются — смоуки обязаны проходить без
///   ключей и не зависеть от окружения разработчика;
/// - `cwd` — тот же tempdir: `./arch-harness.toml` из реального каталога
///   запуска тестов не подхватывается.
pub fn arch_cmd(home: &Path) -> Command {
    let mut cmd = Command::cargo_bin("arch").expect("бинарь arch собран cargo");
    cmd.env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .current_dir(home);
    for var in [
        "ARCH_HOME",
        "DEEPSEEK_API_KEY",
        "ZHIPU_API_KEY",
        "KIMI_API_KEY",
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "OPENAI_API_KEY",
    ] {
        cmd.env_remove(var);
    }
    cmd
}
