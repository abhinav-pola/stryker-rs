//! End-to-end: run the built binary against the bun fixture project.
//! Skips (with a message) when `bun` is not installed.

use std::process::Command;

fn bun_available() -> bool {
    Command::new("bun").arg("--version").output().is_ok_and(|o| o.status.success())
}

fn fixture_dir(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures").join(name)
}

#[test]
fn bun_fixture_end_to_end() {
    if !bun_available() {
        eprintln!("skipping: bun not installed");
        return;
    }
    let dir = fixture_dir("bun-project");
    let report_path = dir.join("reports/mutation/bun.json");
    let _ = std::fs::remove_file(&report_path);

    let output = assert_cmd::Command::cargo_bin("stryker")
        .unwrap()
        .current_dir(&dir)
        .args(["run", "--config", "stryker.bun.config.json", "--force-dirty"])
        .timeout(std::time::Duration::from_secs(300))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "run failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Tree must be pristine (no instrumentation header left behind).
    let calc = std::fs::read_to_string(dir.join("src/calc.ts")).unwrap();
    assert!(!calc.contains("stryMutAct"), "source not restored");

    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&report_path).unwrap()).unwrap();
    assert_eq!(report["schemaVersion"], "2");
    let mutants = report["files"]["src/calc.ts"]["mutants"].as_array().unwrap();
    assert!(!mutants.is_empty());

    // Planted outcomes: at least one Killed, the equivalent `i >= 0` mutant
    // Survived, untestedMax mutants NoCoverage, infinite loop Timeout.
    let statuses: Vec<&str> =
        mutants.iter().map(|m| m["status"].as_str().unwrap()).collect();
    for expected in ["Killed", "Survived", "NoCoverage", "Timeout"] {
        assert!(statuses.contains(&expected), "missing status {expected}: {statuses:?}");
    }

    // killedBy carries exact test ids on at least one mutant.
    assert!(
        mutants.iter().any(|m| m["killedBy"]
            .as_array()
            .is_some_and(|k| !k.is_empty() && k[0].as_str().unwrap().contains(" > "))),
        "no mutant has killedBy test ids"
    );
}
