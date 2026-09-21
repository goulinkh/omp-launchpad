use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde_json::json;
use tokio::fs;
use tokio::process::Command;

use crate::error::Error;
use crate::request::Request;
use crate::response::OperationResult;
use crate::result::Result;

#[derive(Debug)]
pub struct CheckoutSpec {
    pub id: String,
    pub source_ref: String,
    pub source_repository: String,
    pub source_https_url: Option<String>,
    pub source_ssh_url: Option<String>,
    pub target_https_url: Option<String>,
    pub target_ssh_url: Option<String>,
    pub web_link: Option<String>,
}

pub async fn checkout(spec: CheckoutSpec, request: &Request) -> Result<OperationResult> {
    let branch = spec
        .source_ref
        .strip_prefix("refs/heads/")
        .unwrap_or(&spec.source_ref)
        .to_owned();
    let destination = checkout_destination(&spec, request)?;
    if destination.exists() {
        return Err(Error::invalid(format!(
            "checkout destination already exists: {}",
            destination.display()
        )));
    }

    let source_url = spec
        .source_https_url
        .as_deref()
        .or(spec.source_ssh_url.as_deref())
        .ok_or_else(|| Error::invalid("Launchpad did not return a source repository URL"))?;
    let source_url = match clone_repository(source_url, &branch, &destination).await {
        Ok(()) => source_url.to_owned(),
        Err(error) => {
            let Some(source_ssh_url) = spec.source_ssh_url.as_deref() else {
                return Err(error);
            };
            if source_ssh_url == source_url {
                return Err(error);
            }
            if destination.exists() {
                fs::remove_dir_all(&destination)
                    .await
                    .map_err(|source| Error::Io {
                        path: destination.clone(),
                        source,
                    })?;
            }
            clone_repository(source_ssh_url, &branch, &destination).await?;
            source_ssh_url.to_owned()
        }
    };

    let target_url = spec
        .target_https_url
        .as_deref()
        .or(spec.target_ssh_url.as_deref());
    if target_url.is_some_and(|target_url| target_url != source_url) {
        run_git([
            "-C",
            path_text(&destination)?,
            "remote",
            "add",
            "upstream",
            target_url.unwrap_or_default(),
        ])
        .await?;
    }

    let upstream = target_url
        .filter(|target_url| *target_url != source_url)
        .map(str::to_owned);
    let mut text = format!(
        "# Checked out Launchpad merge proposal {}\n\n- **Branch:** {}\n- **Path:** {}\n- **Origin:** {}",
        spec.id,
        branch,
        destination.display(),
        source_url
    );
    if let Some(upstream) = &upstream {
        text.push_str(&format!("\n- **Upstream:** {upstream}"));
    }
    let details = json!({
        "kind": "checkout",
        "proposalId": spec.id,
        "branch": branch,
        "directory": destination,
        "origin": source_url,
        "upstream": upstream,
    });
    Ok(OperationResult::new(text)
        .with_source_url(spec.web_link)
        .with_details(details))
}

pub async fn current_repository_branch() -> Result<(String, String)> {
    let branch = git_stdout(["rev-parse", "--abbrev-ref", "HEAD"]).await?;
    if branch.is_empty() || branch == "HEAD" {
        return Err(Error::invalid(
            "cannot infer a merge proposal from a detached HEAD; provide repository and branch",
        ));
    }
    let repository = git_stdout(["remote", "get-url", "origin"]).await?;
    if repository.is_empty() {
        return Err(Error::invalid(
            "cannot infer a Launchpad repository because origin has no URL",
        ));
    }
    Ok((repository, branch))
}

pub async fn push(request: &Request) -> Result<OperationResult> {
    let directory = request.directory.as_deref().map(PathBuf::from).unwrap_or(
        std::env::current_dir().map_err(|source| Error::Io {
            path: PathBuf::from("."),
            source,
        })?,
    );
    let directory = absolute_path(directory)?;
    let output = run_git([
        "-C",
        path_text(&directory)?,
        "rev-parse",
        "--abbrev-ref",
        "HEAD",
    ])
    .await?;
    let branch = String::from_utf8(output.stdout)
        .map_err(|source| Error::OutputEncoding { source })?
        .trim()
        .to_owned();
    if branch.is_empty() || branch == "HEAD" {
        return Err(Error::invalid("cannot push a detached HEAD"));
    }

    let mut arguments = vec!["-C", path_text(&directory)?, "push"];
    if request.force_with_lease == Some(true) {
        arguments.push("--force-with-lease");
    }
    arguments.extend(["origin", "HEAD"]);
    let output = run_git(arguments).await?;
    let stderr = String::from_utf8(output.stderr)
        .map_err(|source| Error::OutputEncoding { source })?
        .trim()
        .to_owned();
    let mut text = format!(
        "# Pushed Launchpad merge proposal branch\n\n- **Branch:** {branch}\n- **Path:** {}",
        directory.display()
    );
    if !stderr.is_empty() {
        text.push_str(&format!("\n\n{stderr}"));
    }
    let details = json!({
        "kind": "push",
        "branch": branch,
        "directory": directory,
        "forceWithLease": request.force_with_lease == Some(true),
    });
    Ok(OperationResult::new(text).with_details(details))
}

fn checkout_destination(spec: &CheckoutSpec, request: &Request) -> Result<PathBuf> {
    let destination = if let Some(directory) = request.directory.as_deref() {
        PathBuf::from(directory)
    } else {
        let home = dirs::home_dir().ok_or(Error::HomeDirectory)?;
        let repository = Path::new(&spec.source_repository)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("repository");
        home.join(".omp")
            .join("wt")
            .join(format!("lp-{}-{repository}", spec.id))
    };
    absolute_path(destination)
}

fn absolute_path(path: PathBuf) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path);
    }
    let current_dir = std::env::current_dir().map_err(|source| Error::Io {
        path: PathBuf::from("."),
        source,
    })?;
    Ok(current_dir.join(path))
}

async fn clone_repository(source_url: &str, branch: &str, destination: &Path) -> Result<()> {
    run_git([
        "clone",
        "--branch",
        branch,
        "--single-branch",
        source_url,
        path_text(destination)?,
    ])
    .await?;
    Ok(())
}

async fn run_git<'argument>(
    arguments: impl IntoIterator<Item = &'argument str>,
) -> Result<std::process::Output> {
    let mut command = Command::new("git");
    command
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = command.output().await.map_err(|source| Error::Command {
        program: "git".to_owned(),
        source,
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let reason = if !stderr.is_empty() {
            stderr
        } else if !stdout.is_empty() {
            stdout
        } else {
            format!("exited with status {}", output.status)
        };
        return Err(Error::CommandFailed {
            program: "git".to_owned(),
            reason,
        });
    }
    Ok(output)
}

async fn git_stdout<'argument>(
    arguments: impl IntoIterator<Item = &'argument str>,
) -> Result<String> {
    let output = run_git(arguments).await?;
    String::from_utf8(output.stdout)
        .map_err(|source| Error::OutputEncoding { source })
        .map(|stdout| stdout.trim().to_owned())
}

fn path_text(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| Error::invalid(format!("path is not valid UTF-8: {}", path.display())))
}
