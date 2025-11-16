#![allow(dead_code)]

use globset::Glob;
use thiserror::Error;

use crate::{
    config::{Config, ReviewPipeline, ReviewStage, RoleKind, RoutingRule},
    tasks::{Task, TaskLlmOverrides},
};

#[derive(Debug, Clone, Default)]
pub struct CliRoleOverrides {
    pub plan_llm: Option<String>,
    pub code_llm: Option<String>,
    pub review_llm: Option<String>,
    pub llm: Option<String>,
    pub profile: Option<String>,
    pub pipeline: Option<String>,
    pub lang: Option<String>,
}

impl CliRoleOverrides {
    fn role_override(&self, role: RoleKind) -> Option<&str> {
        match role {
            RoleKind::Plan => self.plan_llm.as_deref(),
            RoleKind::Code => self.code_llm.as_deref(),
            RoleKind::Review => self.review_llm.as_deref(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct TaskMeta {
    pub id: Option<String>,
    pub lang: Option<String>,
    pub llm: Option<TaskLlmOverrides>,
    pub paths: Vec<String>,
}

impl TaskMeta {
    pub fn add_path<S: Into<String>>(&mut self, value: S) {
        self.paths.push(value.into());
    }
}

impl From<&Task> for TaskMeta {
    fn from(task: &Task) -> Self {
        let mut paths = Vec::new();
        paths.extend(task.context.scope.iter().cloned());
        paths.extend(task.context.code.iter().cloned());
        paths.extend(task.context.docs.iter().cloned());
        TaskMeta {
            id: Some(task.id.clone()),
            lang: task.lang.clone(),
            llm: task.llm.clone(),
            paths,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedRunners {
    pub profile: Option<String>,
    pub plan: String,
    pub code: String,
    pub review: String,
}

#[derive(Debug, Clone)]
pub struct ResolvedPipeline<'a> {
    pub name: String,
    pub pipeline: &'a ReviewPipeline,
    pub stages: Vec<ResolvedStage<'a>>,
}

#[derive(Debug, Clone)]
pub struct ResolvedStage<'a> {
    pub name: String,
    pub stage: &'a ReviewStage,
}

#[derive(Debug, Error)]
pub enum ResolveError {
    #[error("runner '{name}' is not defined in config")]
    UnknownRunner { name: String },
    #[error("profile '{name}' is not defined in config")]
    UnknownProfile { name: String },
    #[error("unable to resolve runner for role '{role}'")]
    MissingRunner { role: &'static str },
    #[error("pipeline name is required but none was provided")]
    PipelineNotSpecified,
    #[error("review pipeline '{name}' is not defined")]
    PipelineNotFound { name: String },
    #[error("stage '{stage}' referenced by pipeline '{pipeline}' is not defined")]
    StageMissing { pipeline: String, stage: String },
    #[error("invalid glob pattern '{pattern}': {source}")]
    InvalidGlob {
        pattern: String,
        source: globset::Error,
    },
}

pub fn resolve_runners(
    config: &Config,
    task: &TaskMeta,
    cli: &CliRoleOverrides,
) -> Result<ResolvedRunners, ResolveError> {
    let initial_profile = determine_profile(config, cli.profile.as_deref())?;
    let lang = cli
        .lang
        .as_deref()
        .or_else(|| task.lang.as_deref())
        .map(|value| value.to_ascii_lowercase());
    let mut current_profile = initial_profile.clone();

    let plan = resolve_role(
        RoleKind::Plan,
        config,
        cli,
        task,
        lang.as_deref(),
        current_profile.as_deref(),
    )?;
    if let Some(new_profile) = plan.profile_override {
        ensure_profile(config, &new_profile)?;
        current_profile = Some(new_profile);
    }

    let code = resolve_role(
        RoleKind::Code,
        config,
        cli,
        task,
        lang.as_deref(),
        current_profile.as_deref(),
    )?;
    if let Some(new_profile) = code.profile_override {
        ensure_profile(config, &new_profile)?;
        current_profile = Some(new_profile);
    }

    let review = resolve_role(
        RoleKind::Review,
        config,
        cli,
        task,
        lang.as_deref(),
        current_profile.as_deref(),
    )?;
    if let Some(new_profile) = review.profile_override {
        ensure_profile(config, &new_profile)?;
        current_profile = Some(new_profile);
    }

    let final_profile = current_profile.or(initial_profile);

    Ok(ResolvedRunners {
        profile: final_profile,
        plan: plan.runner,
        code: code.runner,
        review: review.runner,
    })
}

pub fn resolve_review_pipeline<'a>(
    config: &'a Config,
    task: &TaskMeta,
    cli: &CliRoleOverrides,
) -> Result<ResolvedPipeline<'a>, ResolveError> {
    let pipeline_name = cli
        .pipeline
        .as_deref()
        .or_else(|| task.llm.as_ref().and_then(|llm| llm.pipeline.as_deref()))
        .or_else(|| config.review.default_pipeline.as_deref())
        .ok_or(ResolveError::PipelineNotSpecified)?;

    let pipeline = config
        .review
        .pipelines
        .get(pipeline_name)
        .ok_or_else(|| ResolveError::PipelineNotFound {
            name: pipeline_name.to_string(),
        })?;

    let mut stages = Vec::new();
    for stage_name in &pipeline.stages {
        let stage = config
            .review
            .stages
            .get(stage_name)
            .ok_or_else(|| ResolveError::StageMissing {
                pipeline: pipeline_name.to_string(),
                stage: stage_name.clone(),
            })?;
        stages.push(ResolvedStage {
            name: stage_name.clone(),
            stage,
        });
    }

    Ok(ResolvedPipeline {
        name: pipeline_name.to_string(),
        pipeline,
        stages,
    })
}

struct RoleResolution {
    runner: String,
    profile_override: Option<String>,
}

fn resolve_role(
    role: RoleKind,
    config: &Config,
    cli: &CliRoleOverrides,
    task: &TaskMeta,
    lang: Option<&str>,
    profile: Option<&str>,
) -> Result<RoleResolution, ResolveError> {
    if let Some(name) = cli.role_override(role) {
        ensure_runner(config, name)?;
        return Ok(RoleResolution {
            runner: name.to_string(),
            profile_override: None,
        });
    }

    if let Some(name) = cli.llm.as_deref() {
        ensure_runner(config, name)?;
        return Ok(RoleResolution {
            runner: name.to_string(),
            profile_override: None,
        });
    }

    if let Some(overrides) = task.llm.as_ref() {
        if let Some(name) = overrides.runner_for(role) {
            ensure_runner(config, name)?;
            return Ok(RoleResolution {
                runner: name.to_string(),
                profile_override: None,
            });
        }
    }

    if let Some(rule) = match_routing_rule(config, role, lang, profile, task)? {
        ensure_runner(config, &rule.use_runner)?;
        return Ok(RoleResolution {
            runner: rule.use_runner.clone(),
            profile_override: rule.profile.clone(),
        });
    }

    if let Some(profile_name) = profile {
        let profile_cfg = config
            .profile(profile_name)
            .ok_or_else(|| ResolveError::UnknownProfile {
                name: profile_name.to_string(),
            })?;
        if let Some(runner) = profile_cfg.roles.runner_for(role) {
            ensure_runner(config, runner)?;
            return Ok(RoleResolution {
                runner: runner.to_string(),
                profile_override: None,
            });
        }
    }

    if let Some(runner) = config.roles.runner_for(role) {
        ensure_runner(config, runner)?;
        return Ok(RoleResolution {
            runner: runner.to_string(),
            profile_override: None,
        });
    }

    Err(ResolveError::MissingRunner {
        role: role.as_str(),
    })
}

fn determine_profile(
    config: &Config,
    cli_profile: Option<&str>,
) -> Result<Option<String>, ResolveError> {
    if let Some(name) = cli_profile {
        ensure_profile(config, name)?;
        return Ok(Some(name.to_string()));
    }

    if config.profiles.contains_key("default") {
        return Ok(Some(String::from("default")));
    }

    if let Some(name) = config.profiles.keys().next() {
        return Ok(Some(name.clone()));
    }

    Ok(None)
}

fn ensure_runner(config: &Config, name: &str) -> Result<(), ResolveError> {
    if config.runner(name).is_some() {
        Ok(())
    } else {
        Err(ResolveError::UnknownRunner {
            name: name.to_string(),
        })
    }
}

fn ensure_profile(config: &Config, name: &str) -> Result<(), ResolveError> {
    if config.profile(name).is_some() {
        Ok(())
    } else {
        Err(ResolveError::UnknownProfile {
            name: name.to_string(),
        })
    }
}

fn match_routing_rule<'a>(
    config: &'a Config,
    role: RoleKind,
    lang: Option<&str>,
    profile: Option<&str>,
    task: &TaskMeta,
) -> Result<Option<&'a RoutingRule>, ResolveError> {
    for rule in &config.routing {
        if rule.role != role {
            continue;
        }

        if let Some(expected_lang) = rule.when.lang.as_deref() {
            match lang {
                Some(current_lang) if current_lang == expected_lang.to_ascii_lowercase() => {}
                Some(_) | None => continue,
            }
        }

        if let Some(expected_profile) = rule.when.profile.as_deref() {
            if profile != Some(expected_profile) {
                continue;
            }
        }

        if let Some(task_id_glob) = rule.when.task_id.as_deref() {
            if let Some(task_id) = task.id.as_deref() {
                if !glob_matches(task_id_glob, task_id)? {
                    continue;
                }
            } else {
                continue;
            }
        }

        if let Some(path_glob) = rule.when.path.as_deref() {
            if task.paths.is_empty() {
                continue;
            }
            let mut matched = false;
            for path in &task.paths {
                if glob_matches(path_glob, path)? {
                    matched = true;
                    break;
                }
            }
            if !matched {
                continue;
            }
        }

        return Ok(Some(rule));
    }

    Ok(None)
}

fn glob_matches(pattern: &str, value: &str) -> Result<bool, ResolveError> {
    let matcher = Glob::new(pattern)
        .map_err(|source| ResolveError::InvalidGlob {
            pattern: pattern.to_string(),
            source,
        })?
        .compile_matcher();
    Ok(matcher.is_match(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    const CONFIG: &str = r#"
[runners.codex]
cmd = "codex"

[runners.claude]
cmd = "claude"

[runners.gpt4]
cmd = "gpt4"

[roles]
plan = "claude"
code = "codex"
review = "claude"

[profiles.default.roles]
plan = "claude"
code = "codex"
review = "claude"

[profiles.big_repo.roles]
plan = "gpt4"
code = "gpt4"
review = "claude"

[[routing]]
role = "code"
use = "gpt4"
profile = "big_repo"
when.lang = "rust"

[review]
default_pipeline = "strict"

[review.pipelines.strict]
stages = ["build", "llm"]

[review.stages.build]
type = "exec"
cmd = ["cargo", "check"]

[review.stages.llm]
type = "llm"
runner = "claude"
"#;

    fn base_task() -> TaskMeta {
        let mut task = TaskMeta::default();
        task.id = Some("A-1".into());
        task.lang = Some("rust".into());
        task.paths.push("src/auth/lib.rs".into());
        task
    }

    #[test]
    fn cli_overrides_take_priority() {
        let config = Config::from_str(CONFIG).expect("valid config");
        let mut cli = CliRoleOverrides::default();
        cli.plan_llm = Some("gpt4".into());
        cli.llm = Some("codex".into());
        let task = base_task();

        let resolved = resolve_runners(&config, &task, &cli).expect("resolved");
        assert_eq!(resolved.plan, "gpt4");
        assert_eq!(resolved.code, "codex");
        assert_eq!(resolved.review, "codex");
    }

    #[test]
    fn routing_switches_profile() {
        let config = Config::from_str(CONFIG).expect("valid config");
        let task = base_task();
        let cli = CliRoleOverrides::default();

        let resolved = resolve_runners(&config, &task, &cli).expect("resolved");
        assert_eq!(resolved.plan, "claude");
        assert_eq!(resolved.code, "gpt4");
        assert_eq!(resolved.profile.as_deref(), Some("big_repo"));
        // After profile switch review should follow profile roles (gpt4 for plan, claude for review).
        assert_eq!(resolved.review, "claude");
    }

    #[test]
    fn pipeline_resolution_prefers_cli_then_task_then_default() {
        let mut config = Config::from_str(CONFIG).expect("valid config");
        config.review.pipelines.insert(
            "security".into(),
            ReviewPipeline {
                stages: vec!["llm".into()],
                consensus: None,
                fail_on: Vec::new(),
                weights: std::collections::HashMap::new(),
            },
        );

        let mut task = base_task();
        task.llm = Some(TaskLlmOverrides {
            plan: None,
            code: None,
            review: None,
            pipeline: Some("security".into()),
        });

        let mut cli = CliRoleOverrides::default();
        cli.pipeline = Some("strict".into());

        let resolved = resolve_review_pipeline(&config, &task, &cli).expect("pipeline");
        assert_eq!(resolved.name, "strict");

        cli.pipeline = None;
        let resolved = resolve_review_pipeline(&config, &task, &cli).expect("pipeline");
        assert_eq!(resolved.name, "security");
    }
}
