use std::env;

use lpcli::auth;
use lpcli::client::{Collection, LaunchpadClient, urlenc};
use lpcli::error::LpError;
use lpcli::git::{GitRef, list_git_refs};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde_json::{Value, json};
use url::Url;

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
        Operation::BugCreate => create_bug(&client, request).await,
        Operation::MergeProposalCreate => create_merge_proposal(&client, request).await,
        Operation::Comment => add_comment(&client, request).await,
        Operation::SetMergeProposalStatus => set_merge_proposal_status(&client, request).await,
        Operation::MergeProposalCheckout => checkout_merge_proposal(&client, request).await,
        Operation::FileRead | Operation::MergeProposalPush => {
            Err(Error::invalid("operation dispatch is inconsistent"))
        }
    }
}

fn launchpad_client(write: bool) -> Result<LaunchpadClient> {
    let force_anonymous = env::var("OMP_LAUNCHPAD_ANONYMOUS").is_ok_and(|value| value == "1");
    if write && force_anonymous {
        return Err(Error::invalid(
            "write operations are unavailable in anonymous mode",
        ));
    }
    let credentials = if force_anonymous {
        None
    } else {
        match auth::load_credentials() {
            Ok(credentials) => Some(credentials),
            Err(LpError::NotAuthenticated) if !write => None,
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
    let target = ResourceTarget::parse(request.target()?)?;
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
        let preview_diff_link = render::text_field(&proposal, "preview_diff_link")
            .ok_or_else(|| Error::invalid("this merge proposal has no preview diff"))?;
        let preview_diff: Value = client.get_url(preview_diff_link).await?;
        let diff_text_link = render::text_field(&preview_diff, "diff_text_link")
            .ok_or_else(|| Error::invalid("this merge proposal has no preview diff text"))?;
        let mut text = client.get_text_url(diff_text_link).await?;
        let truncated = text.len() > MAX_FILE_BYTES;
        if truncated {
            text.truncate(floor_char_boundary(&text, MAX_FILE_BYTES));
            text.push_str("\n\n[Diff truncated at 2 MiB]");
        }
        let details = json!({
            "kind": "branch_merge_proposal",
            "diff": true,
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
