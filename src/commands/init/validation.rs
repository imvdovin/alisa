use std::path::Path;

use crate::{metadata, workspace::Workspace};

use super::{
    InitError, ensure_manifest_compatibility, ensure_schema_version_matches, interruptible, schema,
};

pub(super) fn run_check(workspace: &Workspace) -> Result<(), InitError> {
    let mut report = ValidationReport::new();

    interruptible(|| {
        if !workspace.workspace_root().exists() {
            report.missing("workspace directory", &workspace.workspace_root());
        }
        Ok(())
    })?;

    interruptible(|| {
        match metadata::read_manifest(&workspace.manifest_path()) {
            Ok(Some(manifest)) => {
                ensure_manifest_compatibility(&manifest)?;
            }
            Ok(None) => report.missing("manifest", &workspace.manifest_path()),
            Err(err) => report.push(format!(
                "Malformed manifest at {}: {err}",
                workspace.manifest_path().display()
            )),
        }
        Ok(())
    })?;

    interruptible(|| {
        if let Err(issue) = validate_schema_marker(workspace) {
            report.push(issue);
        }
        Ok(())
    })?;

    for dir in workspace.directory_targets() {
        interruptible(|| {
            if !dir.exists() {
                report.missing("directory", &dir);
            }
            Ok(())
        })?;
    }

    interruptible(|| {
        check_file_presence(&workspace.gitignore_path(), "gitignore", &mut report);
        check_file_presence(
            &workspace.project_snapshot_path(),
            "state/project.toml",
            &mut report,
        );
        check_file_presence(
            &workspace.runtime_snapshot_path(),
            "state/runtime.toml",
            &mut report,
        );
        check_file_presence(
            &workspace.session_state_path(),
            "state/session/current.json",
            &mut report,
        );
        check_file_presence(
            &workspace.registry_path(),
            "state/registry.sqlite",
            &mut report,
        );
        check_file_presence(
            &workspace.audit_index_path(),
            "audit/audit_index.sqlite",
            &mut report,
        );
        check_file_presence(
            &workspace.rag_index_path(),
            "cache/rag/index.sqlite",
            &mut report,
        );
        Ok(())
    })?;

    interruptible(|| {
        validate_content_if_present(
            &workspace.project_snapshot_path(),
            validate_toml_file,
            &mut report,
        );
        Ok(())
    })?;

    interruptible(|| {
        validate_content_if_present(
            &workspace.runtime_snapshot_path(),
            validate_toml_file,
            &mut report,
        );
        Ok(())
    })?;

    interruptible(|| {
        validate_content_if_present(
            &workspace.session_state_path(),
            validate_json_file,
            &mut report,
        );
        Ok(())
    })?;

    interruptible(|| {
        if let Err(issue) = validate_registry_schema(workspace) {
            report.push(issue);
        }
        Ok(())
    })?;

    interruptible(|| {
        if let Err(issue) = validate_audit_schema(workspace) {
            report.push(issue);
        }
        Ok(())
    })?;

    interruptible(|| {
        if let Err(issue) = validate_rag_schema(workspace) {
            report.push(issue);
        }
        Ok(())
    })?;

    interruptible(|| report.finish())
}

pub(super) fn validate_toml_file(path: &Path) -> Result<(), String> {
    let contents = std::fs::read_to_string(path)
        .map_err(|err| format!("Failed to read TOML at {}: {err}", path.display()))?;
    toml::from_str::<toml::Value>(&contents)
        .map(|_| ())
        .map_err(|err| format!("Failed to parse TOML at {}: {err}", path.display()))
}

pub(super) fn validate_json_file(path: &Path) -> Result<(), String> {
    let contents = std::fs::read_to_string(path)
        .map_err(|err| format!("Failed to read JSON at {}: {err}", path.display()))?;
    serde_json::from_str::<serde_json::Value>(&contents)
        .map(|_| ())
        .map_err(|err| format!("Failed to parse JSON at {}: {err}", path.display()))
}

#[derive(Default)]
struct ValidationReport {
    issues: Vec<String>,
}

impl ValidationReport {
    fn new() -> Self {
        Self::default()
    }

    fn missing(&mut self, label: &str, path: &Path) {
        self.issues
            .push(format!("Missing {label}: {}", path.display()));
    }

    fn push(&mut self, message: String) {
        self.issues.push(message);
    }

    fn finish(self) -> Result<(), InitError> {
        if self.issues.is_empty() {
            println!("[ok] Workspace structure is valid.");
            Ok(())
        } else {
            for issue in &self.issues {
                eprintln!("[plan] {issue}");
            }
            Err(InitError::ValidationFailed(self.issues.join("\n")))
        }
    }
}

fn check_file_presence(path: &Path, label: &str, report: &mut ValidationReport) {
    if !path.exists() {
        report.missing(label, path);
    }
}

fn validate_content_if_present<F>(path: &Path, validator: F, report: &mut ValidationReport)
where
    F: Fn(&Path) -> Result<(), String>,
{
    if path.exists() {
        if let Err(issue) = validator(path) {
            report.push(issue);
        }
    }
}

fn validate_schema_marker(workspace: &Workspace) -> Result<(), String> {
    let path = workspace.schema_version_path();
    if !path.exists() {
        return Err(format!("Missing schema marker at {}", path.display()));
    }

    let content = std::fs::read_to_string(&path)
        .map_err(|err| format!("Failed to read schema marker {}: {err}", path.display()))?;
    ensure_schema_version_matches(content.trim(), |found, expected| {
        format!("Schema marker reports {found}, expected {expected}")
    })
}

fn validate_registry_schema(workspace: &Workspace) -> Result<(), String> {
    schema::validate_sqlite_tables(
        &workspace.registry_path(),
        schema::REGISTRY_TABLES,
        "registry database",
    )
}

fn validate_audit_schema(workspace: &Workspace) -> Result<(), String> {
    schema::validate_sqlite_tables(
        &workspace.audit_index_path(),
        schema::AUDIT_TABLES,
        "audit index",
    )
}

fn validate_rag_schema(workspace: &Workspace) -> Result<(), String> {
    schema::validate_sqlite_tables(&workspace.rag_index_path(), schema::RAG_TABLES, "RAG index")
}
