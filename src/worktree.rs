//! Worktree-фабрика: изоляция агентной работы в git worktree.
//!
//! Паттерн Spec Kitty из разборов `_24_августа` («заимствовать worktree +
//! review/accept/merge в собственный harness»): параллельные агенты и
//! рискованные правки не трогают рабочее дерево архитектора до явного
//! accept; review — это `diff` ветки worktree, accept — merge + уборка.
//!
//! КОНТРАКТ (владелец: агент `control`):
//! - worktree = ветка `arch/<name>` + каталог вне репозитория
//!   (`~/.arch-harness/worktrees/<repo-slug>/<name>`); список — из
//!   `git worktree list` (реестр не дублируется файлом);
//! - инструмент агента только один — `worktree_new`: возвращает путь;
//!   дальше агент работает штатными инструментами с `workdir`;
//! - accept/drop — решения человека (CLI `arch worktree …`, слэш
//!   `/worktree`): accept отказывает при незакоммиченных изменениях
//!   (иначе они молча сгорели бы при remove);
//! - имена — kebab-case `[a-z0-9-]` (ветка и каталог безопасны).

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::error::{HarnessError, Result};

/// Префикс веток worktree-фабрики.
const BRANCH_PREFIX: &str = "arch/";

/// Информация о worktree фабрики.
#[derive(Debug, Clone)]
pub struct WorktreeInfo {
    /// Имя (без префикса ветки).
    pub name: String,
    /// Каталог worktree.
    pub path: PathBuf,
    /// Ветка (`arch/<name>`).
    pub branch: String,
    /// Есть ли незакоммиченные изменения.
    pub dirty: bool,
    /// Коммитов впереди базы (HEAD основного дерева).
    pub ahead: usize,
}

/// Валидирует имя worktree (kebab-case, безопасное для ветки и каталога).
fn validate_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 48
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if ok {
        Ok(())
    } else {
        Err(HarnessError::Tool(format!(
            "worktree: имя '{name}' должно быть kebab-case [a-z0-9-], ≤ 48 символов"
        )))
    }
}

/// Запускает git в репозитории и возвращает stdout; ненулевой код — ошибка
/// с stderr (читаемой модели/пользователю).
async fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let out = tokio::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .await
        .map_err(|e| HarnessError::Tool(format!("git не запустился: {e}")))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        Err(HarnessError::Tool(format!(
            "git {}: {}",
            args.join(" "),
            stderr.trim().chars().take(300).collect::<String>()
        )))
    }
}

/// Каталог worktree фабрики для репозитория.
fn worktrees_root(cfg: &crate::config::Config, repo: &Path) -> PathBuf {
    // Слаг репозитория: имя каталога + короткий FNV-хэш пути (коллизии имён).
    let slug = repo
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".into());
    let mut hash = 0xcbf29ce484222325u64;
    for b in repo.to_string_lossy().as_bytes() {
        hash = (hash ^ u64::from(*b)).wrapping_mul(0x100000001b3);
    }
    cfg.paths
        .reports_dir
        .parent()
        .map(|p| p.join("worktrees"))
        .unwrap_or_else(|| PathBuf::from(".arch-worktrees"))
        .join(format!("{slug}-{hash:08x}"))
}

/// Создаёт worktree `arch/<name>` от `base` (дефолт HEAD) и возвращает путь.
///
/// # Errors
/// Невалидное имя, не git-репозиторий, ветка/каталог уже существуют.
pub async fn create(
    cfg: &crate::config::Config,
    repo: &Path,
    name: &str,
    base: Option<&str>,
) -> Result<PathBuf> {
    validate_name(name)?;
    git(repo, &["rev-parse", "--git-dir"]).await.map_err(|_| {
        HarnessError::Tool(format!("worktree: {} не git-репозиторий", repo.display()))
    })?;
    let branch = format!("{BRANCH_PREFIX}{name}");
    let path = worktrees_root(cfg, repo).join(name);
    if path.exists() {
        return Err(HarnessError::Tool(format!(
            "каталог {} уже существует — выберите другое имя или drop",
            path.display()
        )));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| HarnessError::io(parent, e))?;
    }
    let mut args = vec!["worktree", "add", "-b", &branch];
    let path_str = path.to_string_lossy().into_owned();
    args.push(&path_str);
    if let Some(b) = base {
        args.push(b);
    }
    git(repo, &args).await?;
    Ok(path)
}

/// Список worktree фабрики (ветки `arch/*`).
pub async fn list(repo: &Path) -> Result<Vec<WorktreeInfo>> {
    let out = git(repo, &["worktree", "list", "--porcelain"]).await?;
    let mut infos = Vec::new();
    let mut path: Option<PathBuf> = None;
    let mut branch = String::new();
    let mut flush = |path: Option<PathBuf>, branch: String| {
        if let (Some(p), b) = (path, branch) {
            if let Some(name) = b.strip_prefix(BRANCH_PREFIX) {
                infos.push((p, name.to_string(), b.to_string()));
            }
        }
    };
    for line in out.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            let (p_old, b_old) = (path.take(), std::mem::take(&mut branch));
            flush(p_old, b_old);
            path = Some(PathBuf::from(p));
        } else if let Some(b) = line.strip_prefix("branch refs/heads/") {
            branch = b.to_string();
        }
    }
    flush(path, branch);
    let head = git(repo, &["rev-parse", "HEAD"]).await.unwrap_or_default();
    let head = head.trim().to_string();
    let mut out_infos = Vec::new();
    for (path, name, branch) in infos {
        let dirty = git(&path, &["status", "--porcelain"])
            .await
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        let ahead = git(
            &path,
            &["rev-list", "--count", &format!("{head}..{branch}")],
        )
        .await
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
        out_infos.push(WorktreeInfo {
            name,
            path,
            branch,
            dirty,
            ahead,
        });
    }
    Ok(out_infos)
}

/// Diff worktree против HEAD основного дерева (stat + патч, усечённый).
pub async fn diff(repo: &Path, name: &str) -> Result<String> {
    validate_name(name)?;
    let branch = format!("{BRANCH_PREFIX}{name}");
    let stat = git(repo, &["diff", "--stat", &format!("HEAD...{branch}")]).await?;
    let patch = git(repo, &["diff", &format!("HEAD...{branch}")]).await?;
    let patch: String = patch.chars().take(24_000).collect();
    Ok(format!("== diff --stat ==\n{stat}\n== patch ==\n{patch}"))
}

/// Accept: merge ветки worktree в текущую ветку основного дерева и уборка.
///
/// Отказывает при незакоммиченных изменениях в worktree (merge взял бы
/// только коммиты, остальное сгорело бы при remove).
///
/// # Errors
/// Незакоммиченные изменения, конфликт merge, worktree не найден.
pub async fn accept(cfg: &crate::config::Config, repo: &Path, name: &str) -> Result<String> {
    validate_name(name)?;
    let branch = format!("{BRANCH_PREFIX}{name}");
    let path = worktrees_root(cfg, repo).join(name);
    if path.exists() {
        let dirty = git(&path, &["status", "--porcelain"]).await?;
        if !dirty.trim().is_empty() {
            return Err(HarnessError::Tool(format!(
                "worktree '{name}' содержит незакоммиченные изменения — закоммитьте или drop: {dirty}"
            )));
        }
    }
    // Идентичность коммиттера может быть не настроена (CI, свежие
    // контейнеры) — merge тогда падает с «Committer identity unknown».
    // Если git не разрешил идентичность, подставляем фолбэк харнесса через
    // `-c`; настроенная пользовательская идентичность остаётся приоритетной.
    let has_identity = git(repo, &["var", "GIT_COMMITTER_IDENT"]).await.is_ok();
    let message = format!("arch: accept worktree {name}");
    let mut args: Vec<&str> = Vec::with_capacity(8);
    if !has_identity {
        args.extend([
            "-c",
            "user.name=spine-harness",
            "-c",
            "user.email=spine-harness@localhost",
        ]);
    }
    args.extend(["merge", "--no-ff", "-m", &message, &branch]);
    git(repo, &args).await?;
    if path.exists() {
        git(repo, &["worktree", "remove", &path.to_string_lossy()]).await?;
    }
    git(repo, &["branch", "-d", &branch]).await?;
    Ok(format!("worktree '{name}' принят (merge) и убран"))
}

/// Drop: удаление worktree и ветки БЕЗ merge (откат изоляции).
///
/// # Errors
/// Незакоммиченные изменения (форс не делаем — это защита от потери работы).
pub async fn drop(cfg: &crate::config::Config, repo: &Path, name: &str) -> Result<String> {
    validate_name(name)?;
    let branch = format!("{BRANCH_PREFIX}{name}");
    let path = worktrees_root(cfg, repo).join(name);
    if path.exists() {
        let dirty = git(&path, &["status", "--porcelain"]).await?;
        if !dirty.trim().is_empty() {
            return Err(HarnessError::Tool(format!(
                "worktree '{name}' содержит незакоммиченные изменения — drop отменён: {dirty}"
            )));
        }
        git(repo, &["worktree", "remove", &path.to_string_lossy()]).await?;
    }
    // -D: ветка может быть не влита — в этом смысл drop; коммиты остаются
    // в reflog, потеря восстановима.
    let _ = git(repo, &["branch", "-D", &branch]).await;
    Ok(format!("worktree '{name}' удалён без merge"))
}

/// Текстовое представление списка (для CLI и слэша).
#[must_use]
pub fn render_list(infos: &[WorktreeInfo]) -> String {
    if infos.is_empty() {
        return "worktree фабрики нет (создание: worktree_new / arch worktree new <name>)".into();
    }
    let mut out = String::new();
    for i in infos {
        out.push_str(&format!(
            "── {} · {} · впереди: {} · {}{}\n",
            i.name,
            i.path.display(),
            i.ahead,
            if i.dirty {
                "ГРЯЗНЫЙ"
            } else {
                "чистый"
            },
            if i.dirty {
                " (accept/drop заблокированы)"
            } else {
                ""
            }
        ));
    }
    out
}

/// Инструмент агента: `worktree_new` — создать изолированное дерево работы.
pub struct WorktreeNewTool;

#[async_trait::async_trait]
impl crate::tool::Tool for WorktreeNewTool {
    fn spec(&self) -> crate::llm::ToolSpec {
        crate::llm::ToolSpec {
            name: "worktree_new".into(),
            description: "Создать изолированный git worktree (ветка arch/<name>, каталог вне \
                репозитория) для рискованных или параллельных правок: работай в нём через \
                workdir остальных инструментов; основное дерево пользователя не трогается. \
                Review — git diff ветки; accept/drop — решение человека (arch worktree …)."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "имя worktree kebab-case [a-z0-9-], напр. saga-pilot"
                    },
                    "repo": {
                        "type": "string",
                        "description": "путь к git-репозиторию; пусто — текущий каталог"
                    },
                    "base": {
                        "type": "string",
                        "description": "базовая ветка/коммит; пусто — HEAD"
                    }
                },
                "required": ["name"]
            }),
        }
    }

    async fn call(
        &self,
        args: Value,
        ctx: &crate::tool::ToolContext,
    ) -> Result<crate::tool::ToolOutput> {
        let name = args
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        let repo = args
            .get("repo")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map_or_else(|| ctx.cwd.clone(), |r| ctx.resolve(r));
        let base = args
            .get("base")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());
        match create(&ctx.config, &repo, name, base).await {
            Ok(path) => Ok(crate::tool::ToolOutput::ok(format!(
                "worktree создан: {} (ветка arch/{name}). Все правки делай ТОЛЬКО там \
                 (передавай workdir=\"{}\" в bash/write_file/edit_file); основное дерево \
                 не меняй. По завершении сообщи пользователю: review — `arch worktree diff {name}`, \
                 accept — `arch worktree accept {name}`.",
                path.display(),
                path.display()
            ))),
            Err(e) => Ok(crate::tool::ToolOutput::err(format!("{e}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::sync::Arc;

    /// git в каталоге с тестовой идентичностью коммиттера.
    async fn git_in(dir: &Path, args: &[&str]) {
        let out = tokio::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .await
            .expect("git");
        assert!(
            out.status.success(),
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Репозиторий-фикстура: git init + один коммит.
    async fn make_repo(dir: &Path) {
        git_in(dir, &["init", "-b", "main"]).await;
        std::fs::write(dir.join("README.md"), "base\n").expect("write");
        git_in(dir, &["add", "."]).await;
        git_in(dir, &["commit", "-m", "init"]).await;
    }

    fn test_cfg(dir: &Path) -> Arc<Config> {
        let mut cfg = Config::default();
        cfg.paths.reports_dir = dir.join("reports");
        Arc::new(cfg)
    }

    #[test]
    fn name_validation_and_render_list() {
        for ok in ["pilot", "saga-2", "a"] {
            assert!(validate_name(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "Caps",
            "с пробелом",
            "слэш/внутри",
            "точка.com",
            &"x".repeat(49),
        ] {
            assert!(validate_name(bad).is_err(), "{bad}");
        }
        assert!(render_list(&[]).contains("нет"), "пусто — подсказка");
        let infos = vec![WorktreeInfo {
            name: "pilot".into(),
            path: PathBuf::from("/tmp/wt/pilot"),
            branch: "arch/pilot".into(),
            dirty: true,
            ahead: 3,
        }];
        let text = render_list(&infos);
        assert!(
            text.contains("pilot") && text.contains("ГРЯЗНЫЙ") && text.contains("впереди: 3"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn worktree_full_cycle_create_diff_accept() {
        let tmp = tempfile::tempdir().expect("tmp");
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).expect("mkdir");
        make_repo(&repo).await;
        let cfg = test_cfg(tmp.path());

        let path = create(&cfg, &repo, "pilot-x", None).await.expect("create");
        assert!(path.join("README.md").is_file(), "worktree имеет базу");
        // Правка и коммит внутри worktree.
        std::fs::write(path.join("feature.md"), "фича\n").expect("write");
        git_in(&path, &["add", "."]).await;
        git_in(&path, &["commit", "-m", "feature"]).await;
        let infos = list(&repo).await.expect("list");
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].name, "pilot-x");
        assert!(!infos[0].dirty, "после коммита чисто");
        assert_eq!(infos[0].ahead, 1);
        let d = diff(&repo, "pilot-x").await.expect("diff");
        assert!(d.contains("feature.md"), "diff видит файл: {d}");
        let msg = accept(&cfg, &repo, "pilot-x").await.expect("accept");
        assert!(msg.contains("принят"), "{msg}");
        assert!(repo.join("feature.md").is_file(), "merge перенёс файл");
        assert!(
            list(&repo).await.expect("list2").is_empty(),
            "ветка и каталог убраны"
        );
    }

    #[tokio::test]
    async fn drop_and_dirty_guards() {
        let tmp = tempfile::tempdir().expect("tmp");
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).expect("mkdir");
        make_repo(&repo).await;
        let cfg = test_cfg(tmp.path());
        let path = create(&cfg, &repo, "risky", None).await.expect("create");
        // Незакоммиченная правка блокирует drop.
        std::fs::write(path.join("wip.md"), "wip\n").expect("write");
        let err = drop(&cfg, &repo, "risky").await.expect_err("dirty guard");
        assert!(err.to_string().contains("незакоммиченные"), "{err}");
        // Чистый drop убирает всё.
        std::fs::remove_file(path.join("wip.md")).expect("rm");
        drop(&cfg, &repo, "risky").await.expect("drop");
        assert!(list(&repo).await.expect("list").is_empty());
        // Валидация имени.
        assert!(create(&cfg, &repo, "Bad Name!", None).await.is_err());
    }
}
