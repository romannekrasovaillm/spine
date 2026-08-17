//! Интеграционные тесты CLI-контракта `arch` (ревью `SPINE-REVIEW.md`,
//! находка F-7 / задача P0-5; решения — `docs/adr/ADR-005-ci-and-cli-tests.md`).
//!
//! Все тесты детерминированы и офлайн: дом изолирован в tempdir
//! (см. [`common::arch_cmd`]), живые LLM и кодовые харнессы не вызываются.

mod common;

use std::path::{Path, PathBuf};

use predicates::prelude::*;
use predicates::str::contains;

use common::arch_cmd;

/// Пишет исполняемый shell-скрипт фейкового кодового харнесса: печатает
/// в stdout headless JSON-контракт результата (fenced json-блок со
/// `status`) и завершается нулём. Возвращает путь к скрипту.
fn write_fake_harness(dir: &Path, contract_json: &str) -> PathBuf {
    let script = dir.join("fake-harness.sh");
    let body = format!("#!/bin/sh\necho '```json'\necho '{contract_json}'\necho '```'\n");
    std::fs::write(&script, body).expect("запись fake-харнесса");
    // +x: без права на исполнение spawn вернёт PermissionDenied.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut perms = std::fs::metadata(&script)
            .expect("stat скрипта")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).expect("chmod +x");
    }
    script
}

/// Готовая команда `arch harness-run fake`: контракт `contract_json`
/// печатает фейковый харнесс, прописанный в тестовом config.toml.
fn harness_run_cmd(home: &Path, contract_json: &str) -> assert_cmd::Command {
    let script = write_fake_harness(home, contract_json);
    // Харнесс-адаптер собирается из конфига: бинарь — наш скрипт, задача
    // уходит в stdin (скрипт её игнорирует), авто-коммит выключен (репо —
    // не git), таймауты малые, чтобы зависший прогон падал быстро.
    let config = home.join("config.toml");
    let text = format!(
        "[harnesses.fake]\nbinary = '{}'\nprompt_mode = 'stdin'\n\
         timeout_secs = 30\nidle_timeout_secs = 0\nauto_commit = false\n",
        script.display()
    );
    std::fs::write(&config, text).expect("запись config.toml");
    let repo = home.join("repo");
    std::fs::create_dir_all(&repo).expect("mkdir repo");
    let mut cmd = arch_cmd(home);
    cmd.arg("--config")
        .arg(config.as_os_str())
        .arg("harness-run")
        .arg("fake")
        .arg("--repo")
        .arg(repo.as_os_str())
        .arg("--task")
        .arg("тестовая задача");
    cmd
}

/// CONSTRAINTS.yaml с одним правилом `file_exists` уровня error.
fn constraints_yaml(path: &str) -> String {
    format!(
        "rules:\n  - name: spine_present\n    type: file_exists\n    path: \"{path}\"\n    severity: error\n"
    )
}

/// Репозиторий-фикстура с `.arch-handoff/CONSTRAINTS.yaml`.
fn repo_with_constraints(home: &Path, constraints: &str) -> PathBuf {
    let repo = home.join("repo");
    let handoff = repo.join(".arch-handoff");
    std::fs::create_dir_all(&handoff).expect("mkdir .arch-handoff");
    std::fs::write(handoff.join("CONSTRAINTS.yaml"), constraints).expect("запись CONSTRAINTS.yaml");
    repo
}

/// `arch init` в изолированном доме создаёт конфиг и ассеты; повторный
/// запуск не затирает пользовательские правки (F-7: идемпотентность init).
#[test]
fn init_creates_config_and_assets_and_preserves_user_edits() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path();

    arch_cmd(home)
        .arg("init")
        .assert()
        .success()
        .stdout(contains("Инициализация завершена"));

    let config = home.join(".config/arch-harness/config.toml");
    let asset = home.join(".arch-harness/assets/prompts/architect.md");
    assert!(config.is_file(), "конфиг создан: {}", config.display());
    assert!(asset.is_file(), "ассет развёрнут: {}", asset.display());

    // Пользовательские правки: маркер в ассете и смена модели по умолчанию.
    let mut edited = std::fs::read_to_string(&asset).expect("read asset");
    edited.push_str("\n<!-- правка пользователя -->\n");
    std::fs::write(&asset, edited).expect("write asset");
    let cfg_text = std::fs::read_to_string(&config).expect("read config");
    let cfg_text = cfg_text.replace("default_model = \"deepseek\"", "default_model = \"glm\"");
    assert!(
        cfg_text.contains("default_model = \"glm\""),
        "правка конфига применилась до повторного init"
    );
    std::fs::write(&config, cfg_text).expect("write config");

    arch_cmd(home).arg("init").assert().success();

    let asset_after = std::fs::read_to_string(&asset).expect("re-read asset");
    assert!(
        asset_after.contains("<!-- правка пользователя -->"),
        "повторный init затёр правку ассета"
    );
    let cfg_after = std::fs::read_to_string(&config).expect("re-read config");
    assert!(
        cfg_after.contains("default_model = \"glm\""),
        "повторный init потерял пользовательскую модель:\n{cfg_after}"
    );
}

/// `arch control check` на падающем CONSTRAINTS.yaml (обязательный файл
/// отсутствует, severity error) → отчёт FAIL и exit 1 (скриптовый гейт
/// fitness-функций).
#[test]
fn control_check_failing_constraints_exits_1() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = repo_with_constraints(tmp.path(), &constraints_yaml("docs/ARCHITECTURE-SPINE.md"));

    let mut cmd = arch_cmd(tmp.path());
    cmd.arg("control").arg("check").arg(repo.as_os_str());
    cmd.assert()
        .code(1)
        .stdout(contains("Итог: FAIL"))
        .stdout(contains("spine_present"));
}

/// Проходящий CONSTRAINTS.yaml (обязательный файл на месте) → PASS, exit 0.
#[test]
fn control_check_passing_constraints_exits_0() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = repo_with_constraints(tmp.path(), &constraints_yaml("docs/ARCHITECTURE-SPINE.md"));
    let docs = repo.join("docs");
    std::fs::create_dir_all(&docs).expect("mkdir docs");
    std::fs::write(docs.join("ARCHITECTURE-SPINE.md"), "# Spine\n").expect("write spine");

    let mut cmd = arch_cmd(tmp.path());
    cmd.arg("control").arg("check").arg(repo.as_os_str());
    cmd.assert().success().stdout(contains("Итог: PASS"));
}

/// `harness-run` со `status=blocked` в контракте → exit 2 (скриптовый гейт
/// в пайпах, см. `docs/harness_integrations.md`).
#[test]
fn harness_run_blocked_contract_exits_2() {
    let tmp = tempfile::tempdir().expect("tempdir");
    harness_run_cmd(
        tmp.path(),
        r#"{"status": "blocked", "assumptions": [], "open_questions": ["нужен доступ к КШД"], "conflicts_with_prior_decisions": []}"#,
    )
    .assert()
    .code(2)
    .stdout(contains("status=blocked"));
}

/// `harness-run` с непустыми `conflicts_with_prior_decisions` → exit 3
/// (конфликт со spine останавливает интеграцию по контракту).
#[test]
fn harness_run_conflicts_exit_3() {
    let tmp = tempfile::tempdir().expect("tempdir");
    harness_run_cmd(
        tmp.path(),
        r#"{"status": "complete", "conflicts_with_prior_decisions": ["AD-2 запрещает vendor lock-in"]}"#,
    )
    .assert()
    .code(3)
    .stdout(contains("conflicts=1"));
}

/// `harness-run` с чистым `complete` (списки пусты) → exit 0.
#[test]
fn harness_run_complete_exits_0() {
    let tmp = tempfile::tempdir().expect("tempdir");
    harness_run_cmd(tmp.path(), r#"{"status": "complete"}"#)
        .assert()
        .success()
        .stdout(contains("status=complete"));
}

/// `arch mermaid` рендерит пример из репозитория без единого ключа
/// (no-LLM смоук из AGENTS.md).
#[test]
fn mermaid_renders_example_without_keys() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let diagram = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/mermaid/flow.mmd");
    let mut cmd = arch_cmd(tmp.path());
    cmd.arg("mermaid").arg(diagram.as_os_str());
    cmd.assert().success().stdout(contains("API Gateway"));
}

/// `arch doctor` без единого API-ключа в окружении не падает: отчёт
/// рендерится полностью, без паники и трейса ошибки в stderr. Код 1 —
/// задокументированный контракт (Fail «нет ключа модели по умолчанию»,
/// `src/doctor.rs`; отступление от буквы `DoD` ревью — ADR-005 §7).
#[test]
fn doctor_without_keys_reports_problems_and_exits_1() {
    let tmp = tempfile::tempdir().expect("tempdir");
    arch_cmd(tmp.path())
        .arg("doctor")
        .assert()
        .code(1)
        .stdout(contains("arch doctor"))
        .stdout(contains("нет ключа модели по умолчанию"))
        .stdout(contains("Итог:"))
        .stderr(contains("Error:").not());
}

/// Синтетический кейс для `arch trace check`: `model/` с одним AD,
/// CONSTRAINTS.yaml, spine. `with_rule` — связывает AD с правилом C-001.
fn trace_case(home: &Path, with_rule: bool) -> PathBuf {
    let case = home.join("case");
    let model = case.join("model");
    std::fs::create_dir_all(&model).expect("mkdir model");
    let verified = if with_rule {
        "verified_by: [C-001]"
    } else {
        ""
    };
    std::fs::write(
        model.join("AD-1.md"),
        format!("---\nid: AD-1\ntype: ad\ntitle: Инвариант\nstatus: ADOPTED\n{verified}\n---\n"),
    )
    .expect("write AD");
    std::fs::write(
        case.join("CONSTRAINTS.yaml"),
        "constraints:\n  - id: C-001\n    name: правило\n",
    )
    .expect("write constraints");
    std::fs::write(case.join("ARCHITECTURE-SPINE.md"), "## AD-1: Инвариант\n")
        .expect("write spine");
    case
}

/// `arch trace check`: спайн покрыт правилом → PASS, exit 0 (ADR-006).
#[test]
fn trace_check_covered_spine_exits_0() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let case = trace_case(tmp.path(), true);
    let mut cmd = arch_cmd(tmp.path());
    cmd.arg("trace").arg("check").arg(case.as_os_str());
    cmd.assert()
        .success()
        .stdout(contains("AD → fitness-правило | 1/1 | 100%"))
        .stdout(contains("Итог: PASS"));
}

/// `arch trace check`: AD без правила и без `unverifiable` → FAIL, exit 1
/// (скриптовый гейт CI флота, ADR-006).
#[test]
fn trace_check_uncovered_ad_exits_1() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let case = trace_case(tmp.path(), false);
    let mut cmd = arch_cmd(tmp.path());
    cmd.arg("trace").arg("check").arg(case.as_os_str());
    cmd.assert()
        .code(1)
        .stdout(contains("ad-not-verified"))
        .stdout(contains("Итог: FAIL"));
}

/// Синтетический кейс для `arch nfr`: `model/` с INT-hop'ом и NFR с целью
/// p99. `hop_budget_ms` — бюджет hop'а (None — hop без бюджета).
fn nfr_budget_case(home: &Path, target_ms: u32, hop_budget_ms: Option<u32>) -> PathBuf {
    let case = home.join("nfr-case");
    let model = case.join("model");
    std::fs::create_dir_all(&model).expect("mkdir model");
    let budget = hop_budget_ms.map_or(String::new(), |b| format!("latency_budget_ms: {b}\n"));
    std::fs::write(
        model.join("INT-001-hop.md"),
        format!("---\nid: INT-001\ntype: int\ntitle: Hop\nstatus: accepted\n{budget}---\n"),
    )
    .expect("write INT");
    std::fs::write(
        model.join("NFR-001-lat.md"),
        format!(
            "---\nid: NFR-001\ntype: nfr\ntitle: Latency\nstatus: accepted\n\
             verification: histogram\np99_target_ms: {target_ms}\naffects: [INT-001]\n---\n"
        ),
    )
    .expect("write NFR");
    case
}

/// `arch nfr budget`: сумма hop'ов в пределах цели p99 → PASS, exit 0 (ADR-007).
#[test]
fn nfr_budget_converging_exits_0() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let case = nfr_budget_case(tmp.path(), 2000, Some(800));
    let mut cmd = arch_cmd(tmp.path());
    cmd.arg("nfr").arg("budget").arg(case.as_os_str());
    cmd.assert()
        .success()
        .stdout(contains("резерв: 1200 мс"))
        .stdout(contains("Итог: PASS"));
}

/// `arch nfr budget`: сумма hop'ов выше цели p99 → error с виновными hop'ами,
/// exit 1 (DoD P1-1, скриптовый гейт).
#[test]
fn nfr_budget_exceeded_exits_1_with_guilty_hops() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let case = nfr_budget_case(tmp.path(), 2000, Some(3000));
    let mut cmd = arch_cmd(tmp.path());
    cmd.arg("nfr").arg("budget").arg(case.as_os_str());
    cmd.assert()
        .code(1)
        .stdout(contains("budget-exceeded"))
        .stdout(contains("INT-001=3000"))
        .stdout(contains("Итог: FAIL"));
}

/// `arch nfr budget`: hop без заявленного бюджета → error, exit 1.
#[test]
fn nfr_budget_missing_hop_budget_exits_1() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let case = nfr_budget_case(tmp.path(), 2000, None);
    let mut cmd = arch_cmd(tmp.path());
    cmd.arg("nfr").arg("budget").arg(case.as_os_str());
    cmd.assert()
        .code(1)
        .stdout(contains("budget-hop-missing"))
        .stdout(contains("INT-001"))
        .stdout(contains("Итог: FAIL"));
}
