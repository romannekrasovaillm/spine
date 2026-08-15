//! Дельта-спецификации как state machine (по OpenSpec): change-центричные
//! изменения `propose → (apply) → archive`; дельта описывает только изменение
//! относительно текущей истины (ADDED/MODIFIED/REMOVED).
//!
//! Каталог изменений — `changes/` в репозитории: предложенные на верхнем
//! уровне, заархивированные — в `changes/archive/`.

use std::path::{Path, PathBuf};

use crate::control::LintIssue;
use crate::error::{HarnessError, Result};

/// Шаблон дельты.
const DELTA_TEMPLATE: &str = "# Дельта: {name}
\
- Route: Fast|Standard (Critical — полный Solutioning, дельты недостаточно)
- Created: {date}

## Проблема

<что и зачем меняем — 2-3 предложения>

## ADDED

- <новые требования с критериями EARS: When <событие>, the <система> shall <реакция>>

## MODIFIED

- <изменяемые требования: было → стало, причина>

## REMOVED

- <удаляемое: что, замена, план миграции потребителей>

## План отката

<как откатываем изменение>

## Критерии приёмки

- [ ] <проверяемый критерий>
";

/// Создаёт каркас дельты `changes/<name>/DELTA.md`.
///
/// # Errors
/// Каталог существует, ошибка записи.
pub fn new(repo: &Path, name: &str) -> Result<PathBuf> {
    let dir = repo.join("changes").join(name);
    if dir.exists() {
        return Err(HarnessError::Control(format!(
            "дельта '{name}' уже существует: {}",
            dir.display()
        )));
    }
    std::fs::create_dir_all(&dir).map_err(|e| HarnessError::io(&dir, e))?;
    let path = dir.join("DELTA.md");
    let content = DELTA_TEMPLATE
        .replace("{name}", name)
        .replace("{date}", &chrono::Local::now().format("%Y-%m-%d").to_string());
    std::fs::write(&path, content).map_err(|e| HarnessError::io(&path, e))?;
    Ok(path)
}

/// Статус дельты.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeltaStatus {
    /// Предложена (changes/<name>).
    Proposed,
    /// В архиве (changes/archive/<name>).
    Archived,
}

/// Сводка дельты.
#[derive(Debug, Clone)]
pub struct DeltaInfo {
    /// Имя.
    pub name: String,
    /// Статус.
    pub status: DeltaStatus,
    /// Путь к DELTA.md.
    pub path: PathBuf,
}

/// Список дельт (предложенные и архивные).
pub fn list(repo: &Path) -> Vec<DeltaInfo> {
    let mut out = Vec::new();
    for (dir, status) in [
        (repo.join("changes"), DeltaStatus::Proposed),
        (repo.join("changes/archive"), DeltaStatus::Archived),
    ] {
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let p = e.path();
                if !p.is_dir() || p.file_name().is_some_and(|n| n == "archive") {
                    continue;
                }
                let delta = p.join("DELTA.md");
                if delta.is_file() {
                    out.push(DeltaInfo {
                        name: e.file_name().to_string_lossy().to_string(),
                        status,
                        path: delta,
                    });
                }
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Валидация структуры дельты: обязательные секции, заглушки, пустые блоки.
///
/// # Errors
/// DELTA.md не читается.
pub fn validate(repo: &Path, name: &str) -> Result<Vec<LintIssue>> {
    let path = find_delta(repo, name)?;
    let text = std::fs::read_to_string(&path).map_err(|e| HarnessError::io(&path, e))?;
    let mut issues = Vec::new();
    for section in ["## Проблема", "## ADDED", "## MODIFIED", "## REMOVED", "## План отката", "## Критерии приёмки"] {
        if !text.contains(section) {
            issues.push(LintIssue {
                file: path.clone(),
                line: 0,
                rule: "missing_section".into(),
                message: format!("нет секции «{section}»"),
                severity: "error".into(),
            });
        }
    }
    // Пустые секции-заглушки из шаблона.
    for (n, line) in text.lines().enumerate() {
        let t = line.trim();
        if t.starts_with('<') && t.ends_with('>') || t.contains("TODO") || t.contains("TBD") {
            issues.push(LintIssue {
                file: path.clone(),
                line: n + 1,
                rule: "stub_marker".into(),
                message: format!("незаполненное место: {}", t.chars().take(60).collect::<String>()),
                severity: "warn".into(),
            });
        }
    }
    // Все три блока пустые — дельта ни о чём.
    let has_content = ["## ADDED", "## MODIFIED", "## REMOVED"].iter().any(|s| {
        section_body(&text, s).lines().any(|l| l.trim().starts_with('-') && !l.contains('<'))
    });
    if !has_content {
        issues.push(LintIssue {
            file: path.clone(),
            line: 0,
            rule: "empty_delta".into(),
            message: "ADDED/MODIFIED/REMOVED пусты — дельта без содержания".into(),
            severity: "error".into(),
        });
    }
    Ok(issues)
}

/// Тело секции между заголовком `## X` и следующим `## `.
fn section_body<'a>(text: &'a str, section: &str) -> &'a str {
    let Some(start) = text.find(section) else {
        return "";
    };
    let rest = &text[start + section.len()..];
    let end = rest.find("\n## ").unwrap_or(rest.len());
    &rest[..end]
}

/// Архивирует дельту (после apply): валидация → перенос в `changes/archive/`.
///
/// # Errors
/// Дельта не найдена, не прошла валидацию (error-находки), ошибка переноса.
pub fn archive(repo: &Path, name: &str) -> Result<PathBuf> {
    let issues = validate(repo, name)?;
    let errors: Vec<&LintIssue> = issues.iter().filter(|i| i.severity == "error").collect();
    if !errors.is_empty() {
        return Err(HarnessError::Control(format!(
            "дельта '{name}' не прошла валидацию: {} error-находок",
            errors.len()
        )));
    }
    let src = repo.join("changes").join(name);
    let dst_dir = repo.join("changes/archive");
    std::fs::create_dir_all(&dst_dir).map_err(|e| HarnessError::io(&dst_dir, e))?;
    let dst = dst_dir.join(name);
    if dst.exists() {
        return Err(HarnessError::Control(format!(
            "в архиве уже есть '{name}' — номера/имена не переиспользуются"
        )));
    }
    std::fs::rename(&src, &dst).map_err(|e| HarnessError::io(&dst, e))?;
    Ok(dst.join("DELTA.md"))
}

/// Находит DELTA.md по имени (предложенная или архивная).
fn find_delta(repo: &Path, name: &str) -> Result<PathBuf> {
    for base in [repo.join("changes").join(name), repo.join("changes/archive").join(name)] {
        let p = base.join("DELTA.md");
        if p.is_file() {
            return Ok(p);
        }
    }
    Err(HarnessError::Control(format!(
        "дельта '{name}' не найдена в {}/changes",
        repo.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_lifecycle_new_validate_archive() {
        let tmp = tempfile::tempdir().expect("tmp");
        let repo = tmp.path();
        let path = new(repo, "payment-timeout").expect("new");
        assert!(path.is_file());
        // Дубликат — ошибка.
        assert!(new(repo, "payment-timeout").is_err());
        // Шаблон не валиден (заглушки + пустые блоки).
        let issues = validate(repo, "payment-timeout").expect("validate");
        assert!(issues.iter().any(|i| i.rule == "empty_delta" && i.severity == "error"));
        assert!(archive(repo, "payment-timeout").is_err(), "неполная дельта не архивируется");
        // Заполняем.
        std::fs::write(
            &path,
            "# Дельта\n\n## Проблема\nТаймаут велик.\n\n## ADDED\n- Требование: таймаут авторизации 500 мс. Критерий: When запрос, the оркестратор shall ответить ≤ 500 мс\n\n## MODIFIED\n\n## REMOVED\n\n## План отката\nrevert флага\n\n## Критерии приёмки\n- [ ] тест таймаута\n",
        )
        .expect("fill");
        let issues = validate(repo, "payment-timeout").expect("validate2");
        assert!(!issues.iter().any(|i| i.severity == "error"), "{issues:?}");
        // В списке как proposed; после архивации — archived.
        assert!(list(repo).iter().any(|d| d.status == DeltaStatus::Proposed));
        let archived = archive(repo, "payment-timeout").expect("archive");
        assert!(archived.is_file());
        let lst = list(repo);
        assert!(lst.iter().any(|d| d.status == DeltaStatus::Archived));
        assert!(!lst.iter().any(|d| d.status == DeltaStatus::Proposed));
        // Повторная архивация под тем же именем — ошибка (имена не переиспользуются).
        new(repo, "payment-timeout").expect("re-new ok");
        std::fs::write(
            repo.join("changes/payment-timeout/DELTA.md"),
            "# Д\n\n## Проблема\nx\n\n## ADDED\n- y\n\n## MODIFIED\n\n## REMOVED\n\n## План отката\nz\n\n## Критерии приёмки\n- [ ] q\n",
        )
        .expect("fill2");
        assert!(archive(repo, "payment-timeout").is_err());
    }
}
