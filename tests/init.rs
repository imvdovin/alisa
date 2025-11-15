use predicates::str::contains;
use rusqlite::Connection;
use serde_json::Value;
use std::{fs, path::Path};
use tempfile::tempdir;

#[test]
fn init_creates_workspace_structure() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;

    assert_cmd::cargo::cargo_bin_cmd!("alisa")
        .current_dir(temp.path())
        .arg("init")
        .assert()
        .success();

    let workspace_root = temp.path().join(".alisa");
    assert!(workspace_root.exists(), "workspace directory must exist");

    assert_file(workspace_root.join("manifest.json"));
    assert_file(workspace_root.join(".gitignore"));
    assert_file(workspace_root.join("state/project.toml"));
    assert_file(workspace_root.join("state/runtime.toml"));
    assert_file(workspace_root.join("state/session/current.json"));
    assert_file(workspace_root.join("state/registry.sqlite"));
    assert_file(workspace_root.join("audit/audit_index.sqlite"));
    assert_file(workspace_root.join("cache/rag/index.sqlite"));
    assert_file(workspace_root.join("migrations/version.txt"));

    // Manifest should contain the required fields per specification.
    let manifest: Value = serde_json::from_slice(&fs::read(workspace_root.join("manifest.json"))?)?;
    assert!(
        manifest.get("workspace_id").is_some(),
        "workspace_id must exist"
    );
    assert_eq!(
        manifest.get("schema_version"),
        Some(&Value::String("1.0".into()))
    );

    Ok(())
}

#[test]
fn registry_contains_content_column() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;

    assert_cmd::cargo::cargo_bin_cmd!("alisa")
        .current_dir(temp.path())
        .arg("init")
        .assert()
        .success();

    let registry_path = temp.path().join(".alisa/state/registry.sqlite");
    let conn = Connection::open(&registry_path)?;
    let mut stmt = conn.prepare("PRAGMA table_info(tasks)")?;
    let mut rows = stmt.query([])?;
    let mut has_content = false;
    while let Some(row) = rows.next()? {
        let column: String = row.get(1)?; // column name
        if column == "content" {
            has_content = true;
            break;
        }
    }

    assert!(
        has_content,
        "tasks table in registry {} is missing 'content' column",
        registry_path.display()
    );

    Ok(())
}

#[test]
fn init_generates_unique_workspace_ids() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;

    assert_cmd::cargo::cargo_bin_cmd!("alisa")
        .current_dir(temp.path())
        .arg("init")
        .assert()
        .success();

    let workspace_root = temp.path().join(".alisa");
    let manifest_path = workspace_root.join("manifest.json");
    let registry_path = workspace_root.join("state/workspace_ids.json");

    let first_manifest: Value = serde_json::from_slice(&fs::read(&manifest_path)?)?;
    let first_id = first_manifest["workspace_id"]
        .as_str()
        .expect("workspace_id string")
        .to_owned();

    fs::remove_file(&manifest_path)?;

    assert_cmd::cargo::cargo_bin_cmd!("alisa")
        .current_dir(temp.path())
        .arg("init")
        .assert()
        .success();

    let second_manifest: Value = serde_json::from_slice(&fs::read(&manifest_path)?)?;
    let second_id = second_manifest["workspace_id"]
        .as_str()
        .expect("workspace_id string")
        .to_owned();

    assert_ne!(
        first_id, second_id,
        "manifest recreation must generate new workspace_id"
    );

    let registry_ids: Vec<String> = serde_json::from_slice(&fs::read(&registry_path)?)?;
    assert!(registry_ids.iter().any(|id| id == &first_id));
    assert!(registry_ids.iter().any(|id| id == &second_id));

    Ok(())
}

#[test]
fn dry_run_does_not_create_files() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;

    assert_cmd::cargo::cargo_bin_cmd!("alisa")
        .current_dir(temp.path())
        .args(["init", "--dry-run"])
        .assert()
        .success()
        .stdout(contains("[plan]"));

    assert!(
        !temp.path().join(".alisa").exists(),
        "dry-run must not create .alisa"
    );

    Ok(())
}

#[test]
fn init_recovers_corrupted_registry_after_confirmation() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;

    // First initialization to create the workspace.
    assert_cmd::cargo::cargo_bin_cmd!("alisa")
        .current_dir(temp.path())
        .arg("init")
        .assert()
        .success();

    let registry_path = temp.path().join(".alisa/state/registry.sqlite");
    fs::write(&registry_path, b"not a sqlite database")?;

    assert_cmd::cargo::cargo_bin_cmd!("alisa")
        .current_dir(temp.path())
        .arg("init")
        .write_stdin("Y\n")
        .assert()
        .success()
        .stdout(contains("registry database"));

    // Validation should now succeed after the repair.
    assert_cmd::cargo::cargo_bin_cmd!("alisa")
        .current_dir(temp.path())
        .args(["init", "--check"])
        .assert()
        .success();

    Ok(())
}

#[test]
fn check_reports_corrupted_registry() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;

    assert_cmd::cargo::cargo_bin_cmd!("alisa")
        .current_dir(temp.path())
        .arg("init")
        .assert()
        .success();

    let registry_path = temp.path().join(".alisa/state/registry.sqlite");
    fs::write(&registry_path, b"still broken")?;

    assert_cmd::cargo::cargo_bin_cmd!("alisa")
        .current_dir(temp.path())
        .args(["init", "--check"])
        .assert()
        .failure()
        .stderr(contains("registry"));

    Ok(())
}

#[test]
fn check_rejects_invalid_workspace_id() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;

    assert_cmd::cargo::cargo_bin_cmd!("alisa")
        .current_dir(temp.path())
        .arg("init")
        .assert()
        .success();

    let manifest_path = temp.path().join(".alisa/manifest.json");
    let mut manifest: Value = serde_json::from_slice(&fs::read(&manifest_path)?)?;
    manifest["workspace_id"] = Value::String("ws_INVALID".into());
    fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)?;

    assert_cmd::cargo::cargo_bin_cmd!("alisa")
        .current_dir(temp.path())
        .args(["init", "--check"])
        .assert()
        .failure()
        .stderr(contains("workspace_id"));

    Ok(())
}

fn assert_file(path: impl AsRef<Path>) {
    assert!(
        path.as_ref().exists(),
        "{} must exist",
        path.as_ref().display()
    );
}
