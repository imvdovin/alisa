#![allow(dead_code)]

use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use toml::Value;

use crate::config::RoleKind;

pub const TASK_FILE_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum TaskLoadError {
    #[error("failed to read tasks at {path}: {source}")]
    ReadFailed { path: PathBuf, source: std::io::Error },
    #[error("failed to parse tasks{context}: {source}")]
    ParseFailed { context: String, source: toml::de::Error },
    #[error("unsupported tasks version {found}, expected {expected}")]
    UnsupportedVersion { expected: u32, found: u32 },
    #[error("duplicate task id '{id}'")]
    DuplicateTaskId { id: String },
}

#[derive(Debug, Clone, Deserialize)]
pub struct TaskFile {
    pub version: u32,
    pub project: Option<String>,
    #[serde(default)]
    pub tasks: Vec<Task>,
}

#[derive(Debug, Clone)]
pub struct TaskSet {
    pub version: u32,
    pub project: Option<String>,
    pub tasks: Vec<Task>,
}

impl TaskSet {
    pub fn from_path(path: &Path) -> Result<Self, TaskLoadError> {
        let data = fs::read_to_string(path).map_err(|source| TaskLoadError::ReadFailed {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(&data, format!(" at {}", path.display()))
    }

    pub fn from_str(data: &str) -> Result<Self, TaskLoadError> {
        Self::parse(data, String::from(" from inline string"))
    }

    fn parse(data: &str, context: String) -> Result<Self, TaskLoadError> {
        let file: TaskFile = toml::from_str(data).map_err(|source| TaskLoadError::ParseFailed {
            context,
            source,
        })?;

        if file.version != TASK_FILE_VERSION {
            return Err(TaskLoadError::UnsupportedVersion {
                expected: TASK_FILE_VERSION,
                found: file.version,
            });
        }

        let mut ids = HashSet::new();
        for task in &file.tasks {
            if !ids.insert(task.id.clone()) {
                return Err(TaskLoadError::DuplicateTaskId {
                    id: task.id.clone(),
                });
            }
        }

        Ok(TaskSet {
            version: file.version,
            project: file.project,
            tasks: file.tasks,
        })
    }

    pub fn find(&self, id: &str) -> Option<&Task> {
        self.tasks.iter().find(|task| task.id == id)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub status: TaskStatus,
    pub priority: Option<String>,
    pub lang: Option<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub description: Option<String>,
    #[serde(default)]
    pub acceptance: Vec<String>,
    #[serde(default)]
    pub context: TaskContext,
    pub llm: Option<TaskLlmOverrides>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    Todo,
    Doing,
    Done,
    Blocked,
}

impl Default for TaskStatus {
    fn default() -> Self {
        TaskStatus::Todo
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct TaskContext {
    #[serde(default)]
    pub code: Vec<String>,
    #[serde(default)]
    pub docs: Vec<String>,
    #[serde(default)]
    pub scope: Vec<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct TaskLlmOverrides {
    pub plan: Option<String>,
    pub code: Option<String>,
    pub review: Option<String>,
    pub pipeline: Option<String>,
}

impl TaskLlmOverrides {
    pub fn runner_for(&self, role: RoleKind) -> Option<&str> {
        match role {
            RoleKind::Plan => self.plan.as_deref(),
            RoleKind::Code => self.code.as_deref(),
            RoleKind::Review => self.review.as_deref(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tasks_file() {
        let toml = r#"
version = 1

[[tasks]]
id = "A-101"
title = "Implement feature X"
status = "todo"
lang = "rust"
acceptance = ["pass tests"]

[[tasks]]
id = "A-102"
title = "Review"
"#;

        let set = TaskSet::from_str(toml).expect("tasks parsed");
        assert_eq!(set.tasks.len(), 2);
        assert_eq!(set.tasks[0].status, TaskStatus::Todo);
    }

    #[test]
    fn detects_duplicate_ids() {
        let toml = r#"
version = 1

[[tasks]]
id = "A-101"
title = "One"

[[tasks]]
id = "A-101"
title = "Two"
"#;

        let err = TaskSet::from_str(toml).expect_err("duplicate ids fail");
        match err {
            TaskLoadError::DuplicateTaskId { id } => assert_eq!(id, "A-101"),
            other => panic!("unexpected error: {other:?}"),
        }
    }
}
