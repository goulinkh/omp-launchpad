use serde_json::Value;

use crate::diff::{DiffLineKind, DiffLocation, DiffSide};

pub fn render_bug(
    bug: &Value,
    tasks: &[Value],
    comments: &[Value],
    include_comments: bool,
    comment_limit: usize,
) -> String {
    let id = scalar_field(bug, "id").unwrap_or_else(|| "?".to_owned());
    let title = text_field(bug, "title").unwrap_or("Untitled");
    let mut lines = vec![format!("# Bug #{id}: {title}")];

    push_bullet(
        &mut lines,
        "Status",
        unique_fields(tasks, "status").map(|values| values.join(", ")),
    );
    push_bullet(
        &mut lines,
        "Importance",
        unique_fields(tasks, "importance").map(|values| values.join(", ")),
    );
    push_bullet(&mut lines, "Owner", person_field(bug, "owner"));
    push_bullet(&mut lines, "Created", scalar_field(bug, "date_created"));
    push_bullet(
        &mut lines,
        "Updated",
        scalar_field(bug, "date_last_updated"),
    );
    push_bullet(
        &mut lines,
        "Information type",
        scalar_field(bug, "information_type"),
    );
    push_bullet(&mut lines, "Tags", scalar_field(bug, "tags"));
    push_bullet(&mut lines, "Heat", scalar_field(bug, "heat"));
    push_bullet(&mut lines, "URL", scalar_field(bug, "web_link"));
    push_section(
        &mut lines,
        "Description",
        text_field(bug, "description").unwrap_or_default(),
    );

    let task_lines: Vec<_> = tasks.iter().map(render_bug_task).collect();
    if !task_lines.is_empty() {
        push_section(&mut lines, "Tasks", &task_lines.join("\n"));
    }
    if include_comments && !comments.is_empty() {
        let comments: Vec<_> = comments.iter().map(render_comment).collect();
        push_section(
            &mut lines,
            &format!("Comments (up to {comment_limit})"),
            &comments.join("\n\n"),
        );
    }
    lines.join("\n\n")
}

pub fn render_proposal(
    proposal: &Value,
    comments: &[Value],
    votes: &[Value],
    include_comments: bool,
    comment_limit: usize,
) -> String {
    let id = resource_id(proposal).unwrap_or_else(|| "?".to_owned());
    let title = text_field(proposal, "commit_message")
        .filter(|title| !title.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| {
            text_field(proposal, "description")
                .map(compact)
                .filter(|description| !description.is_empty())
                .map(|description| description.chars().take(120).collect())
        })
        .unwrap_or_else(|| "Untitled".to_owned());
    let mut lines = vec![format!("# Merge proposal {id}: {title}")];

    push_bullet(&mut lines, "Status", scalar_field(proposal, "queue_status"));
    push_bullet(
        &mut lines,
        "Source",
        scalar_field(proposal, "source_git_path")
            .or_else(|| nested_scalar(proposal, "source_branch", "unique_name")),
    );
    push_bullet(
        &mut lines,
        "Target",
        scalar_field(proposal, "target_git_path")
            .or_else(|| nested_scalar(proposal, "target_branch", "unique_name")),
    );
    push_bullet(
        &mut lines,
        "Registrant",
        person_field(proposal, "registrant"),
    );
    push_bullet(&mut lines, "Reviewer", person_field(proposal, "reviewer"));
    push_bullet(
        &mut lines,
        "Created",
        scalar_field(proposal, "date_created"),
    );
    push_bullet(
        &mut lines,
        "Reviewed",
        scalar_field(proposal, "date_reviewed"),
    );
    push_bullet(&mut lines, "Merged", scalar_field(proposal, "date_merged"));
    push_bullet(&mut lines, "URL", scalar_field(proposal, "web_link"));
    push_section(
        &mut lines,
        "Description",
        text_field(proposal, "description").unwrap_or_default(),
    );

    let vote_lines: Vec<_> = votes.iter().map(render_vote).collect();
    if !vote_lines.is_empty() {
        push_section(&mut lines, "Reviews", &vote_lines.join("\n"));
    }
    if include_comments && !comments.is_empty() {
        let comments: Vec<_> = comments.iter().map(render_comment).collect();
        push_section(
            &mut lines,
            &format!("Comments (up to {comment_limit})"),
            &comments.join("\n\n"),
        );
    }
    lines.join("\n\n")
}

pub fn render_repository(repository: &Value) -> String {
    let name = text_field(repository, "unique_name")
        .or_else(|| text_field(repository, "name"))
        .unwrap_or("repository");
    let mut lines = vec![format!("# {name}")];
    push_bullet(&mut lines, "Owner", person_field(repository, "owner"));
    push_bullet(&mut lines, "Target", person_field(repository, "target"));
    push_bullet(
        &mut lines,
        "Default branch",
        scalar_field(repository, "default_branch"),
    );
    push_bullet(&mut lines, "Status", scalar_field(repository, "status"));
    push_bullet(
        &mut lines,
        "Information type",
        scalar_field(repository, "information_type"),
    );
    push_bullet(
        &mut lines,
        "Created",
        scalar_field(repository, "date_created"),
    );
    push_bullet(
        &mut lines,
        "Updated",
        scalar_field(repository, "date_last_modified"),
    );
    push_bullet(
        &mut lines,
        "Clone URL",
        scalar_field(repository, "git_https_url"),
    );
    push_bullet(
        &mut lines,
        "SSH URL",
        scalar_field(repository, "git_ssh_url"),
    );
    push_bullet(&mut lines, "URL", scalar_field(repository, "web_link"));
    push_section(
        &mut lines,
        "Description",
        text_field(repository, "description").unwrap_or_default(),
    );
    lines.join("\n\n")
}

pub fn render_preview_diffs(preview_diffs: &[Value], current_id: u64) -> String {
    let mut preview_diffs: Vec<_> = preview_diffs.iter().collect();
    preview_diffs.sort_by_key(|preview_diff| {
        preview_diff
            .get("id")
            .and_then(Value::as_u64)
            .unwrap_or_default()
    });
    let is_empty = preview_diffs.is_empty();
    let mut lines = vec!["# Preview diff history".to_owned()];
    for preview_diff in preview_diffs {
        let raw_id = preview_diff.get("id").and_then(Value::as_u64);
        let id = raw_id.map_or_else(|| "?".to_owned(), |id| id.to_string());
        let current = (raw_id == Some(current_id)).then_some(" · current");
        let stale = preview_diff
            .get("stale")
            .and_then(Value::as_bool)
            .is_some_and(|stale| stale)
            .then_some(" · stale");
        let created = text_field(preview_diff, "date_created")
            .map(|created| format!(" · {created}"))
            .unwrap_or_default();
        lines.push(format!(
            "- **{id}**{created}{}{}",
            current.unwrap_or_default(),
            stale.unwrap_or_default()
        ));
    }
    if is_empty {
        lines.push("No preview diffs.".to_owned());
    }
    lines.join("\n")
}

pub fn render_inline_comments(comments: &[Value], preview_diff_id: u64) -> String {
    let mut lines = vec![format!(
        "# Published inline comments for preview diff {preview_diff_id}"
    )];
    for comment in comments {
        let line = scalar_field(comment, "line_number").unwrap_or_else(|| "?".to_owned());
        let author = person_field(comment, "person").unwrap_or_else(|| "unknown".to_owned());
        let date = text_field(comment, "date")
            .map(|date| format!(" · {date}"))
            .unwrap_or_default();
        let text = text_field(comment, "text").unwrap_or_default();
        lines.push(format!("## Diff line {line} · {author}{date}\n\n{text}"));
    }
    if comments.is_empty() {
        lines.push("No published inline comments.".to_owned());
    }
    lines.join("\n\n")
}

pub fn render_review_drafts(drafts: &Value, preview_diff_id: u64) -> String {
    let mut lines = vec![format!(
        "# Review drafts for preview diff {preview_diff_id}"
    )];
    let mut drafts: Vec<_> = drafts
        .as_object()
        .into_iter()
        .flat_map(|drafts| drafts.iter())
        .collect();
    drafts.sort_by_key(|(line, _)| line.parse::<usize>().unwrap_or_default());
    for (line, body) in &drafts {
        lines.push(format!(
            "## Diff line {line}\n\n{}",
            body.as_str().unwrap_or_default()
        ));
    }
    if drafts.is_empty() {
        lines.push("No review drafts.".to_owned());
    }
    lines.join("\n\n")
}

pub fn render_diff_location(location: &DiffLocation, preview_diff_id: u64) -> String {
    let side = match location.side {
        DiffSide::Original => "original",
        DiffSide::Modified => "modified",
    };
    let kind = match location.kind {
        DiffLineKind::Context => "context",
        DiffLineKind::Added => "added",
        DiffLineKind::Removed => "removed",
    };
    format!(
        "# Diff line mapping\n\n- **Preview diff:** {preview_diff_id}\n- **Path:** {}\n- **Side:** {side}\n- **File line:** {}\n- **Global diff line:** {}\n- **Kind:** {kind}",
        location.path, location.file_line, location.diff_line
    )
}

pub fn render_generic(resource: &Value) -> String {
    let kind = resource_kind(resource);
    let title = text_field(resource, "title")
        .or_else(|| text_field(resource, "display_name"))
        .or_else(|| text_field(resource, "name"))
        .unwrap_or(&kind);
    let mut lines = vec![format!("# {title}")];
    push_bullet(&mut lines, "Type", Some(kind));

    let skip = [
        "self_link",
        "resource_type_link",
        "http_etag",
        "web_link",
        "description",
        "title",
        "name",
        "display_name",
    ];
    if let Some(fields) = resource.as_object() {
        for (name, value) in fields {
            if skip.contains(&name.as_str()) || name.ends_with("_collection_link") {
                continue;
            }
            let label = name
                .split('_')
                .map(capitalise)
                .collect::<Vec<_>>()
                .join(" ");
            push_bullet(&mut lines, &label, scalar(value));
        }
    }
    push_bullet(&mut lines, "URL", scalar_field(resource, "web_link"));
    let description = text_field(resource, "description")
        .or_else(|| text_field(resource, "summary"))
        .unwrap_or_default();
    push_section(&mut lines, "Description", description);
    lines.join("\n\n")
}

pub fn render_bug_search(target: &str, query: Option<&str>, tasks: &[Value]) -> String {
    let mut lines = vec!["# Launchpad bug search".to_owned()];
    push_bullet(&mut lines, "Target", Some(target.to_owned()));
    push_bullet(&mut lines, "Query", query.map(str::to_owned));
    push_bullet(&mut lines, "Results", Some(tasks.len().to_string()));
    lines.extend(tasks.iter().map(|task| {
        let bug_link = text_field(task, "bug_link").unwrap_or_default();
        let id = bug_link.rsplit('/').next().unwrap_or("?");
        let title = text_field(task, "title").unwrap_or("Untitled");
        let status = text_field(task, "status").unwrap_or("Unknown");
        let importance = text_field(task, "importance").unwrap_or("Unknown");
        format!(
            "- [#{id}: {title}](https://bugs.launchpad.net/bugs/{id}) — {status} · {importance}"
        )
    }));
    lines.join("\n\n")
}

pub fn render_proposal_search(repository: &Value, proposals: &[Value]) -> String {
    let name = text_field(repository, "unique_name").unwrap_or("repository");
    let mut lines = vec!["# Launchpad merge proposal search".to_owned()];
    push_bullet(&mut lines, "Repository", Some(name.to_owned()));
    push_bullet(&mut lines, "Results", Some(proposals.len().to_string()));
    lines.extend(proposals.iter().map(|proposal| {
        let id = resource_id(proposal).unwrap_or_else(|| "?".to_owned());
        let title = text_field(proposal, "commit_message")
            .or_else(|| text_field(proposal, "description"))
            .map(compact)
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| "Untitled".to_owned());
        let title: String = title.chars().take(160).collect();
        let web_link = text_field(proposal, "web_link").unwrap_or_default();
        let status = text_field(proposal, "queue_status").unwrap_or("Unknown");
        format!("- [MP {id}: {title}]({web_link}) — {status}")
    }));
    lines.join("\n\n")
}

pub fn text_field<'value>(value: &'value Value, name: &str) -> Option<&'value str> {
    value.get(name).and_then(Value::as_str)
}

pub fn scalar_field(value: &Value, name: &str) -> Option<String> {
    value.get(name).and_then(scalar)
}

pub fn person_field(value: &Value, name: &str) -> Option<String> {
    value
        .get(name)
        .and_then(person_name)
        .or_else(|| value.get(format!("{name}_link")).and_then(person_name))
}

pub fn resource_id(resource: &Value) -> Option<String> {
    text_field(resource, "self_link")
        .and_then(|link| link.trim_end_matches('/').rsplit('/').next())
        .map(str::to_owned)
}

fn render_bug_task(task: &Value) -> String {
    let target = text_field(task, "bug_target_display_name")
        .or_else(|| text_field(task, "bug_target_name"))
        .unwrap_or("unknown");
    let status = text_field(task, "status").unwrap_or("Unknown");
    let importance = text_field(task, "importance").unwrap_or("Unknown");
    let assignee = person_field(task, "assignee")
        .map(|assignee| format!(" · assignee: {assignee}"))
        .unwrap_or_default();
    format!("- **{target}:** {status} · {importance}{assignee}")
}

fn render_comment(comment: &Value) -> String {
    let author = person_field(comment, "author")
        .or_else(|| person_field(comment, "owner"))
        .unwrap_or_else(|| "unknown".to_owned());
    let created = scalar_field(comment, "date_created");
    let vote = scalar_field(comment, "vote");
    let metadata = [created, vote]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
    let heading = if metadata.is_empty() {
        format!("### {author}")
    } else {
        format!("### {author} ({metadata})")
    };
    let title = text_field(comment, "title").or_else(|| text_field(comment, "subject"));
    let body = text_field(comment, "message_body")
        .or_else(|| text_field(comment, "content"))
        .unwrap_or_default();
    let mut lines = vec![heading];
    if title.is_some_and(|title| compact(title) != compact(body)) {
        lines.push(format!("**{}**", title.unwrap_or_default()));
    }
    if !body.trim().is_empty() {
        lines.push(body.trim().to_owned());
    }
    lines.join("\n\n")
}

fn render_vote(vote: &Value) -> String {
    let reviewer = person_field(vote, "reviewer")
        .or_else(|| person_field(vote, "registrant"))
        .unwrap_or_else(|| "unknown".to_owned());
    let verdict = text_field(vote, "vote")
        .or_else(|| text_field(vote, "review_type"))
        .unwrap_or("Pending");
    format!("- **{reviewer}:** {verdict}")
}

fn unique_fields(values: &[Value], name: &str) -> Option<Vec<String>> {
    let values = values
        .iter()
        .filter_map(|value| scalar_field(value, name))
        .fold(Vec::new(), |mut values, value| {
            if !values.contains(&value) {
                values.push(value);
            }
            values
        });
    (!values.is_empty()).then_some(values)
}

fn nested_scalar(value: &Value, parent: &str, name: &str) -> Option<String> {
    value
        .get(parent)
        .and_then(|value| scalar_field(value, name))
}

fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::Bool(value) => Some(value.to_string()),
        Value::Number(value) => Some(value.to_string()),
        Value::String(value) if value.is_empty() => None,
        Value::String(value) => Some(value.clone()),
        Value::Array(values) => {
            let values: Vec<_> = values.iter().filter_map(scalar).collect();
            (!values.is_empty()).then(|| values.join(", "))
        }
        Value::Object(_) => text_field(value, "web_link")
            .or_else(|| text_field(value, "self_link"))
            .map(str::to_owned),
    }
}

fn person_name(value: &Value) -> Option<String> {
    match value {
        Value::String(link) => link
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .map(|name| name.trim_start_matches('~').to_owned()),
        Value::Object(_) => text_field(value, "display_name")
            .or_else(|| text_field(value, "name"))
            .map(str::to_owned)
            .or_else(|| {
                text_field(value, "web_link")
                    .or_else(|| text_field(value, "self_link"))
                    .and_then(|link| link.trim_end_matches('/').rsplit('/').next())
                    .map(|name| name.trim_start_matches('~').to_owned())
            }),
        _ => None,
    }
}

fn resource_kind(resource: &Value) -> String {
    text_field(resource, "resource_type_link")
        .and_then(|link| link.rsplit('#').next())
        .unwrap_or("resource")
        .to_owned()
}

fn push_bullet(lines: &mut Vec<String>, label: &str, value: Option<String>) {
    if let Some(value) = value.filter(|value| !value.is_empty()) {
        lines.push(format!("- **{label}:** {value}"));
    }
}

fn push_section(lines: &mut Vec<String>, title: &str, body: &str) {
    if !body.trim().is_empty() {
        lines.push(format!("## {title}\n\n{}", body.trim()));
    }
}

fn compact(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn capitalise(word: &str) -> String {
    let mut characters = word.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().chain(characters).collect(),
        None => String::new(),
    }
}
