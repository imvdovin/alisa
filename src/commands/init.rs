use std::{
    fs,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    sync::{Arc, Mutex, MutexGuard, TryLockError},
    time::Duration,
};

use anyhow::Context;
use clap::Args;
use thiserror::Error;

mod platform;
mod prompt;
mod schema;
mod validation;

use super::{LockPolicy, WorkspaceLockError, WorkspaceLockStatus, acquire_workspace_lock};
use crate::{
    metadata::{
        self, MANIFEST_SCHEMA_VERSION, Manifest, default_project_toml, default_runtime_toml,
        default_session_state, to_pretty_json,
    },
    workspace::{self, Workspace, WorkspaceLock},
};

#[derive(Debug, Clone, Args)]
pub struct InitCliArgs {
    /// Print the planned changes without touching the filesystem
    #[arg(long)]
    pub dry_run: bool,

    /// Validate the workspace structure and schema without modifications
    #[arg(long)]
    pub check: bool,

    /// Recreate auxiliary artifacts (indices, caches)
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Error)]
pub enum InitError {
    #[error("schema mismatch: {0}")]
    SchemaMismatch(String),
    #[error("workspace lock at {lock_path} is held by another process")]
    WorkspaceLocked { lock_path: String },
    #[error("validation failed: {0}")]
    ValidationFailed(String),
    #[error("operation interrupted")]
    Interrupted,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

// NOTE: tokenizer choice is intentionally hardcoded; if it ever becomes
// configurable, the value must be validated against a whitelist.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

const PROMPT_TIMEOUT_SECS: u64 = 30;
const PROMPT_TIMEOUT: Duration = Duration::from_secs(PROMPT_TIMEOUT_SECS);

pub fn run(args: &InitCliArgs) -> Result<(), InitError> {
    INTERRUPTED.store(false, Ordering::SeqCst);
    let workspace = Workspace::detect_from_cwd().map_err(InitError::Other)?;
    let mode = determine_mode(args)?;

    let lock_policy = match &mode {
        InitMode::Check => LockPolicy::Optional,
        InitMode::Execute(opts) => {
            if opts.dry_run {
                LockPolicy::SkipIfMissing
            } else {
                LockPolicy::Required
            }
        }
    };

    let lock_handle = Arc::new(Mutex::new(
        match acquire_workspace_lock(&workspace, lock_policy) {
            Ok(WorkspaceLockStatus::Acquired(guard)) => Some(guard),
            Ok(WorkspaceLockStatus::Skipped) => None,
            Err(WorkspaceLockError::AlreadyLocked) => {
                let lock_path = workspace.lock_path();
                return Err(InitError::WorkspaceLocked {
                    lock_path: lock_path.display().to_string(),
                });
            }
            Err(WorkspaceLockError::Other(err)) => return Err(InitError::Other(err)),
        },
    ));

    let should_install_handler = mutex_option_is_some(lock_handle.as_ref());

    if should_install_handler {
        install_interrupt_handler(lock_handle.clone())?;
    }

    let result = match mode {
        InitMode::Check => validation::run_check(&workspace),
        InitMode::Execute(opts) => execute(&workspace, opts),
    };

    release_workspace_lock(&lock_handle);

    match result {
        Ok(()) => {
            check_for_interrupt()?;
            Ok(())
        }
        Err(err) => Err(err),
    }
}

fn mutex_option_is_some<T>(lock: &Mutex<Option<T>>) -> bool {
    match lock.lock() {
        Ok(guard) => guard.is_some(),
        Err(poisoned) => poisoned.into_inner().is_some(),
    }
}

fn install_interrupt_handler(
    lock_handle: Arc<Mutex<Option<WorkspaceLock>>>,
) -> Result<(), InitError> {
    ctrlc::set_handler(move || {
        INTERRUPTED.store(true, Ordering::SeqCst);
        if try_release_workspace_lock(&lock_handle) {
            eprintln!("\n[warn] Interrupt received. Released workspace lock.");
        }
    })
    .map_err(|err| InitError::Other(err.into()))
}

fn release_workspace_lock(lock_handle: &Arc<Mutex<Option<WorkspaceLock>>>) -> bool {
    match lock_handle.lock() {
        Ok(guard) => drain_workspace_lock(guard),
        Err(err) => drain_workspace_lock(err.into_inner()),
    }
}

fn try_release_workspace_lock(lock_handle: &Arc<Mutex<Option<WorkspaceLock>>>) -> bool {
    match lock_handle.try_lock() {
        Ok(guard) => drain_workspace_lock(guard),
        Err(TryLockError::Poisoned(err)) => drain_workspace_lock(err.into_inner()),
        Err(TryLockError::WouldBlock) => false,
    }
}

fn drain_workspace_lock(mut guard: MutexGuard<Option<WorkspaceLock>>) -> bool {
    guard.take().is_some()
}

fn check_for_interrupt() -> Result<(), InitError> {
    if INTERRUPTED.load(Ordering::SeqCst) {
        Err(InitError::Interrupted)
    } else {
        Ok(())
    }
}

fn interruptible<T, F>(op: F) -> Result<T, InitError>
where
    F: FnOnce() -> Result<T, InitError>,
{
    check_for_interrupt()?;
    let result = op()?;
    Ok(result)
}

fn determine_mode(args: &InitCliArgs) -> Result<InitMode, InitError> {
    if args.check {
        if args.dry_run {
            return Err(InitError::ValidationFailed(
                "--check cannot be combined with --dry-run".into(),
            ));
        }
        if args.force {
            return Err(InitError::ValidationFailed(
                "--check cannot be combined with --force".into(),
            ));
        }
        return Ok(InitMode::Check);
    }

    Ok(InitMode::Execute(InitOptions {
        dry_run: args.dry_run,
        force: args.force,
    }))
}

fn execute(workspace: &Workspace, opts: InitOptions) -> Result<(), InitError> {
    let mut reporter = InitReporter::new(opts.dry_run);

    interruptible(|| ensure_directories(workspace, &opts, &mut reporter))?;
    interruptible(|| ensure_manifest(workspace, &opts, &mut reporter))?;
    interruptible(|| ensure_gitignore(workspace, &opts, &mut reporter))?;
    interruptible(|| ensure_project_files(workspace, &opts, &mut reporter))?;
    interruptible(|| ensure_session_state(workspace, &opts, &mut reporter))?;
    interruptible(|| ensure_schema_marker(workspace, &opts, &mut reporter))?;
    interruptible(|| schema::ensure_registry_database(workspace, &opts, &mut reporter))?;
    interruptible(|| schema::ensure_audit_index_database(workspace, &opts, &mut reporter))?;
    interruptible(|| schema::ensure_rag_index_database(workspace, &opts, &mut reporter))?;

    Ok(())
}

fn ensure_directories(
    workspace: &Workspace,
    opts: &InitOptions,
    reporter: &mut InitReporter,
) -> Result<(), InitError> {
    for dir in workspace.directory_targets() {
        ensure_directory(&dir, opts, reporter)?;
    }
    Ok(())
}

fn ensure_directory(
    path: &Path,
    opts: &InitOptions,
    reporter: &mut InitReporter,
) -> Result<(), InitError> {
    if path.exists() {
        reporter.exists("Directory", path);
        return Ok(());
    }

    if opts.dry_run {
        reporter.planned("Create directory", path);
        return Ok(());
    }

    fs::create_dir_all(path)
        .with_context(|| format!("Failed to create directory {}", path.display()))
        .map_err(InitError::Other)?;
    reporter.created("Directory", path);
    Ok(())
}

fn ensure_manifest(
    workspace: &Workspace,
    opts: &InitOptions,
    reporter: &mut InitReporter,
) -> Result<(), InitError> {
    let path = workspace.manifest_path();
    let registry_path = workspace.workspace_id_registry_path();
    match metadata::read_manifest(&path) {
        Ok(Some(existing)) => {
            ensure_manifest_compatibility(&existing)?;
            if opts.dry_run {
                if !registry_path.exists() {
                    reporter.planned("Create workspace_id registry", &registry_path);
                }
            } else {
                let registry_existed = registry_path.exists();
                let updated =
                    metadata::ensure_workspace_id_recorded(&registry_path, &existing.workspace_id)
                        .map_err(InitError::Other)?;
                if updated {
                    report_workspace_id_registry_action(reporter, registry_existed, &registry_path);
                }
            }
            reporter.exists("manifest.json", &path);
        }
        Ok(None) => {
            if opts.dry_run {
                reporter.planned("Create manifest.json", &path);
                let action = if registry_path.exists() {
                    "Update workspace_id registry"
                } else {
                    "Create workspace_id registry"
                };
                reporter.planned(action, &registry_path);
            } else {
                let registry_existed = registry_path.exists();
                let workspace_id = metadata::allocate_workspace_id_and_record(&registry_path)
                    .map_err(InitError::Other)?;
                report_workspace_id_registry_action(reporter, registry_existed, &registry_path);
                let mut manifest = Manifest::fresh();
                manifest.workspace_id = workspace_id;
                metadata::write_manifest(&path, &manifest).map_err(InitError::Other)?;
                reporter.created("manifest.json", &path);
            }
        }
        Err(err) => {
            let path_for_repair = path.clone();
            let registry_path_for_repair = registry_path.clone();
            prompt::handle_corrupted_artifact(
                "manifest.json",
                &path,
                &err.to_string(),
                opts,
                reporter,
                move |reporter| {
                    let registry_existed = registry_path_for_repair.exists();
                    let workspace_id =
                        metadata::allocate_workspace_id_and_record(&registry_path_for_repair)
                            .map_err(InitError::Other)?;
                    report_workspace_id_registry_action(
                        reporter,
                        registry_existed,
                        &registry_path_for_repair,
                    );
                    let mut manifest = Manifest::fresh();
                    manifest.workspace_id = workspace_id;
                    metadata::write_manifest(&path_for_repair, &manifest)
                        .map_err(InitError::Other)?;
                    reporter.updated("manifest.json", &path_for_repair);
                    Ok(())
                },
            )?;
        }
    }
    Ok(())
}

fn report_workspace_id_registry_action(
    reporter: &mut InitReporter,
    existed_before: bool,
    path: &Path,
) {
    if existed_before {
        reporter.updated("workspace_id registry", path);
    } else {
        reporter.created("workspace_id registry", path);
    }
}

fn ensure_gitignore(
    workspace: &Workspace,
    opts: &InitOptions,
    reporter: &mut InitReporter,
) -> Result<(), InitError> {
    let path = workspace.gitignore_path();
    if path.exists() {
        reporter.exists(".gitignore", &path);
        return Ok(());
    }

    if opts.dry_run {
        reporter.planned("Create .gitignore", &path);
        return Ok(());
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to prepare directory {}", parent.display()))
            .map_err(InitError::Other)?;
    }

    fs::write(&path, workspace::DEFAULT_GITIGNORE)
        .with_context(|| format!("Failed to write .gitignore at {}", path.display()))
        .map_err(InitError::Other)?;
    reporter.created(".gitignore", &path);
    Ok(())
}

fn ensure_project_files(
    workspace: &Workspace,
    opts: &InitOptions,
    reporter: &mut InitReporter,
) -> Result<(), InitError> {
    prompt::ensure_text_file(
        &workspace.project_snapshot_path(),
        opts,
        reporter,
        "state/project.toml",
        || Ok(default_project_toml()),
        validation::validate_toml_file,
    )?;

    prompt::ensure_text_file(
        &workspace.runtime_snapshot_path(),
        opts,
        reporter,
        "state/runtime.toml",
        || Ok(default_runtime_toml()),
        validation::validate_toml_file,
    )?;

    Ok(())
}

fn ensure_session_state(
    workspace: &Workspace,
    opts: &InitOptions,
    reporter: &mut InitReporter,
) -> Result<(), InitError> {
    prompt::ensure_text_file(
        &workspace.session_state_path(),
        opts,
        reporter,
        "state/session/current.json",
        || to_pretty_json(&default_session_state()).map_err(InitError::Other),
        validation::validate_json_file,
    )
}

fn ensure_schema_marker(
    workspace: &Workspace,
    opts: &InitOptions,
    reporter: &mut InitReporter,
) -> Result<(), InitError> {
    let path = workspace.schema_version_path();

    if path.exists() {
        let content = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read schema version at {}", path.display()))
            .map_err(InitError::Other)?;
        ensure_schema_version_matches(content.trim(), schema_mismatch_error)?;
        reporter.exists("migrations/version.txt", &path);
        return Ok(());
    }

    if opts.dry_run {
        reporter.planned("Create migrations/version.txt", &path);
        return Ok(());
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to prepare directory {}", parent.display()))
            .map_err(InitError::Other)?;
    }

    fs::write(&path, format!("{}\n", MANIFEST_SCHEMA_VERSION))
        .with_context(|| format!("Failed to write schema version at {}", path.display()))
        .map_err(InitError::Other)?;
    reporter.created("migrations/version.txt", &path);
    Ok(())
}

fn ensure_manifest_compatibility(manifest: &Manifest) -> Result<(), InitError> {
    ensure_schema_version_matches(&manifest.schema_version, schema_mismatch_error)
}

fn ensure_schema_version_matches<E, F>(version: &str, err_mapper: F) -> Result<(), E>
where
    F: FnOnce(&str, &str) -> E,
{
    let version = version.trim();
    if version == MANIFEST_SCHEMA_VERSION {
        Ok(())
    } else {
        Err(err_mapper(version, MANIFEST_SCHEMA_VERSION))
    }
}

fn schema_mismatch_error(found: &str, expected: &str) -> InitError {
    InitError::SchemaMismatch(format!(
        "Workspace schema version {found} is incompatible with {expected}"
    ))
}

#[derive(Debug, Clone, Copy)]
struct InitOptions {
    dry_run: bool,
    force: bool,
}

#[derive(Debug)]
enum InitMode {
    Check,
    Execute(InitOptions),
}

struct InitReporter {
    dry_run: bool,
    changes_recorded: bool,
    summary_emitted: bool,
}

impl InitReporter {
    fn new(dry_run: bool) -> Self {
        Self {
            dry_run,
            changes_recorded: false,
            summary_emitted: false,
        }
    }

    fn planned(&mut self, label: &str, path: &Path) {
        self.changes_recorded = true;
        println!("[plan] {label}: {}", path.display());
    }

    fn created(&mut self, label: &str, path: &Path) {
        self.changes_recorded = true;
        println!("[create] {label}: {}", path.display());
    }

    fn updated(&mut self, label: &str, path: &Path) {
        self.changes_recorded = true;
        println!("[update] {label}: {}", path.display());
    }

    fn exists(&self, label: &str, path: &Path) {
        println!("[ok] {label}: {} (already present)", path.display());
    }

    fn skipped(&self, label: &str, path: &Path) {
        eprintln!(
            "[skip] {label}: {} (left unchanged at user's request)",
            path.display()
        );
    }

    fn summarize(&mut self) {
        if self.summary_emitted {
            return;
        }
        self.summary_emitted = true;

        if !self.changes_recorded {
            if self.dry_run {
                println!("[plan] Workspace already satisfies all requirements.");
            } else {
                println!("[ok] Workspace already satisfies all requirements.");
            }
        }
    }
}

impl Drop for InitReporter {
    fn drop(&mut self) {
        self.summarize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;
    use std::{
        panic::{self, AssertUnwindSafe},
        sync::{Arc, Mutex},
    };
    use std::sync::atomic::Ordering;
    use tempfile::tempdir;

    static INTERRUPT_TEST_GUARD: Mutex<()> = Mutex::new(());

    fn with_interrupt_guard<F: FnOnce()>(f: F) {
        let _guard = INTERRUPT_TEST_GUARD.lock().expect("interrupt guard");
        INTERRUPTED.store(false, Ordering::SeqCst);
        f();
        INTERRUPTED.store(false, Ordering::SeqCst);
    }

    #[test]
    fn interruptible_short_circuits_when_flag_is_set() {
        with_interrupt_guard(|| {
            INTERRUPTED.store(true, Ordering::SeqCst);

            let mut executed = false;
            let result = interruptible(|| {
                executed = true;
                Ok(())
            });

            assert!(!executed, "operation must not execute once interrupt is raised");
            match result {
                Err(InitError::Interrupted) => {}
                other => panic!("expected interrupt error, got {:?}", other),
            }
        });
    }

    #[test]
    fn interruptible_reports_interrupt_on_next_entry() {
        with_interrupt_guard(|| {
            interruptible(|| {
                INTERRUPTED.store(true, Ordering::SeqCst);
                Ok(())
            })
            .expect("first operation completes before interrupt is observed");

            match interruptible(|| Ok(())) {
                Err(InitError::Interrupted) => {}
                other => panic!("pending interrupt must be reported, got {:?}", other),
            }
        });
    }

    #[test]
    fn mutex_option_is_some_returns_true_when_value_present() {
        let lock = Mutex::new(Some(()));
        assert!(mutex_option_is_some(&lock));
    }

    #[test]
    fn mutex_option_is_some_returns_false_when_empty() {
        let lock = Mutex::new(None::<()>);
        assert!(!mutex_option_is_some(&lock));
    }

    #[test]
    fn mutex_option_is_some_handles_poisoned_lock() {
        let lock = Mutex::new(Some(()));
        let _ = panic::catch_unwind(AssertUnwindSafe(|| {
            let _guard = lock.lock().unwrap();
            panic!("poison");
        }));

        assert!(mutex_option_is_some(&lock));
    }

    #[test]
    fn try_release_workspace_lock_handles_busy_mutex() {
        let temp = tempdir().expect("temp dir");
        let workspace = Workspace::new(temp.path());
        let guard = workspace
            .try_acquire_lock()
            .expect("lock attempt")
            .expect("initial lock must succeed");

        let lock_handle = Arc::new(Mutex::new(Some(guard)));
        let blocking_guard = lock_handle.lock().expect("mutex lock");
        let cloned = lock_handle.clone();
        assert!(
            !try_release_workspace_lock(&cloned),
            "try_release must not block and must report no release when mutex is held"
        );
        drop(blocking_guard);
        assert!(
            try_release_workspace_lock(&lock_handle),
            "lock must be released once mutex becomes available"
        );
    }

    #[test]
    fn try_release_workspace_lock_releases_when_available() {
        let temp = tempdir().expect("temp dir");
        let workspace = Workspace::new(temp.path());
        let guard = workspace
            .try_acquire_lock()
            .expect("lock attempt")
            .expect("initial lock must succeed");

        let lock_handle = Arc::new(Mutex::new(Some(guard)));
        assert!(
            try_release_workspace_lock(&lock_handle),
            "try_release should drop the guard when mutex is free"
        );
        assert!(
            !try_release_workspace_lock(&lock_handle),
            "subsequent releases should report no-op"
        );
    }
}
