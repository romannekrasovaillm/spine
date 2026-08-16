//! Риск-адаптивная автономность (R-уровни R0–R5, по AI-Disrupt PDLC):
//! автономия калибруется риском действия (обратимость, blast radius), а не
//! брендом модели. Политика применяется к каждому вызову инструмента в
//! [`crate::tool::ToolRegistry::dispatch`].
//!
//! - R0 — всё через человека (auto только чтения);
//! - R1 — + чтения и поиск авто;
//! - R2 — + изменения в рабочем каталоге авто (дефолт харнесса);
//! - R3 — + изменения авто с обязательным журналом (у нас журнал всегда);
//! - R4 — деструктивные действия только с подтверждением человека;
//! - R5 — полная автономия (не рекомендуется; красный флаг аудита).

use serde::{Deserialize, Serialize};

/// Класс риска действия.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RiskClass {
    /// Чтение/поиск — обратимо тривиально (R0+).
    ReadOnly,
    /// Изменение в рабочем каталоге — обратимо (R2+).
    Mutating,
    /// Деструктивное/необратимое, выход за контур (R4 confirm / R5 auto).
    Destructive,
}

/// Решение политики.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyDecision {
    /// Разрешено автоматически.
    Allow,
    /// Требуется подтверждение человека (headless → отказ с объяснением).
    RequireConfirm(String),
    /// Запрещено на текущем уровне автономии.
    Deny(String),
}

/// Политика автономии.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// Уровень автономии 0–5.
    pub level: u8,
}

impl Default for Policy {
    fn default() -> Self {
        Self { level: 2 }
    }
}

impl Policy {
    /// Парсит уровень из конфига (`"R2"`, `"r3"`, `"4"`).
    ///
    /// # Errors
    /// Неизвестный уровень.
    pub fn parse(s: &str) -> Result<Self, crate::error::HarnessError> {
        let digits: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
        let level: u8 = digits.parse().map_err(|_| {
            crate::error::HarnessError::Config(format!("некорректный уровень автономии '{s}' (R0–R5)"))
        })?;
        if level > 5 {
            return Err(crate::error::HarnessError::Config(format!(
                "уровень автономии R{level} недопустим (R0–R5)"
            )));
        }
        Ok(Self { level })
    }

    /// Решение по инструменту и его аргументам.
    pub fn check(&self, tool: &str, args: &serde_json::Value) -> PolicyDecision {
        let class = classify_tool(tool, args);
        match class {
            RiskClass::ReadOnly => PolicyDecision::Allow,
            RiskClass::Mutating => {
                if self.level >= 2 {
                    PolicyDecision::Allow
                } else {
                    PolicyDecision::RequireConfirm(format!(
                        "{tool}: изменяющее действие требует R2+, текущий уровень R{}",
                        self.level
                    ))
                }
            }
            RiskClass::Destructive => {
                if self.level >= 5 {
                    PolicyDecision::Allow
                } else if self.level == 4 {
                    PolicyDecision::RequireConfirm(format!(
                        "{tool}: деструктивное действие требует подтверждения человека (R4)"
                    ))
                } else {
                    PolicyDecision::Deny(format!(
                        "{tool}: деструктивное действие запрещено на уровне R{} (нужен R4+)",
                        self.level
                    ))
                }
            }
        }
    }
}

/// Классификация инструмента по риску; bash — по тексту команды.
pub fn classify_tool(tool: &str, args: &serde_json::Value) -> RiskClass {
    match tool {
        "bash" => {
            let cmd = args.get("command").and_then(|c| c.as_str()).unwrap_or("");
            classify_bash(cmd)
        }
        "write_file" | "edit_file" | "adr_new" | "handoff_create" | "harness_run"
        | "agentsmd_generate" => RiskClass::Mutating,
        _ => RiskClass::ReadOnly,
    }
}

/// Деструктивные паттерны команд (необратимые/внешние эффекты).
const DESTRUCTIVE_PATTERNS: &[&str] = &[
    "rm -rf", "rm -fr", "rm -r /", "mkfs", "dd if=", "dd of=", ":(){", "shutdown", "reboot",
    "kill -9", "pkill", "chmod -R /", "chown -R /", "> /dev/", "git push --force",
    "git push -f", "git reset --hard", "drop table", "DROP TABLE", "truncate table",
    "kubectl delete", "docker system prune", "terraform destroy", "ansible",
];

/// Изменяющие паттерны (обратимые, в рабочем контуре).
const MUTATING_PATTERNS: &[&str] = &[
    "rm ", "mv ", "cp ", "mkdir", "touch ", "sed -i", "sed -i.bak", "tee ", "> ", ">> ",
    "git add", "git commit", "git push", "git checkout", "git switch", "git merge", "git rebase",
    "cargo build", "cargo test", "cargo clippy", "cargo fmt", "npm install", "npm run", "pnpm ",
    "pip install", "mvn ", "gradle", "make ", "docker build", "kubectl apply", "curl -X POST",
    "curl -X PUT", "curl -X DELETE", "curl -d ", "wget -O",
];

/// Классификация bash-команды по тексту.
pub fn classify_bash(command: &str) -> RiskClass {
    let cmd = command.trim();
    for pat in DESTRUCTIVE_PATTERNS {
        if cmd.contains(pat) {
            return RiskClass::Destructive;
        }
    }
    for pat in MUTATING_PATTERNS {
        if cmd.contains(pat) {
            return RiskClass::Mutating;
        }
    }
    RiskClass::ReadOnly
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readonly_commands_are_always_allowed() {
        let p = Policy::default();
        for cmd in ["ls -la", "cat spec.md", "grep -r foo src/", "git status", "pwd"] {
            assert_eq!(classify_bash(cmd), RiskClass::ReadOnly, "{cmd}");
            assert_eq!(p.check("bash", &serde_json::json!({"command": cmd})), PolicyDecision::Allow);
        }
    }

    #[test]
    fn mutating_requires_r2() {
        let cmd = serde_json::json!({"command": "cargo test"});
        assert!(matches!(
            Policy::parse("R1").expect("R1").check("bash", &cmd),
            PolicyDecision::RequireConfirm(_)
        ));
        assert_eq!(
            Policy::parse("R2").expect("R2").check("bash", &cmd),
            PolicyDecision::Allow
        );
        assert!(matches!(
            Policy::parse("R0").expect("R0").check("write_file", &serde_json::json!({})),
            PolicyDecision::RequireConfirm(_)
        ));
    }

    #[test]
    fn destructive_denied_below_r4_and_confirmed_at_r4() {
        let cmd = serde_json::json!({"command": "rm -rf /tmp/x"});
        assert!(matches!(Policy::default().check("bash", &cmd), PolicyDecision::Deny(_)));
        assert!(matches!(Policy::parse("R4").unwrap().check("bash", &cmd), PolicyDecision::RequireConfirm(_)));
        assert_eq!(Policy::parse("R5").unwrap().check("bash", &cmd), PolicyDecision::Allow);
        assert!(matches!(Policy::default().check("bash", &serde_json::json!({"command": "git push --force"})), PolicyDecision::Deny(_)));
    }

    #[test]
    fn parse_validates_levels() {
        assert_eq!(Policy::parse("r3").unwrap().level, 3);
        assert!(Policy::parse("R9").is_err());
        assert!(Policy::parse("auto").is_err());
    }
}
