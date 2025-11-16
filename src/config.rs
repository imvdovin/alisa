#![allow(dead_code)]

use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
};

use globset::Glob;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read config at {path}: {source}")]
    ReadFailed { path: PathBuf, source: std::io::Error },
    #[error("failed to parse config{context}: {source}")]
    ParseFailed { context: String, source: toml::de::Error },
    #[error("invalid config: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    #[serde(default = "default_config_version")]
    pub version: u32,
    pub project: Option<String>,
    #[serde(default)]
    pub runners: HashMap<String, RunnerDef>,
    pub roles: Roles,
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
    #[serde(default)]
    pub routing: Vec<RoutingRule>,
    pub limits: Limits,
    pub apply: Apply,
    pub paths: Paths,
    pub review: ReviewConfig,
    pub summaries: SummariesConfig,
}

impl Config {
    pub fn from_path(path: &Path) -> Result<Self, ConfigError> {
        let data = fs::read_to_string(path)
            .map_err(|source| ConfigError::ReadFailed {
                path: path.to_path_buf(),
                source,
            })?;
        let config: Config = toml::from_str(&data).map_err(|source| ConfigError::ParseFailed {
            context: format!(" at {}", path.display()),
            source,
        })?;
        config.validate()?;
        Ok(config)
    }

    pub fn from_str(data: &str) -> Result<Self, ConfigError> {
        let config: Config = toml::from_str(data).map_err(|source| ConfigError::ParseFailed {
            context: String::from(" from inline string"),
            source,
        })?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        let mut issues = Vec::new();

        if self.runners.is_empty() {
            issues.push("at least one runner must be defined".to_string());
        }

        for (name, runner) in &self.runners {
            if runner.cmd.trim().is_empty() {
                issues.push(format!("runner '{}' must define non-empty cmd", name));
            }
        }

        for (role, runner) in self.roles.configured_entries() {
            if !self.runners.contains_key(runner) {
                issues.push(format!(
                    "role '{}' references unknown runner '{}'",
                    role.as_str(),
                    runner
                ));
            }
        }

        for (profile_name, profile) in &self.profiles {
            for (role, runner) in profile.roles.configured_entries() {
                if !self.runners.contains_key(runner) {
                    issues.push(format!(
                        "profile '{}' role '{}' references unknown runner '{}'",
                        profile_name,
                        role.as_str(),
                        runner
                    ));
                }
            }
        }

        for (idx, rule) in self.routing.iter().enumerate() {
            if !self.runners.contains_key(&rule.use_runner) {
                issues.push(format!(
                    "routing rule #{} for role '{}' references unknown runner '{}'",
                    idx,
                    rule.role.as_str(),
                    rule.use_runner
                ))
            }
            if let Some(profile) = &rule.profile {
                if !self.profiles.contains_key(profile) {
                    issues.push(format!(
                        "routing rule #{} references unknown profile '{}'",
                        idx, profile
                    ));
                }
            }
            if let Some(pattern) = &rule.when.path {
                if let Err(err) = Glob::new(pattern) {
                    issues.push(format!(
                        "routing rule #{} has invalid path glob '{}': {}",
                        idx, pattern, err
                    ));
                }
            }
            if let Some(pattern) = &rule.when.task_id {
                if let Err(err) = Glob::new(pattern) {
                    issues.push(format!(
                        "routing rule #{} has invalid task_id glob '{}': {}",
                        idx, pattern, err
                    ));
                }
            }
        }

        if let Some(default_pipeline) = &self.review.default_pipeline {
            if !self.review.pipelines.contains_key(default_pipeline) {
                issues.push(format!(
                    "review.default_pipeline '{}' is not defined",
                    default_pipeline
                ));
            }
        }

        for (name, pipeline) in &self.review.pipelines {
            if pipeline.stages.is_empty() {
                issues.push(format!(
                    "review pipeline '{}' must list at least one stage",
                    name
                ));
            }
            for stage_name in &pipeline.stages {
                if !self.review.stages.contains_key(stage_name) {
                    issues.push(format!(
                        "review pipeline '{}' references undefined stage '{}'",
                        name, stage_name
                    ));
                }
            }
        }

        for (stage_name, stage) in &self.review.stages {
            match stage.kind {
                ReviewStageKind::Exec => {
                    if stage.cmd.as_ref().map(|cmd| cmd.is_empty()).unwrap_or(true) {
                        issues.push(format!(
                            "review stage '{}' of type exec must define a non-empty cmd",
                            stage_name
                        ));
                    }
                }
                ReviewStageKind::Llm | ReviewStageKind::Arbiter => {
                    match stage.runner.as_deref() {
                        Some(runner) if self.runners.contains_key(runner) => {}
                        Some(runner) => issues.push(format!(
                            "review stage '{}' references unknown runner '{}'",
                            stage_name, runner
                        )),
                        None => issues.push(format!(
                            "review stage '{}' of type '{}' must specify runner",
                            stage_name,
                            stage.kind.as_str()
                        )),
                    }
                }
            }
        }

        if issues.is_empty() {
            Ok(())
        } else {
            Err(ConfigError::Invalid(issues.join("; ")))
        }
    }

    pub fn profile(&self, name: &str) -> Option<&Profile> {
        self.profiles.get(name)
    }

    pub fn runner(&self, name: &str) -> Option<&RunnerDef> {
        self.runners.get(name)
    }

    pub fn review_stage(&self, name: &str) -> Option<&ReviewStage> {
        self.review.stages.get(name)
    }
}

fn default_config_version() -> u32 {
    1
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: default_config_version(),
            project: None,
            runners: HashMap::new(),
            roles: Roles::default(),
            profiles: BTreeMap::new(),
            routing: Vec::new(),
            limits: Limits::default(),
            apply: Apply::default(),
            paths: Paths::default(),
            review: ReviewConfig::default(),
            summaries: SummariesConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RunnerDef {
    pub cmd: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub model: Option<String>,
    pub prompt_dir: Option<String>,
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

impl Default for RunnerDef {
    fn default() -> Self {
        Self {
            cmd: String::new(),
            args: Vec::new(),
            model: None,
            prompt_dir: None,
            timeout_ms: None,
            env: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Roles {
    pub plan: Option<String>,
    pub code: Option<String>,
    pub review: Option<String>,
}

impl Roles {
    pub fn runner_for(&self, role: RoleKind) -> Option<&str> {
        match role {
            RoleKind::Plan => self.plan.as_deref(),
            RoleKind::Code => self.code.as_deref(),
            RoleKind::Review => self.review.as_deref(),
        }
    }

    pub fn configured_entries(&self) -> impl Iterator<Item = (RoleKind, &str)> {
        [
            (RoleKind::Plan, self.plan.as_deref()),
            (RoleKind::Code, self.code.as_deref()),
            (RoleKind::Review, self.review.as_deref()),
        ]
        .into_iter()
        .filter_map(|(role, value)| value.map(|value| (role, value)))
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Profile {
    #[serde(default)]
    pub roles: Roles,
    #[serde(default)]
    pub limits: Limits,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RoutingRule {
    pub when: RoutingConditions,
    pub role: RoleKind,
    #[serde(rename = "use")]
    pub use_runner: String,
    pub profile: Option<String>,
}

impl Default for RoutingRule {
    fn default() -> Self {
        Self {
            when: RoutingConditions::default(),
            role: RoleKind::Plan,
            use_runner: String::new(),
            profile: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct RoutingConditions {
    pub lang: Option<String>,
    pub path: Option<String>,
    pub task_id: Option<String>,
    pub profile: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RoleKind {
    Plan,
    Code,
    Review,
}

impl RoleKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            RoleKind::Plan => "plan",
            RoleKind::Code => "code",
            RoleKind::Review => "review",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Limits {
    pub max_files: Option<u32>,
    pub max_tokens: Option<u32>,
    pub max_changed_lines: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Apply {
    #[serde(default = "default_true")]
    pub confirm: bool,
}

impl Default for Apply {
    fn default() -> Self {
        Self { confirm: true }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Paths {
    #[serde(default = "default_tasks_file")]
    pub tasks_file: String,
    #[serde(default = "default_tasks_dir")]
    pub tasks_dir: String,
    #[serde(default = "default_state_dir")]
    pub state_dir: String,
    #[serde(default = "default_docs_dir")]
    pub docs_dir: String,
}

impl Default for Paths {
    fn default() -> Self {
        Self {
            tasks_file: default_tasks_file(),
            tasks_dir: default_tasks_dir(),
            state_dir: default_state_dir(),
            docs_dir: default_docs_dir(),
        }
    }
}

fn default_tasks_file() -> String {
    "tasks.toml".into()
}

fn default_tasks_dir() -> String {
    "tasks".into()
}

fn default_state_dir() -> String {
    "state".into()
}

fn default_docs_dir() -> String {
    "docs".into()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ReviewConfig {
    pub default_pipeline: Option<String>,
    #[serde(default)]
    pub pipelines: BTreeMap<String, ReviewPipeline>,
    #[serde(default)]
    pub stages: BTreeMap<String, ReviewStage>,
}

impl Default for ReviewConfig {
    fn default() -> Self {
        Self {
            default_pipeline: None,
            pipelines: BTreeMap::new(),
            stages: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ReviewPipeline {
    pub stages: Vec<String>,
    pub consensus: Option<ReviewConsensus>,
    #[serde(default)]
    pub fail_on: Vec<String>,
    #[serde(default)]
    pub weights: HashMap<String, f32>,
}

impl Default for ReviewPipeline {
    fn default() -> Self {
        Self {
            stages: Vec::new(),
            consensus: None,
            fail_on: Vec::new(),
            weights: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReviewConsensus {
    Gate,
    Majority,
    Weighted,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ReviewStage {
    #[serde(rename = "type")]
    pub kind: ReviewStageKind,
    pub cmd: Option<Vec<String>>,
    pub runner: Option<String>,
    pub prompt: Option<String>,
    pub schema: Option<String>,
    #[serde(default)]
    pub strict: bool,
}

impl Default for ReviewStage {
    fn default() -> Self {
        Self {
            kind: ReviewStageKind::Exec,
            cmd: None,
            runner: None,
            prompt: None,
            schema: None,
            strict: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReviewStageKind {
    Exec,
    Llm,
    Arbiter,
}

impl ReviewStageKind {
    fn as_str(&self) -> &'static str {
        match self {
            ReviewStageKind::Exec => "exec",
            ReviewStageKind::Llm => "llm",
            ReviewStageKind::Arbiter => "arbiter",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SummariesConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub per_stage: bool,
    #[serde(default = "default_true")]
    pub aggregate: bool,
    #[serde(default)]
    pub redact: Vec<String>,
    pub retention_runs: Option<u32>,
}

impl Default for SummariesConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            per_stage: true,
            aggregate: true,
            redact: Vec::new(),
            retention_runs: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_config() {
        let toml = r#"
version = 1
project = "demo"

[runners.claude]
cmd = "claude"

[runners.codex]
cmd = "codex"

[roles]
plan = "claude"
code = "codex"
review = "claude"

[profiles.default.roles]
plan = "claude"
code = "codex"
review = "claude"

[[routing]]
role = "code"
use = "codex"
when.lang = "rust"

[review]
default_pipeline = "strict"

[review.pipelines.strict]
stages = ["build", "llm.criteria"]

[review.stages.build]
type = "exec"
cmd = ["cargo", "check"]

[review.stages."llm.criteria"]
type = "llm"
runner = "claude"
"#;

        let config = Config::from_str(toml).expect("config parses");
        assert_eq!(config.version, 1);
        assert_eq!(config.runners.len(), 2);
        assert_eq!(config.roles.plan.as_deref(), Some("claude"));
        assert_eq!(config.review.pipelines.len(), 1);
        assert!(config.review.stages.contains_key("build"));
    }

    #[test]
    fn validation_detects_missing_runner() {
        let toml = r#"
[runners.codex]
cmd = "codex"

[roles]
plan = "claude"
"#;

        let err = Config::from_str(toml).expect_err("validation should fail");
        match err {
            ConfigError::Invalid(msg) => {
                assert!(msg.contains("claude"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }
}
