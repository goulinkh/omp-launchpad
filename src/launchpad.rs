use std::collections::HashSet;
use std::env;

use lpcli::auth;
use lpcli::client::{Collection, LaunchpadClient, urlenc};
use lpcli::error::LpError;
use lpcli::git::{GitRef, list_git_refs};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde_json::{Value, json};
use url::Url;

use crate::diff;
use crate::error::Error;
use crate::local_git::{self, CheckoutSpec};
use crate::render;
use crate::request::{OneOrMany, Operation, Request, ResourceKind, ResourceTarget};
use crate::response::OperationResult;
use crate::result::Result;

const MAX_FILE_BYTES: usize = 2 * 1024 * 1024;
const MAX_ITEMS: usize = 50;

pub async fn execute(request: &Request) -> Result<OperationResult> {
    request.validate()?;
    if request.op == Operation::FileRead {
        return read_repository_file(request).await;
    }
    if request.op == Operation::MergeProposalPush {
        return local_git::push(request).await;
    }

    let client = launchpad_client(request.op.requires_authentication())?;
    match request.op {
        Operation::ResourceView => view_resource(&client, request).await,
        Operation::RepoView => view_repository(&client, request).await,
        Operation::SearchBugs => search_bugs(&client, request).await,
        Operation::SearchMergeProposals => search_merge_proposals(&client, request).await,
        Operation::PreviewDiffs => view_preview_diffs(&client, request).await,
        Operation::InlineComments => view_inline_comments(&client, request).await,
        Operation::ReviewDrafts => view_review_drafts(&client, request).await,
        Operation::DiffLineMap => map_diff_line(&client, request).await,
        Operation::BugCreate => create_bug(&client, request).await,
        Operation::MergeProposalCreate => create_merge_proposal(&client, request).await,
        Operation::Comment => add_comment(&client, request).await,
        Operation::ReviewDraftUpdate => update_review_draft(&client, request).await,
        Operation::ReviewSubmit => submit_review(&client, request).await,
        Operation::SetMergeProposalStatus => set_merge_proposal_status(&client, request).await,
        Operation::MergeProposalCheckout => checkout_merge_proposal(&client, request).await,
        Operation::FileRead | Operation::MergeProposalPush => {
            Err(Error::invalid("operation dispatch is inconsistent"))
        }
    }
}

fn launchpad_client(require_authentication: bool) -> Result<LaunchpadClient> {
    let force_anonymous = env::var("OMP_LAUNCHPAD_ANONYMOUS").is_ok_and(|value| value == "1");
    if require_authentication && force_anonymous {
        return Err(Error::invalid(
            "authenticated operations are unavailable in anonymous mode",
        ));
    }
    let credentials = if force_anonymous {
        None
    } else {
        match auth::load_credentials() {
            Ok(credentials) => Some(credentials),
            Err(LpError::NotAuthenticated) if !require_authentication => None,
            Err(source) => return Err(Error::Launchpad { source }),
        }
    };
    let client = LaunchpadClient::new(credentials);
    let base_url = api_base_url()?;
    Ok(client.with_base_url(base_url))
}

fn api_base_url() -> Result<String> {
    if let Ok(base_url) = env::var("OMP_LAUNCHPAD_API_BASE") {
        return Ok(base_url.trim_end_matches('/').to_owned());
    }
    let instance = env::var("OMP_LAUNCHPAD_INSTANCE").unwrap_or_else(|_| "production".to_owned());
    let version = env::var("OMP_LAUNCHPAD_API_VERSION").unwrap_or_else(|_| "devel".to_owned());
    let host = match instance.as_str() {
        "production" => "api.launchpad.net".to_owned(),
        "staging" => "api.staging.launchpad.net".to_owned(),
        "qastaging" => "api.qastaging.launchpad.net".to_owned(),
        instance if instance.contains('.') => instance.to_owned(),
        _ => {
            return Err(Error::invalid(format!(
                "unknown Launchpad instance: {instance}"
            )));
        }
    };
    Ok(format!("https://{host}/{version}"))
}

async fn view_resource(client: &LaunchpadClient, request: &Request) -> Result<OperationResult> {
    let mut target = ResourceTarget::parse(request.target()?)?;
    if let Some(preview_diff_id) = request.preview_diff_id {
        if preview_diff_id == 0 {
            return Err(Error::invalid("preview_diff_id must be greater than zero"));
        }
        if !target.diff {
            return Err(Error::invalid(
                "preview_diff_id is only valid for a merge proposal diff",
            ));
        }
        if target
            .preview_diff_id
            .is_some_and(|id| id != preview_diff_id)
        {
            return Err(Error::invalid(
                "target and preview_diff_id select different snapshots",
            ));
        }
        target.preview_diff_id = Some(preview_diff_id);
    }
    match &target.kind {
        ResourceKind::Bug { id } => view_bug(client, &target, *id).await,
        ResourceKind::MergeProposal { repository, id } => {
            view_merge_proposal(client, &target, repository, *id).await
        }
        ResourceKind::Repository => {
            let repository = get_repository(client, &target.path).await?;
            let source_url = render::text_field(&repository, "web_link").map(str::to_owned);
            let text = render::render_repository(&repository);
            let details = json!({ "kind": "git_repository", "target": target.path });
            Ok(OperationResult::new(text)
                .with_source_url(source_url)
                .with_details(details))
        }
        ResourceKind::Generic => {
            if target.diff {
                return Err(Error::invalid("/diff is only valid for a merge proposal"));
            }
            let resource: Value = client.get(&format!("/{}", target.path)).await?;
            let source_url = render::text_field(&resource, "web_link").map(str::to_owned);
            let kind = resource_kind(&resource);
            let text = render::render_generic(&resource);
            let details = json!({ "kind": kind, "target": target.path });
            Ok(OperationResult::new(text)
                .with_source_url(source_url)
                .with_details(details))
        }
    }
}

async fn view_bug(
    client: &LaunchpadClient,
    target: &ResourceTarget,
    id: u64,
) -> Result<OperationResult> {
    if target.diff {
        return Err(Error::invalid("/diff is only valid for a merge proposal"));
    }
    let bug: Value = client.get(&format!("/bugs/{id}")).await?;
    let tasks = fetch_entries(
        client,
        &client.url(&format!("/bugs/{id}/bug_tasks")),
        MAX_ITEMS,
    )
    .await?;
    let comments = if target.include_comments {
        fetch_entries(
            client,
            &client.url(&format!("/bugs/{id}/messages")),
            target.comment_limit,
        )
        .await?
    } else {
        Vec::new()
    };
    let text = render::render_bug(
        &bug,
        &tasks,
        &comments,
        target.include_comments,
        target.comment_limit,
    );
    let source_url = render::text_field(&bug, "web_link").map(str::to_owned);
    let details = json!({ "kind": "bug", "target": target.path, "id": id });
    Ok(OperationResult::new(text)
        .with_source_url(source_url)
        .with_details(details))
}

async fn view_merge_proposal(
    client: &LaunchpadClient,
    target: &ResourceTarget,
    repository: &str,
    id: u64,
) -> Result<OperationResult> {
    let proposal = get_merge_proposal(client, repository, id).await?;
    let source_url = render::text_field(&proposal, "web_link").map(str::to_owned);
    if target.diff {
        let preview_diff = get_preview_diff(client, &proposal, target.preview_diff_id).await?;
        let preview_diff_id = preview_diff_id(&preview_diff)?;
        let mut text = get_diff_text(&preview_diff).await?;
        let truncated = text.len() > MAX_FILE_BYTES;
        if truncated {
            text.truncate(floor_char_boundary(&text, MAX_FILE_BYTES));
            text.push_str("\n\n[Diff truncated at 2 MiB]");
        }
        let details = json!({
            "kind": "branch_merge_proposal",
            "diff": true,
            "preview_diff_id": preview_diff_id,
            "stale": preview_diff.get("stale").and_then(Value::as_bool).unwrap_or(false),
            "truncated": truncated,
        });
        return Ok(OperationResult::new(text)
            .with_source_url(source_url)
            .with_details(details));
    }
    let comments = if target.include_comments {
        fetch_entries(
            client,
            &client.url(&format!("/{repository}/+merge/{id}/all_comments")),
            target.comment_limit,
        )
        .await?
    } else {
        Vec::new()
    };
    let votes_url = render::text_field(&proposal, "votes_collection_link")
        .map(str::to_owned)
        .unwrap_or_else(|| client.url(&format!("/{repository}/+merge/{id}/votes")));
    let votes = fetch_entries(client, &votes_url, MAX_ITEMS)
        .await
        .unwrap_or_default();
    let text = render::render_proposal(
        &proposal,
        &comments,
        &votes,
        target.include_comments,
        target.comment_limit,
    );
    let details = proposal_details(client, &proposal).await?;
    Ok(OperationResult::new(text)
        .with_source_url(source_url)
        .with_details(details))
}

async fn view_preview_diffs(
    client: &LaunchpadClient,
    request: &Request,
) -> Result<OperationResult> {
    let (_, proposal) = request_merge_proposal(client, request).await?;
    let preview_diffs = get_preview_diffs(client, &proposal).await?;
    let current = get_preview_diff(client, &proposal, None).await?;
    let current_id = preview_diff_id(&current)?;
    let source_url = render::text_field(&proposal, "web_link").map(str::to_owned);
    let text = render::render_preview_diffs(&preview_diffs, current_id);
    let details = json!({
        "kind": "preview_diff_history",
        "current_preview_diff_id": current_id,
        "preview_diffs": preview_diffs,
    });
    Ok(OperationResult::new(text)
        .with_source_url(source_url)
        .with_details(details))
}

async fn view_inline_comments(
    client: &LaunchpadClient,
    request: &Request,
) -> Result<OperationResult> {
    let preview_diff_id = request.preview_diff_id()?;
    let (_, proposal) = request_merge_proposal(client, request).await?;
    get_preview_diff(client, &proposal, Some(preview_diff_id)).await?;
    let comments = inline_comments(client, &proposal, preview_diff_id).await?;
    let source_url = render::text_field(&proposal, "web_link").map(str::to_owned);
    let text = render::render_inline_comments(&comments, preview_diff_id);
    let details = json!({
        "kind": "inline_comments",
        "preview_diff_id": preview_diff_id,
        "comments": comments,
    });
    Ok(OperationResult::new(text)
        .with_source_url(source_url)
        .with_details(details))
}

async fn view_review_drafts(
    client: &LaunchpadClient,
    request: &Request,
) -> Result<OperationResult> {
    let preview_diff_id = request.preview_diff_id()?;
    let (_, proposal) = request_merge_proposal(client, request).await?;
    get_preview_diff(client, &proposal, Some(preview_diff_id)).await?;
    let drafts = review_drafts(client, &proposal, preview_diff_id).await?;
    let source_url = render::text_field(&proposal, "web_link").map(str::to_owned);
    let text = render::render_review_drafts(&drafts, preview_diff_id);
    let details = json!({
        "kind": "review_drafts",
        "preview_diff_id": preview_diff_id,
        "drafts": drafts,
    });
    Ok(OperationResult::new(text)
        .with_source_url(source_url)
        .with_details(details))
}

async fn map_diff_line(client: &LaunchpadClient, request: &Request) -> Result<OperationResult> {
    let preview_diff_id = request.preview_diff_id()?;
    let file_line = request.file_line()?;
    let side = request.side()?;
    let (_, proposal) = request_merge_proposal(client, request).await?;
    let preview_diff = get_preview_diff(client, &proposal, Some(preview_diff_id)).await?;
    let diff_text = get_diff_text(&preview_diff).await?;
    let location = diff::map_file_line(&diff_text, request.path("path")?, side, file_line)
        .ok_or_else(|| Error::invalid("file line is not present in the selected preview diff"))?;
    let source_url = render::text_field(&proposal, "web_link").map(str::to_owned);
    let text = render::render_diff_location(&location, preview_diff_id);
    let details = json!({
        "kind": "diff_line_mapping",
        "preview_diff_id": preview_diff_id,
        "location": location,
    });
    Ok(OperationResult::new(text)
        .with_source_url(source_url)
        .with_details(details))
}

async fn view_repository(client: &LaunchpadClient, request: &Request) -> Result<OperationResult> {
    let repository = get_repository(client, request.repository()?).await?;
    let source_url = render::text_field(&repository, "web_link").map(str::to_owned);
    let text = render::render_repository(&repository);
    let details = json!({ "kind": "git_repository" });
    Ok(OperationResult::new(text)
        .with_source_url(source_url)
        .with_details(details))
}

async fn search_bugs(client: &LaunchpadClient, request: &Request) -> Result<OperationResult> {
    let target = request.target()?;
    let mut url = Url::parse(&client.url(&format!("/{target}"))).map_err(|source| Error::Url {
        url: client.url(&format!("/{target}")),
        source,
    })?;
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("ws.op", "searchTasks");
        query.append_pair("omit_duplicates", "true");
        query.append_pair("ws.size", &request.limit().to_string());
        append_values(&mut query, "status", request.status.as_ref());
        append_values(&mut query, "importance", request.importance.as_ref());
        if let Some(search_text) = request.query.as_deref() {
            query.append_pair("search_text", search_text);
        }
        if let Some(tags) = &request.tags {
            for tag in tags {
                query.append_pair("tags", tag);
            }
        }
    }
    let tasks = fetch_entries(client, url.as_str(), request.limit()).await?;
    let text = render::render_bug_search(target, request.query.as_deref(), &tasks);
    let details = json!({ "count": tasks.len(), "target": target });
    Ok(OperationResult::new(text).with_details(details))
}

async fn search_merge_proposals(
    client: &LaunchpadClient,
    request: &Request,
) -> Result<OperationResult> {
    let repository_path = request.repository()?;
    let repository = get_repository(client, repository_path).await?;
    let mut url =
        Url::parse(&client.url(&format!("/{repository_path}"))).map_err(|source| Error::Url {
            url: client.url(&format!("/{repository_path}")),
            source,
        })?;
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("ws.op", "getMergeProposals");
        query.append_pair("ws.size", &request.limit().to_string());
        append_values(&mut query, "status", request.status.as_ref());
    }
    let proposals = fetch_entries(client, url.as_str(), request.limit()).await?;
    let text = render::render_proposal_search(&repository, &proposals);
    let name = render::text_field(&repository, "unique_name");
    let details = json!({ "count": proposals.len(), "repository": name });
    Ok(OperationResult::new(text).with_details(details))
}

async fn create_bug(client: &LaunchpadClient, request: &Request) -> Result<OperationResult> {
    let target = request.target()?;
    let title = request.string(&request.title, "title")?;
    let description = request.string(&request.description, "description")?;
    let target_url = client.url(&format!("/{target}"));
    let mut parameters = vec![
        ("ws.op", "createBug"),
        ("title", title),
        ("description", description),
        ("target", target_url.as_str()),
    ];
    if let Some(information_type) = request.information_type.as_deref() {
        parameters.push(("information_type", information_type));
    }
    if let Some(tags) = &request.tags {
        parameters.extend(tags.iter().map(|tag| ("tags", tag.as_str())));
    }
    let location = client
        .post_pairs_created_location("/bugs", &parameters)
        .await?;
    let bug: Value = client.get_url(&location).await?;
    let id = bug
        .get("id")
        .and_then(Value::as_u64)
        .ok_or_else(|| Error::invalid("Launchpad returned a bug without an ID"))?;
    let tasks = fetch_entries(
        client,
        &client.url(&format!("/bugs/{id}/bug_tasks")),
        MAX_ITEMS,
    )
    .await?;
    let text = format!(
        "# Created Launchpad bug\n\n{}",
        render::render_bug(&bug, &tasks, &[], false, 1)
    );
    let source_url = render::text_field(&bug, "web_link").map(str::to_owned);
    let details = json!({ "kind": "bug", "id": id });
    Ok(OperationResult::new(text)
        .with_source_url(source_url)
        .with_details(details))
}

async fn create_merge_proposal(
    client: &LaunchpadClient,
    request: &Request,
) -> Result<OperationResult> {
    let source_repository = request.repository()?;
    let target_repository = request
        .target_repository
        .as_deref()
        .unwrap_or(source_repository);
    let source_ref = request.string(&request.source_ref, "source_ref")?;
    let target_ref = request.string(&request.target_ref, "target_ref")?;
    let source = find_git_ref(client, source_repository, source_ref).await?;
    let target = find_git_ref(client, target_repository, target_ref).await?;
    let source_link = source
        .self_link
        .as_deref()
        .ok_or_else(|| Error::invalid("source ref has no API link"))?;
    let target_link = target
        .self_link
        .as_deref()
        .ok_or_else(|| Error::invalid("target ref has no API link"))?;
    let needs_review = request.needs_review.unwrap_or(true).to_string();
    let mut parameters = vec![
        ("ws.op", "createMergeProposal"),
        ("merge_target", target_link),
        (
            "initial_comment",
            request.description.as_deref().unwrap_or_default(),
        ),
        ("needs_review", needs_review.as_str()),
    ];
    if let Some(commit_message) = request.commit_message.as_deref() {
        parameters.push(("commit_message", commit_message));
    }
    let location = client
        .post_pairs_url_created_location(source_link, &parameters)
        .await?;
    let proposal: Value = client.get_url(&location).await?;
    let text = format!(
        "# Created Launchpad merge proposal\n\n{}",
        render::render_proposal(&proposal, &[], &[], false, 1)
    );
    let source_url = render::text_field(&proposal, "web_link").map(str::to_owned);
    let details = proposal_details(client, &proposal).await?;
    Ok(OperationResult::new(text)
        .with_source_url(source_url)
        .with_details(details))
}

async fn add_comment(client: &LaunchpadClient, request: &Request) -> Result<OperationResult> {
    let target = ResourceTarget::parse(request.target()?)?;
    let body = request.string(&request.body, "body")?;
    let kind = match target.kind {
        ResourceKind::Bug { id } => {
            let url = client.url(&format!("/bugs/{id}"));
            let mut parameters = vec![("ws.op", "newMessage"), ("content", body)];
            if let Some(subject) = request.subject.as_deref() {
                parameters.push(("subject", subject));
            }
            client.post_pairs_url_ok(&url, &parameters).await?;
            "bug"
        }
        ResourceKind::MergeProposal { repository, id } => {
            let url = client.url(&format!("/{repository}/+merge/{id}"));
            let mut parameters = vec![("ws.op", "createComment"), ("content", body)];
            if let Some(subject) = request.subject.as_deref() {
                parameters.push(("subject", subject));
            }
            if let Some(vote) = request.vote.as_deref() {
                parameters.push(("vote", vote));
            }
            client.post_pairs_url_ok(&url, &parameters).await?;
            "branch_merge_proposal"
        }
        ResourceKind::Repository | ResourceKind::Generic => {
            return Err(Error::invalid(
                "comments are supported only for bugs and merge proposals",
            ));
        }
    };
    let source_url = launchpad_web_url(&target.path, kind);
    let text = format!("Comment added to {source_url}");
    let details = json!({ "kind": kind });
    Ok(OperationResult::new(text)
        .with_source_url(Some(source_url))
        .with_details(details))
}

async fn update_review_draft(
    client: &LaunchpadClient,
    request: &Request,
) -> Result<OperationResult> {
    let preview_diff_id = request.preview_diff_id()?;
    let file_line = request.file_line()?;
    let side = request.side()?;
    let (target, proposal) = request_merge_proposal(client, request).await?;
    let preview_diff = enforce_current_preview_diff(client, &proposal, preview_diff_id).await?;
    let diff_text = get_diff_text(&preview_diff).await?;
    let location = diff::map_file_line(&diff_text, request.path("path")?, side, file_line)
        .ok_or_else(|| Error::invalid("file line is not present in the selected preview diff"))?;
    if !location.is_commentable() {
        return Err(Error::invalid(
            "original-side comments require a removed line",
        ));
    }

    let mut drafts = review_drafts(client, &proposal, preview_diff_id).await?;
    let draft_map = drafts
        .as_object_mut()
        .ok_or_else(|| Error::invalid("Launchpad returned invalid review drafts"))?;
    let diff_line = location.diff_line.to_string();
    let action = if let Some(body) = request.body.as_deref() {
        let body = body.trim();
        if body.is_empty() {
            return Err(Error::invalid("draft body cannot be empty"));
        }
        draft_map.insert(diff_line, Value::String(body.to_owned()));
        "saved"
    } else {
        draft_map.remove(&diff_line);
        "deleted"
    };
    let (_, current_proposal) = request_merge_proposal(client, request).await?;
    enforce_current_preview_diff(client, &current_proposal, preview_diff_id).await?;
    save_review_drafts(client, &current_proposal, preview_diff_id, &drafts).await?;

    let source_url = launchpad_web_url(&target.path, "branch_merge_proposal");
    let side = match location.side {
        crate::diff::DiffSide::Original => "original",
        crate::diff::DiffSide::Modified => "modified",
    };
    let text = format!(
        "Review draft {action} on {}:{} ({side}, global diff line {})",
        location.path, location.file_line, location.diff_line
    );
    let details = json!({
        "kind": "review_draft_update",
        "preview_diff_id": preview_diff_id,
        "action": action,
        "location": location,
        "drafts": drafts,
    });
    Ok(OperationResult::new(text)
        .with_source_url(Some(source_url))
        .with_details(details))
}

async fn submit_review(client: &LaunchpadClient, request: &Request) -> Result<OperationResult> {
    let preview_diff_id = request.preview_diff_id()?;
    let (target, proposal) = request_merge_proposal(client, request).await?;
    enforce_current_preview_diff(client, &proposal, preview_diff_id).await?;
    let drafts = review_drafts(client, &proposal, preview_diff_id).await?;
    let draft_count = drafts.as_object().map_or(0, serde_json::Map::len);
    let content = request.body.as_deref().unwrap_or_default();
    if content.trim().is_empty() && request.vote.is_none() && draft_count == 0 {
        return Err(Error::invalid(
            "review must include content, a vote, or inline drafts",
        ));
    }
    let (_, current_proposal) = request_merge_proposal(client, request).await?;
    enforce_current_preview_diff(client, &current_proposal, preview_diff_id).await?;
    create_inline_review(
        client,
        &current_proposal,
        preview_diff_id,
        content,
        &drafts,
        request.vote.as_deref(),
    )
    .await?;

    let source_url = launchpad_web_url(&target.path, "branch_merge_proposal");
    let text = format!(
        "Review submitted with {draft_count} inline comment{}: {source_url}",
        if draft_count == 1 { "" } else { "s" }
    );
    let details = json!({
        "kind": "inline_review",
        "preview_diff_id": preview_diff_id,
        "inline_comment_count": draft_count,
        "vote": request.vote,
    });
    Ok(OperationResult::new(text)
        .with_source_url(Some(source_url))
        .with_details(details))
}

async fn set_merge_proposal_status(
    client: &LaunchpadClient,
    request: &Request,
) -> Result<OperationResult> {
    let target = ResourceTarget::parse(request.target()?)?;
    if !matches!(target.kind, ResourceKind::MergeProposal { .. }) {
        return Err(Error::invalid(
            "set_merge_proposal_status requires a merge proposal target",
        ));
    }
    let status = request.status()?;
    let url = client.url(&format!("/{}", target.path));
    client
        .post_pairs_url_ok(&url, &[("ws.op", "setStatus"), ("status", status)])
        .await?;
    let source_url = launchpad_web_url(&target.path, "branch_merge_proposal");
    let text = format!("Merge proposal status set to {status}: {source_url}");
    let details = json!({ "status": status });
    Ok(OperationResult::new(text)
        .with_source_url(Some(source_url))
        .with_details(details))
}

async fn checkout_merge_proposal(
    client: &LaunchpadClient,
    request: &Request,
) -> Result<OperationResult> {
    let target = ResourceTarget::parse(request.target()?)?;
    let ResourceKind::MergeProposal { repository, id } = target.kind else {
        return Err(Error::invalid("target is not a Launchpad merge proposal"));
    };
    let proposal = get_merge_proposal(client, &repository, id).await?;
    let spec = checkout_spec(client, &proposal).await?;
    local_git::checkout(spec, request).await
}

async fn read_repository_file(request: &Request) -> Result<OperationResult> {
    let repository = request.repository()?.trim_matches('/');
    let path = request.path("path")?.trim();
    let repository = encode_path(repository);
    let path = encode_path(path);
    let mut url = Url::parse(&format!(
        "https://git.launchpad.net/{repository}/plain/{path}"
    ))
    .map_err(|source| Error::Url {
        url: format!("https://git.launchpad.net/{repository}/plain/{path}"),
        source,
    })?;
    if let Some(branch) = request.branch.as_deref() {
        url.query_pairs_mut().append_pair("h", branch);
    }
    let response = reqwest::Client::new()
        .get(url.clone())
        .send()
        .await
        .map_err(|source| Error::Web {
            url: url.to_string(),
            source,
        })?;
    let status = response.status();
    if !status.is_success() {
        return Err(Error::HttpStatus {
            url: url.to_string(),
            status,
        });
    }
    let bytes = response.bytes().await.map_err(|source| Error::Web {
        url: url.to_string(),
        source,
    })?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(Error::invalid(format!(
            "Launchpad file is larger than {MAX_FILE_BYTES} bytes"
        )));
    }
    let byte_count = bytes.len();
    let text =
        String::from_utf8(bytes.to_vec()).map_err(|source| Error::OutputEncoding { source })?;
    let details = json!({
        "kind": "file",
        "repository": request.repository()?,
        "path": request.path("path")?,
        "branch": request.branch,
        "bytes": byte_count,
    });
    Ok(OperationResult::new(text)
        .with_source_url(Some(url.to_string()))
        .with_details(details))
}

async fn request_merge_proposal(
    client: &LaunchpadClient,
    request: &Request,
) -> Result<(ResourceTarget, Value)> {
    let target = ResourceTarget::parse(request.target()?)?;
    let (repository, id) = match &target.kind {
        ResourceKind::MergeProposal { repository, id } => (repository.clone(), *id),
        ResourceKind::Bug { .. } | ResourceKind::Repository | ResourceKind::Generic => {
            return Err(Error::invalid("operation requires a merge proposal target"));
        }
    };
    let proposal = get_merge_proposal(client, &repository, id).await?;
    Ok((target, proposal))
}

async fn get_preview_diffs(client: &LaunchpadClient, proposal: &Value) -> Result<Vec<Value>> {
    let collection_link = render::text_field(proposal, "preview_diffs_collection_link")
        .ok_or_else(|| Error::invalid("merge proposal has no preview diff history"))?;
    fetch_all_entries(client, collection_link).await
}

async fn get_preview_diff(
    client: &LaunchpadClient,
    proposal: &Value,
    requested_id: Option<u64>,
) -> Result<Value> {
    if let Some(requested_id) = requested_id {
        return get_preview_diffs(client, proposal)
            .await?
            .into_iter()
            .find(|preview_diff| {
                preview_diff.get("id").and_then(Value::as_u64) == Some(requested_id)
            })
            .ok_or_else(|| {
                Error::invalid(format!(
                    "preview diff {requested_id} does not belong to this merge proposal"
                ))
            });
    }

    let preview_diff_link = render::text_field(proposal, "preview_diff_link")
        .ok_or_else(|| Error::invalid("merge proposal has no current preview diff"))?;
    client.get_url(preview_diff_link).await.map_err(Into::into)
}

fn preview_diff_id(preview_diff: &Value) -> Result<u64> {
    preview_diff
        .get("id")
        .and_then(Value::as_u64)
        .filter(|id| *id > 0)
        .ok_or_else(|| Error::invalid("Launchpad returned a preview diff without an ID"))
}

async fn get_diff_text(preview_diff: &Value) -> Result<String> {
    let diff_text_link = render::text_field(preview_diff, "diff_text_link")
        .ok_or_else(|| Error::invalid("preview diff has no diff text"))?;
    let url = diff_download_url(diff_text_link)?;
    let response = reqwest::get(url.clone())
        .await
        .map_err(|source| Error::Web {
            url: url.to_string(),
            source,
        })?;
    let status = response.status();
    if !status.is_success() {
        return Err(Error::HttpStatus {
            url: url.to_string(),
            status,
        });
    }
    response.text().await.map_err(|source| Error::Web {
        url: url.to_string(),
        source,
    })
}

fn diff_download_url(diff_text_link: &str) -> Result<Url> {
    let base_url = api_base_url()?;
    let base = Url::parse(&base_url).map_err(|source| Error::Url {
        url: base_url,
        source,
    })?;
    let mut url = Url::parse(diff_text_link).map_err(|source| Error::Url {
        url: diff_text_link.to_owned(),
        source,
    })?;
    let base_path = base.path().trim_end_matches('/');
    let path_matches = base_path.is_empty()
        || url.path() == base_path
        || url
            .path()
            .strip_prefix(base_path)
            .is_some_and(|path| path.starts_with('/'));
    if url.scheme() != base.scheme()
        || url.host_str() != base.host_str()
        || url.port_or_known_default() != base.port_or_known_default()
        || !path_matches
    {
        return Err(Error::invalid(
            "preview diff text URL is outside the configured Launchpad API",
        ));
    }

    let web_host = match base.host_str() {
        Some("api.launchpad.net") => Some("code.launchpad.net"),
        Some("api.staging.launchpad.net") => Some("code.staging.launchpad.net"),
        Some("api.qastaging.launchpad.net") => Some("code.qastaging.launchpad.net"),
        _ => None,
    };
    if let Some(web_host) = web_host {
        let relative_path = url
            .path()
            .strip_prefix(base_path)
            .and_then(|path| path.strip_suffix("/diff_text"))
            .ok_or_else(|| Error::invalid("preview diff text URL has an invalid path"))?;
        let path = format!("{relative_path}/+files/preview.diff");
        url.set_host(Some(web_host))
            .map_err(|_| Error::invalid("preview diff text URL has an invalid host"))?;
        url.set_path(&path);
        url.set_query(None);
    }
    Ok(url)
}

async fn inline_comments(
    client: &LaunchpadClient,
    proposal: &Value,
    preview_diff_id: u64,
) -> Result<Vec<Value>> {
    let url = review_operation_url(proposal, "getInlineComments", preview_diff_id)?;
    let comments: Value = client.get_url(url.as_str()).await?;
    comments
        .as_array()
        .cloned()
        .ok_or_else(|| Error::invalid("Launchpad returned invalid inline comments"))
}

async fn review_drafts(
    client: &LaunchpadClient,
    proposal: &Value,
    preview_diff_id: u64,
) -> Result<Value> {
    let url = review_operation_url(proposal, "getDraftInlineComments", preview_diff_id)?;
    let drafts: Value = client.get_url(url.as_str()).await?;
    if drafts.is_null() {
        return Ok(json!({}));
    }
    let Some(drafts) = drafts.as_object() else {
        return Err(Error::invalid("Launchpad returned invalid review drafts"));
    };
    if drafts
        .iter()
        .any(|(line, body)| line.parse::<usize>().is_err() || !body.is_string())
    {
        return Err(Error::invalid("Launchpad returned invalid review drafts"));
    }
    Ok(Value::Object(drafts.clone()))
}

async fn save_review_drafts(
    client: &LaunchpadClient,
    proposal: &Value,
    preview_diff_id: u64,
    drafts: &Value,
) -> Result<()> {
    let url = proposal_api_url(proposal)?;
    let preview_diff_id = preview_diff_id.to_string();
    let drafts = drafts.to_string();
    client
        .post_pairs_url_ok(
            url.as_str(),
            &[
                ("ws.op", "saveDraftInlineComment"),
                ("previewdiff_id", preview_diff_id.as_str()),
                ("comments", drafts.as_str()),
            ],
        )
        .await?;
    Ok(())
}

async fn create_inline_review(
    client: &LaunchpadClient,
    proposal: &Value,
    preview_diff_id: u64,
    content: &str,
    drafts: &Value,
    vote: Option<&str>,
) -> Result<()> {
    let url = proposal_api_url(proposal)?;
    let preview_diff_id = preview_diff_id.to_string();
    let drafts = drafts.to_string();
    let mut parameters = vec![
        ("ws.op", "createComment"),
        ("content", content),
        ("previewdiff_id", preview_diff_id.as_str()),
        ("inline_comments", drafts.as_str()),
    ];
    if let Some(vote) = vote {
        parameters.push(("vote", vote));
    }
    client.post_pairs_url_ok(url.as_str(), &parameters).await?;
    Ok(())
}

async fn enforce_current_preview_diff(
    client: &LaunchpadClient,
    proposal: &Value,
    requested_id: u64,
) -> Result<Value> {
    let current = get_preview_diff(client, proposal, None).await?;
    validate_current_preview_diff(&current, requested_id)?;
    Ok(current)
}

fn validate_current_preview_diff(current: &Value, requested_id: u64) -> Result<()> {
    let current_id = preview_diff_id(current)?;
    if requested_id != current_id {
        return Err(Error::invalid(format!(
            "preview diff {requested_id} is stale; current preview diff is {current_id}"
        )));
    }
    if current.get("stale").and_then(Value::as_bool) == Some(true) {
        return Err(Error::invalid(format!(
            "preview diff {requested_id} is marked stale"
        )));
    }
    Ok(())
}

fn review_operation_url(proposal: &Value, operation: &str, preview_diff_id: u64) -> Result<Url> {
    let mut url = proposal_api_url(proposal)?;
    url.query_pairs_mut()
        .append_pair("ws.op", operation)
        .append_pair("previewdiff_id", &preview_diff_id.to_string());
    Ok(url)
}

fn proposal_api_url(proposal: &Value) -> Result<Url> {
    let self_link = render::text_field(proposal, "self_link")
        .ok_or_else(|| Error::invalid("merge proposal has no API link"))?;
    Url::parse(self_link).map_err(|source| Error::Url {
        url: self_link.to_owned(),
        source,
    })
}

async fn fetch_all_entries(client: &LaunchpadClient, first_url: &str) -> Result<Vec<Value>> {
    let mut entries = Vec::new();
    let mut seen = HashSet::new();
    let mut url = first_url.to_owned();
    loop {
        if !seen.insert(url.clone()) {
            return Err(Error::invalid(
                "Launchpad collection pagination repeated a page",
            ));
        }
        let page: Collection<Value> = client.get_url(&url).await?;
        entries.extend(page.entries);
        let Some(next_url) = page.next_collection_link else {
            break;
        };
        url = next_url;
    }
    Ok(entries)
}

async fn fetch_entries(
    client: &LaunchpadClient,
    first_url: &str,
    limit: usize,
) -> Result<Vec<Value>> {
    let mut entries = Vec::with_capacity(limit);
    let mut url = first_url.to_owned();
    while entries.len() < limit {
        let page: Collection<Value> = client.get_url(&url).await?;
        let remaining = limit - entries.len();
        entries.extend(page.entries.into_iter().take(remaining));
        let Some(next_url) = page.next_collection_link else {
            break;
        };
        url = next_url;
    }
    Ok(entries)
}

async fn get_repository(client: &LaunchpadClient, path: &str) -> Result<Value> {
    let url = client.url(&format!("/+git?ws.op=getByPath&path={}", urlenc(path)));
    let repository = client.get_url(&url).await?;
    Ok(repository)
}

async fn get_merge_proposal(client: &LaunchpadClient, repository: &str, id: u64) -> Result<Value> {
    let proposal = client.get(&format!("/{repository}/+merge/{id}")).await?;
    Ok(proposal)
}

async fn find_git_ref(
    client: &LaunchpadClient,
    repository: &str,
    requested_path: &str,
) -> Result<GitRef> {
    let requested_path = normalise_ref(requested_path);
    list_git_refs(client, repository)
        .await?
        .into_iter()
        .find(|git_ref| git_ref.path.as_deref() == Some(requested_path.as_str()))
        .ok_or_else(|| {
            Error::invalid(format!(
                "ref {requested_path} was not found in {repository}"
            ))
        })
}

async fn proposal_details(client: &LaunchpadClient, proposal: &Value) -> Result<Value> {
    let source_repository = linked_resource(client, proposal, "source_git_repository_link").await?;
    let target_repository = linked_resource(client, proposal, "target_git_repository_link").await?;
    Ok(json!({
        "kind": "branch_merge_proposal",
        "id": render::resource_id(proposal),
        "source_ref": render::scalar_field(proposal, "source_git_path"),
        "target_ref": render::scalar_field(proposal, "target_git_path"),
        "source_repository": render::scalar_field(&source_repository, "unique_name"),
        "target_repository": render::scalar_field(&target_repository, "unique_name"),
        "source_https_url": render::scalar_field(&source_repository, "git_https_url"),
        "source_ssh_url": render::scalar_field(&source_repository, "git_ssh_url"),
        "target_https_url": render::scalar_field(&target_repository, "git_https_url"),
        "target_ssh_url": render::scalar_field(&target_repository, "git_ssh_url"),
    }))
}

async fn checkout_spec(client: &LaunchpadClient, proposal: &Value) -> Result<CheckoutSpec> {
    let source_repository = linked_resource(client, proposal, "source_git_repository_link").await?;
    let target_repository = linked_resource(client, proposal, "target_git_repository_link").await?;
    let id =
        render::resource_id(proposal).ok_or_else(|| Error::invalid("merge proposal has no ID"))?;
    let source_ref = render::scalar_field(proposal, "source_git_path")
        .ok_or_else(|| Error::invalid("merge proposal has no source ref"))?;
    let source_repository_name = render::scalar_field(&source_repository, "unique_name")
        .ok_or_else(|| Error::invalid("merge proposal has no source repository"))?;
    Ok(CheckoutSpec {
        id,
        source_ref,
        source_repository: source_repository_name,
        source_https_url: render::scalar_field(&source_repository, "git_https_url"),
        source_ssh_url: render::scalar_field(&source_repository, "git_ssh_url"),
        target_https_url: render::scalar_field(&target_repository, "git_https_url"),
        target_ssh_url: render::scalar_field(&target_repository, "git_ssh_url"),
        web_link: render::scalar_field(proposal, "web_link"),
    })
}

async fn linked_resource(
    client: &LaunchpadClient,
    resource: &Value,
    link_name: &str,
) -> Result<Value> {
    let link = render::text_field(resource, link_name)
        .ok_or_else(|| Error::invalid(format!("resource has no {link_name}")))?;
    let resource = client.get_url(link).await?;
    Ok(resource)
}

fn append_values(
    query: &mut url::form_urlencoded::Serializer<'_, url::UrlQuery<'_>>,
    name: &str,
    values: Option<&OneOrMany>,
) {
    if let Some(values) = values {
        values.append_to(query, name);
    }
}

fn normalise_ref(path: &str) -> String {
    if path.starts_with("refs/") {
        path.to_owned()
    } else {
        format!("refs/heads/{path}")
    }
}

fn resource_kind(resource: &Value) -> String {
    render::text_field(resource, "resource_type_link")
        .and_then(|link| link.rsplit('#').next())
        .unwrap_or("resource")
        .to_owned()
}

fn launchpad_web_url(path: &str, kind: &str) -> String {
    let host = if kind == "bug" {
        "https://bugs.launchpad.net"
    } else {
        "https://code.launchpad.net"
    };
    format!("{host}/{path}")
}

fn encode_path(path: &str) -> String {
    path.split('/')
        .map(|segment| utf8_percent_encode(segment, NON_ALPHANUMERIC).to_string())
        .collect::<Vec<_>>()
        .join("/")
}

fn floor_char_boundary(value: &str, index: usize) -> usize {
    let mut index = index.min(value.len());
    while !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::validate_current_preview_diff;

    #[test]
    fn rejects_non_current_preview_diff() {
        let current = json!({ "id": 102, "stale": false });
        let error = validate_current_preview_diff(&current, 101).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("preview diff 101 is stale; current preview diff is 102")
        );
    }

    #[test]
    fn rejects_current_preview_diff_marked_stale() {
        let current = json!({ "id": 102, "stale": true });
        let error = validate_current_preview_diff(&current, 102).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("preview diff 102 is marked stale")
        );
    }
}
