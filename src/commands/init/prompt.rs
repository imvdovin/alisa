use std::{
    fs,
    io::{self, Write},
    path::Path,
};

use anyhow::Context;

use super::{InitError, InitOptions, InitReporter, PROMPT_TIMEOUT, PROMPT_TIMEOUT_SECS, platform};

pub(super) fn ensure_text_file<F, V>(
    path: &Path,
    opts: &InitOptions,
    reporter: &mut InitReporter,
    label: &str,
    content_fn: F,
    validator: V,
) -> Result<(), InitError>
where
    F: FnOnce() -> Result<String, InitError>,
    V: Fn(&Path) -> Result<(), String>,
{
    let mut content_fn = Some(content_fn);

    if path.exists() {
        match validator(path) {
            Ok(_) => {
                reporter.exists(label, path);
                return Ok(());
            }
            Err(reason) => {
                let builder = content_fn
                    .take()
                    .expect("content_fn already consumed when overwriting text file");
                return handle_corrupted_artifact(
                    label,
                    path,
                    &reason,
                    opts,
                    reporter,
                    move |reporter| write_text_file(path, label, builder, reporter, opts, true),
                );
            }
        }
    }

    let builder = content_fn
        .take()
        .expect("content_fn already consumed when creating text file");
    write_text_file(path, label, builder, reporter, opts, false)
}

pub(super) fn handle_corrupted_artifact<R>(
    label: &str,
    path: &Path,
    reason: &str,
    opts: &InitOptions,
    reporter: &mut InitReporter,
    repair: R,
) -> Result<(), InitError>
where
    R: FnOnce(&mut InitReporter) -> Result<(), InitError>,
{
    eprintln!(
        "[warn] {label}: {} appears corrupted ({reason}).",
        path.display()
    );

    if opts.dry_run {
        reporter.planned(&format!("Overwrite {label}"), path);
        return Ok(());
    }

    let question = format!("Overwrite {label} at {}? [Y/n]", path.display());

    if prompt_yes_no(&question)? {
        repair(reporter)?;
    } else {
        reporter.skipped(label, path);
    }
    Ok(())
}

fn write_text_file<F>(
    path: &Path,
    label: &str,
    content_fn: F,
    reporter: &mut InitReporter,
    opts: &InitOptions,
    is_update: bool,
) -> Result<(), InitError>
where
    F: FnOnce() -> Result<String, InitError>,
{
    if opts.dry_run {
        let action = if is_update {
            format!("Overwrite {label}")
        } else {
            format!("Create {label}")
        };
        reporter.planned(&action, path);
        return Ok(());
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to prepare directory {}", parent.display()))
            .map_err(InitError::Other)?;
    }

    let content = content_fn()?;
    fs::write(path, content)
        .with_context(|| format!("Failed to write {label} at {}", path.display()))
        .map_err(InitError::Other)?;

    if is_update {
        reporter.updated(label, path);
    } else {
        reporter.created(label, path);
    }
    Ok(())
}

fn prompt_yes_no(question: &str) -> Result<bool, InitError> {
    let mut stdout = io::stdout();

    loop {
        print!("{question} ");
        stdout.flush().map_err(|err| InitError::Other(err.into()))?;

        if !platform::wait_for_stdin(PROMPT_TIMEOUT).map_err(|err| InitError::Other(err.into()))? {
            eprintln!(
                "No input received within {PROMPT_TIMEOUT_SECS} seconds. Leaving artifact unchanged."
            );
            return Ok(false);
        }

        let mut buffer = String::new();
        let bytes = io::stdin()
            .read_line(&mut buffer)
            .map_err(|err| InitError::Other(err.into()))?;

        if bytes == 0 {
            eprintln!("No input received. Leaving artifact unchanged.");
            return Ok(false);
        }

        match buffer.trim().to_lowercase().as_str() {
            "" | "y" | "yes" => return Ok(true),
            "n" | "no" => return Ok(false),
            _ => {
                eprintln!("Please answer Y or n.");
            }
        }
    }
}
