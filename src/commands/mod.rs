use std::fmt;

use anyhow::Error;

use crate::workspace::{Workspace, WorkspaceLock};

pub mod init;

/// Policy describing when workspace lock should be attempted.
#[derive(Debug, Clone, Copy)]
pub enum LockPolicy {
    /// Lock must always be acquired (used for mutating commands that create workspace if needed).
    Required,
    /// Lock is best-effort: attempt it only when workspace already exists.
    Optional,
    /// Lock is taken only for existing workspaces; missing workspaces should be skipped entirely.
    SkipIfMissing,
}

impl LockPolicy {
    fn should_attempt_lock(&self, workspace_exists: bool) -> bool {
        match self {
            LockPolicy::Required => true,
            LockPolicy::Optional | LockPolicy::SkipIfMissing => workspace_exists,
        }
    }
}

/// Result of attempting to lock the workspace.
pub enum WorkspaceLockStatus {
    /// Lock was acquired and must be held while the command runs.
    Acquired(WorkspaceLock),
    /// Lock was intentionally skipped (e.g. workspace missing on read-only command).
    Skipped,
}

impl fmt::Debug for WorkspaceLockStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WorkspaceLockStatus::Acquired(_) => f.write_str("Acquired(<workspace lock>)"),
            WorkspaceLockStatus::Skipped => f.write_str("Skipped"),
        }
    }
}

/// Errors that can arise while acquiring the workspace lock.
#[derive(Debug)]
pub enum WorkspaceLockError {
    AlreadyLocked,
    Other(Error),
}

pub fn acquire_workspace_lock(
    workspace: &Workspace,
    policy: LockPolicy,
) -> Result<WorkspaceLockStatus, WorkspaceLockError> {
    let workspace_exists = workspace.workspace_root().exists();
    if !policy.should_attempt_lock(workspace_exists) {
        return Ok(WorkspaceLockStatus::Skipped);
    }

    match workspace.try_acquire_lock() {
        Ok(Some(lock)) => Ok(WorkspaceLockStatus::Acquired(lock)),
        Ok(None) => Err(WorkspaceLockError::AlreadyLocked),
        Err(err) => Err(WorkspaceLockError::Other(err)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn optional_skips_when_workspace_missing() {
        let temp = tempdir().unwrap();
        let workspace = Workspace::new(temp.path());

        assert!(matches!(
            acquire_workspace_lock(&workspace, LockPolicy::Optional).unwrap(),
            WorkspaceLockStatus::Skipped
        ));
    }

    #[test]
    fn skip_if_missing_behaves_as_expected() {
        let temp = tempdir().unwrap();
        let workspace = Workspace::new(temp.path());

        // Missing workspace -> skip
        assert!(matches!(
            acquire_workspace_lock(&workspace, LockPolicy::SkipIfMissing).unwrap(),
            WorkspaceLockStatus::Skipped
        ));

        // Create workspace -> should take lock
        fs::create_dir_all(workspace.workspace_root()).unwrap();
        assert!(matches!(
            acquire_workspace_lock(&workspace, LockPolicy::SkipIfMissing).unwrap(),
            WorkspaceLockStatus::Acquired(_)
        ));
    }

    #[test]
    fn required_detects_existing_lock() {
        let temp = tempdir().unwrap();
        let workspace = Workspace::new(temp.path());

        let guard = match acquire_workspace_lock(&workspace, LockPolicy::Required).unwrap() {
            WorkspaceLockStatus::Acquired(guard) => guard,
            WorkspaceLockStatus::Skipped => panic!("required policy must not skip lock"),
        };

        match acquire_workspace_lock(&workspace, LockPolicy::Required) {
            Err(WorkspaceLockError::AlreadyLocked) => {}
            other => panic!("expected AlreadyLocked, got {:?}", other),
        }

        drop(guard);

        assert!(matches!(
            acquire_workspace_lock(&workspace, LockPolicy::Required).unwrap(),
            WorkspaceLockStatus::Acquired(_)
        ));
    }
}
