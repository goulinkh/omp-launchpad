use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;

use chrono::{DateTime, FixedOffset};
use futures::{StreamExt, stream};

use lpcli::auth;
use lpcli::client::{Collection, LaunchpadClient, urlenc};
use lpcli::error::LpError;
use lpcli::git::{GitRef, list_git_refs};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use serde_json::{Value, json};
use url::Url;

use crate::diff;
use crate::error::Error;
use crate::local_git::{self, CheckoutSpec};
use crate::render;
use crate::request::{
    DiscussionFormat, OneOrMany, Operation, Request, ResourceKind, ResourceTarget,
    normalise_repository,
};
use crate::response::OperationResult;
use crate::result::Result;

const MAX_FILE_BYTES: usize = 2 * 1024 * 1024;
const MAX_ITEMS: usize = 50;
const MAX_BRANCH_FALLBACK_ITEMS: usize = 250;
const MAX_REPOSITORY_CANDIDATES: usize = 100;
const GIT_PATH_SEGMENT_ENCODE_SET: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~')
    .remove(b'+');

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
        Operation::MergeProposalForBranch => view_merge_proposal_for_branch(&client, request).await,
        Operation::CurrentMergeProposal => view_current_merge_proposal(&client, request).await,
        Operation::MergeProposalDiscussion => {
            view_merge_proposal_discussion(&client, request).await
        }
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
    select_preview_diff(&mut target, request.preview_diff_id)?;
    match &target.kind {
        ResourceKind::Bug { id } => view_bug(client, &target, *id).await,
        ResourceKind::MergeProposal { repository, id } => {
            view_merge_proposal(client, &target, repository, *id).await
        }
        ResourceKind::MergeProposalId { .. } => {
            let (_, proposal) = resolve_merge_proposal_target(client, request.target()?).await?;
            view_merge_proposal_value(client, &target, proposal).await
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

fn select_preview_diff(target: &mut ResourceTarget, preview_diff_id: Option<u64>) -> Result<()> {
    let Some(preview_diff_id) = preview_diff_id else {
        return Ok(());
    };
    if preview_diff_id == 0 {
        return Err(Error::invalid("preview_diff_id must be greater than zero"));
    }
    if !matches!(
        &target.kind,
        ResourceKind::MergeProposal { .. } | ResourceKind::MergeProposalId { .. }
    ) {
        return Err(Error::invalid(
            "preview_diff_id requires a merge proposal target",
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
    target.diff = true;
    target.preview_diff_id = Some(preview_diff_id);
    Ok(())
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
    view_merge_proposal_value(client, target, proposal).await
}

async fn view_merge_proposal_value(
    client: &LaunchpadClient,
    target: &ResourceTarget,
    proposal: Value,
) -> Result<OperationResult> {
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
    let comments_url = render::text_field(&proposal, "all_comments_collection_link")
        .ok_or_else(|| Error::invalid("merge proposal has no comments collection"))?;
    let comments = if target.include_comments {
        fetch_entries(client, comments_url, target.comment_limit).await?
    } else {
        Vec::new()
    };
    let votes_url = render::text_field(&proposal, "votes_collection_link")
        .ok_or_else(|| Error::invalid("merge proposal has no review vote collection"))?;
    let votes = fetch_entries(client, votes_url, MAX_ITEMS)
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
    let proposal_id =
        render::resource_id(&proposal).ok_or_else(|| Error::invalid("merge proposal has no ID"))?;
    let text = render::render_preview_diffs(&preview_diffs, current_id, &proposal_id);
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
    let proposal_id =
        render::resource_id(&proposal).ok_or_else(|| Error::invalid("merge proposal has no ID"))?;
    let text = render::render_inline_comments(&comments, preview_diff_id, &proposal_id);
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
    let proposal_id =
        render::resource_id(&proposal).ok_or_else(|| Error::invalid("merge proposal has no ID"))?;
    let text = render::render_review_drafts(&drafts, preview_diff_id, &proposal_id);
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
    let proposal_id =
        render::resource_id(&proposal).ok_or_else(|| Error::invalid("merge proposal has no ID"))?;
    let text = render::render_diff_location(&location, preview_diff_id, &proposal_id);
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
    let repository_path = normalise_repository(request.repository()?)?;
    let repository = get_repository(client, &repository_path).await?;
    let repository_url = render::text_field(&repository, "self_link")
        .ok_or_else(|| Error::invalid("Launchpad repository has no API link"))?;
    let mut url = Url::parse(repository_url).map_err(|source| Error::Url {
        url: repository_url.to_owned(),
        source,
    })?;
    let requested_limit = request.limit();
    let fetch_limit = requested_limit + 1;
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("ws.op", "getMergeProposals");
        query.append_pair("ws.size", &fetch_limit.to_string());
        append_values(&mut query, "status", request.status.as_ref());
    }
    let mut proposals = fetch_entries(client, url.as_str(), fetch_limit).await?;
    let truncated = proposals.len() > requested_limit;
    proposals.truncate(requested_limit);
    let text = render::render_proposal_search(&repository, &proposals, request.limit, truncated);
    let name = render::text_field(&repository, "unique_name");
    let details = json!({
        "count": proposals.len(),
        "limit": request.limit,
        "truncated": truncated,
        "repository": name,
    });
    Ok(OperationResult::new(text).with_details(details))
}

async fn view_merge_proposal_for_branch(
    client: &LaunchpadClient,
    request: &Request,
) -> Result<OperationResult> {
    let (repository_path, branch, inferred, requested_repository) =
        branch_selector(request).await?;
    let selection = merge_proposal_for_branch(client, &repository_path, &branch, request).await?;
    proposal_lookup_result(
        client,
        request,
        selection,
        ProposalLookupContext {
            requested_repository: &requested_repository,
            branch: &branch,
            inferred,
            kind: "merge_proposal_for_branch",
            metadata: None,
        },
    )
    .await
}

async fn view_current_merge_proposal(
    client: &LaunchpadClient,
    request: &Request,
) -> Result<OperationResult> {
    let current = local_git::current_repository().await?;
    let requested_repository = &current.selected_remote.url;
    let repository_path = normalise_repository(requested_repository)?;
    let selection =
        merge_proposal_for_branch(client, &repository_path, &current.branch, request).await?;
    let context = json!({
        "working_directory": current.working_directory,
        "selected_remote": {
            "name": current.selected_remote.name,
            "url": current.selected_remote.url,
        },
        "inspected_remotes": current.remotes.into_iter().map(|remote| {
            json!({ "name": remote.name, "url": remote.url })
        }).collect::<Vec<_>>(),
    });
    proposal_lookup_result(
        client,
        request,
        selection,
        ProposalLookupContext {
            requested_repository,
            branch: &current.branch,
            inferred: true,
            kind: "current_merge_proposal",
            metadata: Some(context),
        },
    )
    .await
}

struct ProposalLookupContext<'value> {
    requested_repository: &'value str,
    branch: &'value str,
    inferred: bool,
    kind: &'value str,
    metadata: Option<Value>,
}

async fn proposal_lookup_result(
    client: &LaunchpadClient,
    request: &Request,
    selection: ProposalLookup,
    context: ProposalLookupContext<'_>,
) -> Result<OperationResult> {
    let selection = match selection {
        ProposalLookup::Found(selection) => selection,
        ProposalLookup::Missing {
            repository,
            inspected_repositories,
        } => {
            let branch = normalise_ref(context.branch);
            let text = format!("# No merge proposal found for {repository}:{branch}");
            let details = json!({
                "kind": context.kind,
                "found": false,
                "repository": repository,
                "requested_repository": context.requested_repository,
                "inspected_repositories": inspected_repositories,
                "branch": branch,
                "inferred": context.inferred,
                "selection_filters": proposal_selection_filters(request),
                "context": context.metadata,
            });
            return Ok(OperationResult::new(text).with_details(details));
        }
    };
    let canonical_repository = render::text_field(&selection.repository, "unique_name")
        .unwrap_or(repository_identifier(&selection.repository));
    let source_url = render::text_field(&selection.proposal, "web_link").map(str::to_owned);
    let resolution = if context.inferred {
        "current Git checkout"
    } else if selection.related_repository {
        "related target repository"
    } else {
        "explicit repository and branch"
    };
    let text = render::render_proposal_lookup(
        &selection.proposal,
        &selection.alternatives,
        canonical_repository,
        &normalise_ref(context.branch),
        resolution,
        &selection.reason,
    );
    let selected_proposal = proposal_details(client, &selection.proposal).await?;
    let details = json!({
        "kind": context.kind,
        "found": true,
        "repository": canonical_repository,
        "requested_repository": context.requested_repository,
        "branch": normalise_ref(context.branch),
        "inferred": context.inferred,
        "related_repository": selection.related_repository,
        "candidate_count": selection.candidate_count,
        "matching_proposals": selection.alternatives.len() + 1,
        "selected_proposal": selected_proposal,
        "selection_reason": selection.reason,
        "selection_filters": proposal_selection_filters(request),
        "other_proposals": selection.alternatives.iter().map(proposal_summary).collect::<Vec<_>>(),
        "context": context.metadata,
    });
    Ok(OperationResult::new(text)
        .with_source_url(source_url)
        .with_details(details))
}

async fn view_merge_proposal_discussion(
    client: &LaunchpadClient,
    request: &Request,
) -> Result<OperationResult> {
    let has_target = request
        .target
        .as_deref()
        .is_some_and(|target| !target.trim().is_empty());
    let (proposal, lookup, selection_reason) = if has_target {
        let (_, proposal) = request_merge_proposal(client, request).await?;
        (
            proposal,
            None,
            "selected the explicitly requested merge proposal".to_owned(),
        )
    } else {
        let (repository_path, branch, inferred, requested_repository) =
            branch_selector(request).await?;
        let selection =
            merge_proposal_for_branch(client, &repository_path, &branch, request).await?;
        let selection = match selection {
            ProposalLookup::Found(selection) => selection,
            missing @ ProposalLookup::Missing { .. } => {
                let result = proposal_lookup_result(
                    client,
                    request,
                    missing,
                    ProposalLookupContext {
                        requested_repository: &requested_repository,
                        branch: &branch,
                        inferred,
                        kind: "merge_proposal_discussion",
                        metadata: None,
                    },
                )
                .await?;
                return Ok(format_discussion_result(
                    result,
                    request.format.unwrap_or_default(),
                ));
            }
        };
        let canonical_repository = render::text_field(&selection.repository, "unique_name")
            .unwrap_or(repository_identifier(&selection.repository));
        let lookup = json!({
            "repository": canonical_repository,
            "found": true,
            "requested_repository": requested_repository,
            "branch": normalise_ref(&branch),
            "inferred": inferred,
            "related_repository": selection.related_repository,
            "candidate_count": selection.candidate_count,
            "matching_proposals": selection.alternatives.len() + 1,
            "selection_filters": proposal_selection_filters(request),
        });
        (selection.proposal, Some(lookup), selection.reason)
    };

    let comments_kind = request.comments.unwrap_or_default();
    let since = request.since()?;
    let reviewer = request.reviewer.as_deref().map(str::trim);
    let (current_preview_diff_id, current_preview_diff_stale) =
        if render::text_field(&proposal, "preview_diff_link").is_some() {
            let current_preview_diff = get_preview_diff(client, &proposal, None).await?;
            (
                Some(preview_diff_id(&current_preview_diff)?),
                current_preview_diff
                    .get("stale")
                    .and_then(Value::as_bool)
                    .unwrap_or_default(),
            )
        } else {
            (None, false)
        };
    let mut people = HashMap::new();
    let mut identity_resolution_failures = HashSet::new();

    let (general_comments, review_votes, review_requests) = if comments_kind.includes_general() {
        let comments_url = render::text_field(&proposal, "all_comments_collection_link")
            .ok_or_else(|| Error::invalid("merge proposal has no comments collection"))?;
        let raw_comments = fetch_all_entries(client, comments_url).await?;
        let votes_url = render::text_field(&proposal, "votes_collection_link")
            .ok_or_else(|| Error::invalid("merge proposal has no review vote collection"))?;
        let raw_review_requests = fetch_all_entries(client, votes_url).await?;
        load_people(
            client,
            raw_comments.iter().chain(&raw_review_requests),
            &mut people,
            &mut identity_resolution_failures,
        )
        .await;
        let comments = raw_comments
            .iter()
            .map(|comment| normalise_general_comment(comment, &people))
            .collect::<Vec<_>>();
        let general_comments = comments
            .iter()
            .filter(|comment| {
                comment.get("body").is_some_and(|body| !body.is_null())
                    && matches_discussion_filters(comment, "author", since.as_ref(), reviewer)
            })
            .cloned()
            .collect();
        let review_votes = comments
            .into_iter()
            .filter(|comment| {
                comment.get("vote").is_some_and(|vote| !vote.is_null())
                    && matches_discussion_filters(comment, "author", since.as_ref(), reviewer)
            })
            .collect();
        let review_requests = raw_review_requests
            .iter()
            .map(|vote| normalise_review_assignment(vote, &people))
            .filter(|assignment| {
                matches_discussion_filters(assignment, "reviewer", since.as_ref(), reviewer)
            })
            .collect();
        (general_comments, review_votes, review_requests)
    } else {
        (Vec::new(), Vec::new(), Vec::new())
    };

    let preview_diffs = if comments_kind.includes_inline() {
        get_preview_diffs(client, &proposal).await?
    } else {
        Vec::new()
    };
    let mut diff_discussions = Vec::with_capacity(preview_diffs.len());
    let mut inline_threads = Vec::new();
    for preview_diff in &preview_diffs {
        let id = preview_diff_id(preview_diff)?;
        let current = current_preview_diff_id == Some(id);
        if request.current_diff_only.unwrap_or_default() && !current {
            continue;
        }
        let stale = preview_diff
            .get("stale")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let state = thread_state(current, stale);
        if request.unresolved_only.unwrap_or_default() && state != "open" {
            continue;
        }
        let comments = inline_comments(client, &proposal, id).await?;
        load_people(
            client,
            comments.iter(),
            &mut people,
            &mut identity_resolution_failures,
        )
        .await;
        let diff_text = if comments.is_empty() {
            None
        } else {
            Some(get_diff_text(preview_diff).await?)
        };
        let mut grouped = BTreeMap::<usize, Vec<&Value>>::new();
        for comment in &comments {
            let line = render::text_field(comment, "line_number")
                .and_then(|line| line.parse().ok())
                .ok_or_else(|| {
                    Error::invalid("Launchpad returned an inline comment without a diff line")
                })?;
            grouped.entry(line).or_default().push(comment);
        }
        let mut threads = Vec::with_capacity(grouped.len());
        for (line, mut comments) in grouped {
            comments.sort_by_key(|comment| render::text_field(comment, "date"));
            let location = diff_text
                .as_deref()
                .and_then(|diff_text| diff::locate_diff_line(diff_text, line));
            let comments = comments
                .into_iter()
                .enumerate()
                .map(|(index, comment)| normalise_inline_comment(comment, index, &people))
                .filter(|comment| {
                    matches_discussion_filters(comment, "author", since.as_ref(), reviewer)
                })
                .collect::<Vec<_>>();
            if comments.is_empty() {
                continue;
            }
            let source_line = location
                .as_ref()
                .filter(|location| location.side == diff::DiffSide::Original)
                .map(|location| location.file_line);
            let target_line = location
                .as_ref()
                .filter(|location| location.side == diff::DiffSide::Modified)
                .map(|location| location.file_line);
            let thread = json!({
                "thread_id": format!("{id}:{line}"),
                "preview_diff_id": id,
                "current": current,
                "stale": stale,
                "state": state,
                "diff_line": line,
                "file": location.as_ref().map(|location| location.path.as_str()),
                "source_line": source_line,
                "target_line": target_line,
                "location": location,
                "comments": comments,
            });
            inline_threads.push(thread.clone());
            threads.push(thread);
        }
        diff_discussions.push(json!({
            "preview_diff_id": id,
            "date_created": render::text_field(preview_diff, "date_created"),
            "current": current,
            "stale": stale,
            "state": state,
            "threads": threads,
        }));
    }
    diff_discussions.sort_by_key(|diff| {
        diff.get("preview_diff_id")
            .and_then(Value::as_u64)
            .unwrap_or_default()
    });
    inline_threads.sort_by_key(|thread| {
        (
            thread
                .get("preview_diff_id")
                .and_then(Value::as_u64)
                .unwrap_or_default(),
            thread
                .get("diff_line")
                .and_then(Value::as_u64)
                .unwrap_or_default(),
        )
    });

    let preview_diff_summaries = diff_discussions
        .iter()
        .map(|preview_diff| {
            json!({
                "preview_diff_id": preview_diff.get("preview_diff_id"),
                "date_created": preview_diff.get("date_created"),
                "current": preview_diff.get("current"),
                "stale": preview_diff.get("stale"),
                "thread_count": preview_diff
                    .get("threads")
                    .and_then(Value::as_array)
                    .map(Vec::len)
                    .unwrap_or_default(),
            })
        })
        .collect::<Vec<_>>();
    let review_summary = build_review_summary(
        &general_comments,
        &review_votes,
        &review_requests,
        &inline_threads,
    );
    let source_url = render::text_field(&proposal, "web_link").map(str::to_owned);
    let markdown = render::render_proposal_discussion(
        &proposal,
        current_preview_diff_id,
        &review_summary,
        render::DiscussionSections {
            general: comments_kind.includes_general(),
            inline: comments_kind.includes_inline(),
            current_preview_diff_stale,
            identity_resolution_failures: identity_resolution_failures.len(),
        },
    );
    let coverage = if !comments_kind.includes_inline() {
        "not_requested"
    } else if request.current_diff_only.unwrap_or_default()
        || request.unresolved_only.unwrap_or_default()
    {
        "current_preview_diff"
    } else {
        "all_preview_diffs"
    };
    let format = request.format.unwrap_or_default();
    let details = json!({
        "kind": "merge_proposal_discussion",
        "found": true,
        "selected_proposal": proposal_details(client, &proposal).await?,
        "selection_reason": selection_reason,
        "lookup": lookup,
        "current_preview_diff_id": current_preview_diff_id,
        "current_preview_diff_stale": current_preview_diff_stale,
        "coverage": coverage,
        "format": format,
        "filters": {
            "comments": comments_kind,
            "current_diff_only": request.current_diff_only.unwrap_or_default(),
            "unresolved_only": request.unresolved_only.unwrap_or_default(),
            "since": request.since.as_deref(),
            "reviewer": reviewer,
        },
        "review_summary": review_summary,
        "identity_resolution_failures": identity_resolution_failures.len(),
        "review_votes": review_votes,
        "general_comments": general_comments,
        "review_requests": review_requests,
        "inline_threads": inline_threads,
        "preview_diffs": preview_diff_summaries,
    });
    Ok(format_discussion_result(
        OperationResult::new(markdown)
            .with_source_url(source_url)
            .with_details(details),
        format,
    ))
}

fn format_discussion_result(
    mut result: OperationResult,
    format: DiscussionFormat,
) -> OperationResult {
    if format == DiscussionFormat::Summary {
        return result;
    }
    let details = Value::Object(std::mem::take(&mut result.details));
    result.text = if format == DiscussionFormat::Structured {
        details.to_string()
    } else {
        format!(
            "{}\n\n## Structured data\n\n```json\n{details}\n```",
            result.text
        )
    };
    result.with_details(details)
}

struct ProposalSelection {
    repository: Value,
    proposal: Value,
    alternatives: Vec<Value>,
    related_repository: bool,
    reason: String,
    candidate_count: usize,
}

async fn branch_selector(request: &Request) -> Result<(String, String, bool, String)> {
    if let (Some(repository), Some(branch)) = (
        request
            .repository
            .as_deref()
            .filter(|repository| !repository.trim().is_empty()),
        request
            .branch
            .as_deref()
            .filter(|branch| !branch.trim().is_empty()),
    ) {
        return Ok((
            normalise_repository(repository)?,
            branch.trim().to_owned(),
            false,
            repository.to_owned(),
        ));
    }
    let (repository, branch) = local_git::current_repository_branch().await?;
    Ok((normalise_repository(&repository)?, branch, true, repository))
}

enum ProposalLookup {
    Found(ProposalSelection),
    Missing {
        repository: String,
        inspected_repositories: Vec<String>,
    },
}

async fn merge_proposal_for_branch(
    client: &LaunchpadClient,
    repository_path: &str,
    branch: &str,
    request: &Request,
) -> Result<ProposalLookup> {
    let repository = get_repository(client, repository_path)
        .await
        .map_err(|source| {
            Error::context(
                format!("cannot look up repository {repository_path} for branch {branch}"),
                source,
            )
        })?;
    let branch = normalise_ref(branch);
    let (proposals, requested_fallback_capped) =
        repository_branch_proposals(client, &repository, &branch).await?;
    let requested_candidate_count = proposals.len();
    let eligible = eligible_proposals(proposals, request);
    if !eligible.is_empty() {
        let (proposal, alternatives, reason) = select_proposal(eligible, request)?;
        return Ok(ProposalLookup::Found(ProposalSelection {
            repository,
            proposal,
            alternatives,
            related_repository: false,
            reason,
            candidate_count: requested_candidate_count,
        }));
    }

    let target_link = render::text_field(&repository, "target_link")
        .ok_or_else(|| Error::invalid("Launchpad repository has no target"))?;
    let repository_name = render::text_field(&repository, "name");
    let canonical_repository =
        render::text_field(&repository, "unique_name").unwrap_or(repository_path);
    let (related_repositories, truncated) =
        target_repositories(client, target_link, repository_name).await?;
    let mut inspected_repositories = Vec::new();
    for candidate in &related_repositories {
        if let Some(name) = render::text_field(candidate, "unique_name")
            && name != canonical_repository
            && !inspected_repositories
                .iter()
                .any(|inspected| inspected == name)
        {
            inspected_repositories.push(name.to_owned());
        }
    }
    let mut matches = stream::iter(related_repositories)
        .map(|related_repository| {
            let branch = branch.as_str();
            async move {
                if render::text_field(&related_repository, "unique_name")
                    == Some(canonical_repository)
                {
                    return Ok(None);
                }
                let proposals =
                    related_repository_branch_proposals(client, &related_repository, branch)
                        .await
                        .map_err(|error| {
                            Error::context(
                                format!(
                                    "cannot inspect related repository {} for {branch}",
                                    repository_identifier(&related_repository)
                                ),
                                error,
                            )
                        })?;
                let candidate_count = proposals.len();
                let proposals = eligible_proposals(proposals, request);
                Ok((!proposals.is_empty()).then_some((
                    related_repository,
                    proposals,
                    candidate_count,
                )))
            }
        })
        .buffer_unordered(8)
        .collect::<Vec<Result<Option<(Value, Vec<Value>, usize)>>>>()
        .await
        .into_iter()
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();

    if matches.len() > 1 {
        let candidates = matches
            .iter()
            .map(|(repository, proposals, _)| {
                let repository =
                    render::text_field(repository, "unique_name").unwrap_or("unknown repository");
                let proposal_ids = proposals
                    .iter()
                    .filter_map(render::resource_id)
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{repository}:{branch} (MP {proposal_ids})")
            })
            .collect::<Vec<_>>()
            .join("; ");
        return Err(Error::invalid(format!(
            "cannot select a merge proposal for {canonical_repository}:{branch}; matching repositories: {candidates}; retry with a full merge proposal target or narrow with status and target_branch"
        )));
    }
    let Some((repository, proposals, candidate_count)) = matches.pop() else {
        if requested_fallback_capped || truncated {
            let requested_cap = if requested_fallback_capped {
                format!(
                    "; requested repository search was limited to its newest {MAX_BRANCH_FALLBACK_ITEMS} proposals"
                )
            } else {
                String::new()
            };
            let related_cap = if truncated {
                format!(
                    "; related lookup was limited to the first {MAX_REPOSITORY_CANDIDATES} likely repositories"
                )
            } else {
                String::new()
            };
            let inspected = if inspected_repositories.is_empty() {
                "none".to_owned()
            } else {
                inspected_repositories.join(", ")
            };
            return Err(Error::invalid(format!(
                "cannot establish whether a merge proposal exists for {canonical_repository}:{branch}; inspected repositories: {canonical_repository}, {inspected}; filters: {}; retry with a full merge proposal target or narrow the search{requested_cap}{related_cap}",
                proposal_selection_filter_text(request)
            )));
        }
        return Ok(ProposalLookup::Missing {
            repository: canonical_repository.to_owned(),
            inspected_repositories,
        });
    };
    let (proposal, alternatives, reason) = select_proposal(proposals, request)?;
    Ok(ProposalLookup::Found(ProposalSelection {
        repository,
        proposal,
        alternatives,
        related_repository: true,
        reason,
        candidate_count: requested_candidate_count + candidate_count,
    }))
}

async fn repository_branch_proposals(
    client: &LaunchpadClient,
    repository: &Value,
    branch: &str,
) -> Result<(Vec<Value>, bool)> {
    let canonical_repository = render::text_field(repository, "unique_name").ok_or_else(|| {
        Error::invalid(format!(
            "cannot inspect repository {}; Launchpad omitted its unique name; accepted syntax: lp:<project>, lp://~owner/project/+git/repository, or a git.launchpad.net URL; retry with the canonical repository path",
            repository_identifier(repository)
        ))
    })?;
    let git_ref = list_git_refs(client, canonical_repository)
        .await?
        .into_iter()
        .find(|git_ref| git_ref.path.as_deref() == Some(branch));
    let ref_exists = git_ref.is_some();
    let collection_url = if let Some(self_link) = git_ref
        .as_ref()
        .and_then(|git_ref| git_ref.self_link.as_deref())
    {
        format!("{}/landing_targets", self_link.trim_end_matches('/'))
    } else {
        render::text_field(repository, "landing_targets_collection_link")
            .ok_or_else(|| Error::invalid("Launchpad repository has no merge proposal collection"))?
            .to_owned()
    };
    let mut entries = if ref_exists {
        fetch_all_entries(client, &collection_url).await?
    } else {
        fetch_entries(client, &collection_url, MAX_BRANCH_FALLBACK_ITEMS + 1).await?
    };
    let fallback_was_capped = !ref_exists && entries.len() > MAX_BRANCH_FALLBACK_ITEMS;
    if fallback_was_capped {
        entries.truncate(MAX_BRANCH_FALLBACK_ITEMS);
    }
    let proposals = entries
        .into_iter()
        .filter(|proposal| render::text_field(proposal, "source_git_path") == Some(branch))
        .collect();
    Ok((proposals, fallback_was_capped))
}

async fn related_repository_branch_proposals(
    client: &LaunchpadClient,
    repository: &Value,
    branch: &str,
) -> Result<Vec<Value>> {
    let repository_url = render::text_field(repository, "self_link")
        .ok_or_else(|| Error::invalid("Launchpad repository has no API link"))?;
    let mut ref_url = Url::parse(repository_url).map_err(|source| Error::Url {
        url: repository_url.to_owned(),
        source,
    })?;
    ref_url
        .query_pairs_mut()
        .append_pair("ws.op", "getRefByPath")
        .append_pair("path", branch);
    let git_ref: Value = match client.get_url(ref_url.as_str()).await {
        Ok(git_ref) => git_ref,
        Err(LpError::NotFound(_)) => return Ok(Vec::new()),
        Err(source) => return Err(source.into()),
    };
    if git_ref.is_null() {
        return Ok(Vec::new());
    }
    let ref_url = render::text_field(&git_ref, "self_link")
        .ok_or_else(|| Error::invalid("Launchpad Git reference has no API link"))?;
    let proposals = fetch_all_entries(
        client,
        &format!("{}/landing_targets", ref_url.trim_end_matches('/')),
    )
    .await?
    .into_iter()
    .filter(|proposal| render::text_field(proposal, "source_git_path") == Some(branch))
    .collect();
    Ok(proposals)
}

async fn target_repositories(
    client: &LaunchpadClient,
    target_link: &str,
    repository_name: Option<&str>,
) -> Result<(Vec<Value>, bool)> {
    let mut url = Url::parse(&client.url("/+git")).map_err(|source| Error::Url {
        url: client.url("/+git"),
        source,
    })?;
    url.query_pairs_mut()
        .append_pair("ws.op", "getRepositories")
        .append_pair("target", target_link)
        .append_pair("order_by", "most recently changed first")
        .append_pair("ws.size", &(MAX_REPOSITORY_CANDIDATES + 1).to_string());
    let mut repositories =
        fetch_entries(client, url.as_str(), MAX_REPOSITORY_CANDIDATES + 1).await?;
    let truncated = repositories.len() > MAX_REPOSITORY_CANDIDATES;
    repositories.truncate(MAX_REPOSITORY_CANDIDATES);
    repositories.retain(|repository| {
        render::text_field(repository, "name") == repository_name
            || repository
                .get("target_default")
                .and_then(Value::as_bool)
                .unwrap_or_default()
    });
    Ok((repositories, truncated))
}

fn eligible_proposals(proposals: Vec<Value>, request: &Request) -> Vec<Value> {
    let requested_statuses = request.status.as_ref();
    let explicitly_requests_superseded = requested_statuses.is_some_and(|statuses| {
        statuses
            .values()
            .any(|status| status.eq_ignore_ascii_case("Superseded"))
    });
    let include_superseded =
        request.include_superseded.unwrap_or_default() || explicitly_requests_superseded;
    let target_branch = request.target_branch.as_deref().map(normalise_ref);
    proposals
        .into_iter()
        .filter(|proposal| {
            let status = render::text_field(proposal, "queue_status").unwrap_or_default();
            let status_matches = requested_statuses.is_none_or(|statuses| {
                statuses
                    .values()
                    .any(|requested| requested.eq_ignore_ascii_case(status))
            });
            let target_matches = target_branch.as_deref().is_none_or(|target| {
                render::text_field(proposal, "target_git_path") == Some(target)
            });
            status_matches
                && target_matches
                && (include_superseded || !status.eq_ignore_ascii_case("Superseded"))
        })
        .collect()
}

fn select_proposal(
    mut proposals: Vec<Value>,
    request: &Request,
) -> Result<(Value, Vec<Value>, String)> {
    let latest = request.latest.unwrap_or_default();
    proposals.sort_by(|left, right| {
        if latest {
            proposal_sort_key(left).cmp(&proposal_sort_key(right))
        } else {
            proposal_selection_key(left).cmp(&proposal_selection_key(right))
        }
    });
    let proposal = proposals
        .pop()
        .ok_or_else(|| Error::invalid("cannot select a merge proposal from an empty result"))?;
    let status = render::text_field(&proposal, "queue_status").unwrap_or("unknown");
    let reason = if latest {
        "selected the latest matching proposal by creation date and ID".to_owned()
    } else if is_active_proposal_status(status) {
        "preferred an active proposal, then selected the newest by creation date and ID".to_owned()
    } else if status.eq_ignore_ascii_case("Merged") {
        "selected the newest merged proposal because no active proposal matched".to_owned()
    } else {
        format!(
            "selected the newest {status} proposal because no active or merged proposal matched"
        )
    };
    Ok((proposal, proposals, reason))
}

fn proposal_selection_key(proposal: &Value) -> (u8, &str, u64) {
    let status = render::text_field(proposal, "queue_status").unwrap_or_default();
    let priority = if is_active_proposal_status(status) {
        3
    } else if status.eq_ignore_ascii_case("Merged") {
        2
    } else if status.eq_ignore_ascii_case("Rejected") {
        1
    } else {
        0
    };
    let (date, id) = proposal_sort_key(proposal);
    (priority, date, id)
}

fn is_active_proposal_status(status: &str) -> bool {
    !["Merged", "Rejected", "Superseded"]
        .iter()
        .any(|terminal| terminal.eq_ignore_ascii_case(status))
}

fn proposal_sort_key(proposal: &Value) -> (&str, u64) {
    let date = render::text_field(proposal, "date_created").unwrap_or_default();
    let id = render::resource_id(proposal)
        .and_then(|id| id.parse().ok())
        .unwrap_or_default();
    (date, id)
}

fn proposal_selection_filters(request: &Request) -> Value {
    json!({
        "status": request.status.as_ref().map(|statuses| statuses.values().collect::<Vec<_>>()),
        "target_branch": request.target_branch.as_deref().map(normalise_ref),
        "latest": request.latest.unwrap_or_default(),
        "include_superseded": request.include_superseded.unwrap_or_default(),
    })
}

fn proposal_selection_filter_text(request: &Request) -> String {
    let filters = proposal_selection_filters(request);
    serde_json::to_string(&filters).unwrap_or_else(|_| "{}".to_owned())
}

fn proposal_summary(proposal: &Value) -> Value {
    json!({
        "id": render::resource_id(proposal),
        "status": render::text_field(proposal, "queue_status"),
        "url": render::text_field(proposal, "web_link"),
    })
}

fn build_review_summary(
    general_comments: &[Value],
    review_votes: &[Value],
    review_requests: &[Value],
    inline_threads: &[Value],
) -> Value {
    let mut vote_counts = BTreeMap::<String, usize>::new();
    let mut previous_votes = BTreeMap::<String, String>::new();
    let mut current_votes = BTreeMap::<String, Value>::new();
    let mut transitions = Vec::new();
    let mut ordered_votes = review_votes.iter().collect::<Vec<_>>();
    ordered_votes.sort_by_key(|vote| render::text_field(vote, "created_at"));
    for vote in ordered_votes {
        let Some(value) = render::text_field(vote, "vote") else {
            continue;
        };
        let reviewer = vote
            .get("author")
            .and_then(|author| render::text_field(author, "username"))
            .unwrap_or("unknown")
            .to_owned();
        *vote_counts.entry(value.to_owned()).or_default() += 1;
        let previous = previous_votes.insert(reviewer.clone(), value.to_owned());
        if previous
            .as_deref()
            .is_some_and(|previous| previous != value)
        {
            transitions.push(json!({
                "reviewer": reviewer.clone(),
                "from": previous,
                "to": value,
                "at": render::text_field(vote, "created_at"),
            }));
        }
        current_votes.insert(
            reviewer.clone(),
            json!({
                "reviewer": reviewer,
                "vote": value,
                "at": render::text_field(vote, "created_at"),
            }),
        );
    }
    let state_count = |state: &str| {
        inline_threads
            .iter()
            .filter(|thread| render::text_field(thread, "state") == Some(state))
            .count()
    };
    json!({
        "general_comment_count": general_comments.len(),
        "inline_thread_count": inline_threads.len(),
        "current_open_inline_thread_count": state_count("open"),
        "outdated_inline_thread_count": state_count("outdated"),
        "superseded_inline_thread_count": state_count("superseded"),
        "resolved_inline_thread_count": Value::Null,
        "resolution_tracking": "Launchpad does not expose resolved inline-thread state",
        "review_request_count": review_requests.len(),
        "pending_review_request_count": review_requests.iter().filter(|request| {
            request.get("pending").and_then(Value::as_bool).unwrap_or_default()
        }).count(),
        "review_vote_count": review_votes.len(),
        "review_vote_counts": vote_counts,
        "current_review_votes": current_votes.into_values().collect::<Vec<_>>(),
        "review_vote_transitions": transitions,
    })
}

fn repository_identifier(repository: &Value) -> &str {
    render::text_field(repository, "web_link").unwrap_or("repository")
}

async fn load_people<'entry>(
    client: &LaunchpadClient,
    entries: impl Iterator<Item = &'entry Value>,
    people: &mut HashMap<String, Value>,
    failures: &mut HashSet<String>,
) {
    let mut links = HashSet::new();
    for entry in entries {
        for field in ["author", "owner", "person", "reviewer", "registrant"] {
            if let Some(link) = person_link(entry, field)
                && !people.contains_key(link)
                && !failures.contains(link)
            {
                links.insert(link.to_owned());
            }
        }
    }
    let loaded = stream::iter(links)
        .map(|link| async move {
            let person = client.get_url(&link).await.ok();
            (link, person)
        })
        .buffer_unordered(8)
        .collect::<Vec<_>>()
        .await;
    for (link, person) in loaded {
        if let Some(person) = person {
            people.insert(link, person);
        } else {
            failures.insert(link);
        }
    }
}

fn normalise_general_comment(comment: &Value, people: &HashMap<String, Value>) -> Value {
    let body = render::text_field(comment, "message_body")
        .or_else(|| render::text_field(comment, "content"))
        .filter(|body| !body.trim().is_empty());
    json!({
        "id": comment.get("id"),
        "author": normalise_identity(comment, "author", people)
            .or_else(|| normalise_identity(comment, "owner", people)),
        "created_at": render::text_field(comment, "date_created"),
        "vote": render::text_field(comment, "vote"),
        "title": render::text_field(comment, "title"),
        "body": body,
        "url": render::text_field(comment, "web_link"),
    })
}

fn normalise_inline_comment(
    comment: &Value,
    index: usize,
    people: &HashMap<String, Value>,
) -> Value {
    let body = render::text_field(comment, "text").filter(|body| !body.trim().is_empty());
    json!({
        "sequence": index + 1,
        "author": normalise_identity(comment, "person", people),
        "created_at": render::text_field(comment, "date"),
        "body": body,
    })
}

fn normalise_review_assignment(vote: &Value, people: &HashMap<String, Value>) -> Value {
    json!({
        "reviewer": normalise_identity(vote, "reviewer", people),
        "registrant": normalise_identity(vote, "registrant", people),
        "created_at": render::text_field(vote, "date_created"),
        "review_type": render::text_field(vote, "review_type"),
        "pending": vote.get("is_pending").and_then(Value::as_bool),
        "comment_url": render::text_field(vote, "comment_link"),
        "url": render::text_field(vote, "web_link"),
    })
}

fn normalise_identity(
    value: &Value,
    field: &str,
    people: &HashMap<String, Value>,
) -> Option<Value> {
    let link = person_link(value, field);
    let person = value
        .get(field)
        .filter(|person| person.is_object())
        .or_else(|| link.and_then(|link| people.get(link)));
    if person.is_none() && link.is_none() {
        return None;
    }
    let username = person
        .and_then(|person| render::text_field(person, "name"))
        .or_else(|| {
            link.and_then(|link| {
                link.trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .map(|name| name.trim_start_matches('~'))
            })
        });
    let display_name = person
        .and_then(|person| render::text_field(person, "display_name"))
        .or(username);
    let url = person
        .and_then(|person| render::text_field(person, "web_link"))
        .map(str::to_owned)
        .or_else(|| username.map(|username| format!("https://launchpad.net/~{username}")));
    Some(json!({
        "username": username,
        "display_name": display_name,
        "url": url,
    }))
}

fn person_link<'value>(value: &'value Value, field: &str) -> Option<&'value str> {
    let link_field = match field {
        "author" => Some("author_link"),
        "owner" => Some("owner_link"),
        "person" => Some("person_link"),
        "reviewer" => Some("reviewer_link"),
        "registrant" => Some("registrant_link"),
        _ => None,
    };
    value
        .get(field)
        .and_then(|person| render::text_field(person, "self_link"))
        .or_else(|| link_field.and_then(|link_field| render::text_field(value, link_field)))
        .or_else(|| value.get(field).and_then(Value::as_str))
}

fn matches_discussion_filters(
    value: &Value,
    identity_field: &str,
    since: Option<&DateTime<FixedOffset>>,
    reviewer: Option<&str>,
) -> bool {
    let after_since = since.is_none_or(|since| {
        render::text_field(value, "created_at")
            .and_then(|created| DateTime::parse_from_rfc3339(created).ok())
            .is_none_or(|created| created >= *since)
    });
    let matches_reviewer = reviewer.is_none_or(|reviewer| {
        let reviewer = reviewer.trim_start_matches('~');
        value.get(identity_field).is_some_and(|identity| {
            ["username", "display_name"]
                .into_iter()
                .filter_map(|field| render::text_field(identity, field))
                .any(|candidate| candidate.eq_ignore_ascii_case(reviewer))
        })
    });
    after_since && matches_reviewer
}

fn thread_state(current: bool, stale: bool) -> &'static str {
    if stale {
        "outdated"
    } else if current {
        "open"
    } else {
        "superseded"
    }
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
    let source_repository = normalise_repository(request.repository()?)?;
    let target_repository = request
        .target_repository
        .as_deref()
        .map(normalise_repository)
        .transpose()?
        .unwrap_or_else(|| source_repository.clone());
    let source_ref = request.string(&request.source_ref, "source_ref")?;
    let target_ref = request.string(&request.target_ref, "target_ref")?;
    let source = find_git_ref(client, "source", &source_repository, source_ref).await?;
    let target = find_git_ref(client, "target", &target_repository, target_ref).await?;
    let source_link = source.self_link.as_deref().ok_or_else(|| {
        Error::invalid(format!(
            "source ref {} in repository {source_repository} has no API link",
            normalise_ref(source_ref)
        ))
    })?;
    let target_link = target.self_link.as_deref().ok_or_else(|| {
        Error::invalid(format!(
            "target ref {} in repository {target_repository} has no API link",
            normalise_ref(target_ref)
        ))
    })?;
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
        .await
        .map_err(|source| Error::context(format!(
            "cannot create merge proposal from source {source_repository}:{} to target {target_repository}:{}",
            normalise_ref(source_ref), normalise_ref(target_ref)
        ), source.into()))?;
    let proposal: Value = client.get_url(&location).await.map_err(|source| Error::context(
        format!(
            "cannot load merge proposal created from source {source_repository}:{} to target {target_repository}:{}",
            normalise_ref(source_ref), normalise_ref(target_ref)
        ),
        source.into(),
    ))?;
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
    let (kind, source_url) = match &target.kind {
        ResourceKind::Bug { id } => {
            let url = client.url(&format!("/bugs/{id}"));
            let mut parameters = vec![("ws.op", "newMessage"), ("content", body)];
            if let Some(subject) = request.subject.as_deref() {
                parameters.push(("subject", subject));
            }
            client.post_pairs_url_ok(&url, &parameters).await?;
            ("bug", launchpad_web_url(&target.path, "bug"))
        }
        ResourceKind::MergeProposal { .. } | ResourceKind::MergeProposalId { .. } => {
            let (_, proposal) = resolve_merge_proposal_target(client, request.target()?).await?;
            let url = proposal_api_url(&proposal)?;
            let mut parameters = vec![("ws.op", "createComment"), ("content", body)];
            if let Some(subject) = request.subject.as_deref() {
                parameters.push(("subject", subject));
            }
            if let Some(vote) = request.vote.as_deref() {
                parameters.push(("vote", vote));
            }
            client.post_pairs_url_ok(url.as_str(), &parameters).await?;
            (
                "branch_merge_proposal",
                render::text_field(&proposal, "web_link")
                    .unwrap_or(request.target()?)
                    .to_owned(),
            )
        }
        ResourceKind::Repository | ResourceKind::Generic => {
            return Err(Error::invalid(
                "comments are supported only for bugs and merge proposals",
            ));
        }
    };
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
    let (_, proposal) = request_merge_proposal(client, request).await?;
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

    let source_url = render::text_field(&proposal, "web_link")
        .unwrap_or(request.target()?)
        .to_owned();
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
    let (_, proposal) = request_merge_proposal(client, request).await?;
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

    let source_url = render::text_field(&proposal, "web_link")
        .unwrap_or(request.target()?)
        .to_owned();
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
    let (_, proposal) = request_merge_proposal(client, request).await?;
    let status = request.status()?;
    let url = proposal_api_url(&proposal)?;
    client
        .post_pairs_url_ok(url.as_str(), &[("ws.op", "setStatus"), ("status", status)])
        .await?;
    let source_url = render::text_field(&proposal, "web_link")
        .unwrap_or(request.target()?)
        .to_owned();
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
    let (_, proposal) = request_merge_proposal(client, request).await?;
    let spec = checkout_spec(client, &proposal).await?;
    local_git::checkout(spec, request).await
}

async fn read_repository_file(request: &Request) -> Result<OperationResult> {
    let repository = normalise_repository(request.repository()?)?;
    let repository = repository.trim_matches('/');
    let path = request.path("path")?.trim();
    let branch = request.branch.as_deref().unwrap_or("(default)");
    let context =
        format!("cannot read Launchpad repository file {repository}:{path} at branch {branch}");
    let encoded_repository = encode_path(repository);
    let encoded_path = encode_path(path);
    let mut url = Url::parse(&format!(
        "https://git.launchpad.net/{encoded_repository}/plain/{encoded_path}"
    ))
    .map_err(|source| Error::Url {
        url: format!("https://git.launchpad.net/{encoded_repository}/plain/{encoded_path}"),
        source,
    })?;
    if let Some(branch) = request.branch.as_deref() {
        url.query_pairs_mut().append_pair("h", branch);
    }
    let text = fetch_git_plain_file(&url, &context).await?;
    let details = json!({
        "kind": "file",
        "repository": request.repository()?,
        "path": request.path("path")?,
        "branch": request.branch,
        "bytes": text.len(),
    });
    Ok(OperationResult::new(text)
        .with_source_url(Some(url.to_string()))
        .with_details(details))
}

async fn fetch_git_plain_file(url: &Url, context: &str) -> Result<String> {
    const FILE_TRANSPORT_GUIDANCE: &str = "git.launchpad.net/plain uses anonymous Git HTTP, not Launchpad API login; check repository, path and ref or use an authenticated Git checkout";

    let response = reqwest::Client::new()
        .get(url.clone())
        .send()
        .await
        .map_err(|source| {
            Error::context(
                context,
                Error::Web {
                    url: url.to_string(),
                    source,
                },
            )
        })?;
    if response.url().scheme() != url.scheme()
        || response.url().host_str() != url.host_str()
        || !response.url().path().contains("/plain/")
    {
        let host = response.url().host_str().unwrap_or("unknown host");
        return Err(Error::context(
            context,
            Error::GitFileResponse {
                reason: format!(
                    "redirected to {host} outside a Git plain-file URL; {FILE_TRANSPORT_GUIDANCE}"
                ),
            },
        ));
    }
    let status = response.status();
    if !status.is_success() {
        let guidance = if status == reqwest::StatusCode::UNAUTHORIZED
            || status == reqwest::StatusCode::FORBIDDEN
        {
            format!("; {FILE_TRANSPORT_GUIDANCE}")
        } else {
            String::new()
        };
        return Err(Error::context(
            context,
            Error::GitFileResponse {
                reason: format!("git.launchpad.net returned HTTP {status}{guidance}"),
            },
        ));
    }
    let html_response = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|content_type| {
            content_type.trim().eq_ignore_ascii_case("text/html")
                || content_type
                    .trim()
                    .eq_ignore_ascii_case("application/xhtml+xml")
        });
    if html_response {
        return Err(Error::context(
            context,
            Error::GitFileResponse {
                reason: format!(
                    "git.launchpad.net returned an HTML page instead of file content; {FILE_TRANSPORT_GUIDANCE}"
                ),
            },
        ));
    }
    let bytes = response.bytes().await.map_err(|source| {
        Error::context(
            context,
            Error::Web {
                url: url.to_string(),
                source,
            },
        )
    })?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(Error::context(
            context,
            Error::GitFileResponse {
                reason: format!("Launchpad file is larger than {MAX_FILE_BYTES} bytes"),
            },
        ));
    }
    let text = String::from_utf8(bytes.to_vec()).map_err(|_| {
        Error::context(
            context,
            Error::GitFileResponse {
                reason: "file response is not UTF-8 text".to_owned(),
            },
        )
    })?;
    if text.trim() == "Invalid OpenID transaction" {
        return Err(Error::context(
            context,
            Error::GitFileResponse {
                reason: format!(
                    "git.launchpad.net returned an OpenID error instead of file content; {FILE_TRANSPORT_GUIDANCE}"
                ),
            },
        ));
    }
    Ok(text)
}

async fn request_merge_proposal(
    client: &LaunchpadClient,
    request: &Request,
) -> Result<(ResourceTarget, Value)> {
    resolve_merge_proposal_target(client, request.target()?).await
}

async fn resolve_merge_proposal_target(
    client: &LaunchpadClient,
    raw_target: &str,
) -> Result<(ResourceTarget, Value)> {
    let target = ResourceTarget::parse(raw_target)?;
    let proposal = match &target.kind {
        ResourceKind::MergeProposal { repository, id } => {
            get_merge_proposal(client, repository, *id).await?
        }
        ResourceKind::MergeProposalId { id } => resolve_merge_proposal_id(client, *id).await?,
        ResourceKind::Bug { .. } | ResourceKind::Repository | ResourceKind::Generic => {
            return Err(Error::invalid(
                "operation requires a merge proposal target; accepted syntax: numeric ID or lp://~owner/project/+git/repository/+merge/ID",
            ));
        }
    };
    Ok((target, proposal))
}

async fn resolve_merge_proposal_id(client: &LaunchpadClient, id: u64) -> Result<Value> {
    let current = local_git::current_repository().await.map_err(|error| {
        Error::invalid(format!(
            "cannot resolve merge proposal ID {id} without a Launchpad checkout: {error}; accepted syntax: numeric ID in a checkout with a Launchpad remote, or lp://~owner/project/+git/repository/+merge/{id}"
        ))
    })?;
    let mut repository_paths = Vec::new();
    for remote in &current.remotes {
        if let Ok(path) = normalise_repository(&remote.url)
            && !repository_paths.contains(&path)
        {
            repository_paths.push(path);
        }
    }
    let mut inspected_repositories = Vec::new();
    for path in &repository_paths {
        let Ok(repository) = get_repository(client, path).await else {
            continue;
        };
        inspected_repositories.push(
            render::text_field(&repository, "unique_name")
                .unwrap_or(path)
                .to_owned(),
        );
        for field in [
            "landing_targets_collection_link",
            "landing_candidates_collection_link",
        ] {
            let Some(collection_link) = render::text_field(&repository, field) else {
                continue;
            };
            if let Some(proposal) = find_proposal_by_id(client, collection_link, id).await? {
                return Ok(proposal);
            }
        }
    }
    let remotes = current
        .remotes
        .iter()
        .map(|remote| format!("{}={}", remote.name, remote.url))
        .collect::<Vec<_>>()
        .join(", ");
    let repositories = if inspected_repositories.is_empty() {
        "none".to_owned()
    } else {
        inspected_repositories.join(", ")
    };
    Err(Error::invalid(format!(
        "cannot resolve merge proposal ID {id}; working directory: {}; inspected remotes: {}; inspected Launchpad repositories: {repositories}; accepted syntax: numeric ID in a related Launchpad checkout or lp://~owner/project/+git/repository/+merge/{id}; retry with the full merge proposal target",
        current.working_directory.display(),
        if remotes.is_empty() { "none" } else { &remotes }
    )))
}

async fn find_proposal_by_id(
    client: &LaunchpadClient,
    first_url: &str,
    id: u64,
) -> Result<Option<Value>> {
    let mut seen = HashSet::new();
    let mut url = first_url.to_owned();
    loop {
        if !seen.insert(url.clone()) {
            return Err(Error::invalid(
                "Launchpad proposal pagination repeated a page",
            ));
        }
        let page: Collection<Value> = client.get_url(&url).await?;
        if let Some(proposal) = page.entries.into_iter().find(|proposal| {
            render::resource_id(proposal).and_then(|value| value.parse().ok()) == Some(id)
        }) {
            return Ok(Some(proposal));
        }
        let Some(next_url) = page.next_collection_link else {
            return Ok(None);
        };
        url = next_url;
    }
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
    let path = normalise_repository(path)?;
    let url = client.url(&format!("/+git?ws.op=getByPath&path={}", urlenc(&path)));
    let repository = client.get_url(&url).await?;
    Ok(repository)
}

async fn get_merge_proposal(client: &LaunchpadClient, repository: &str, id: u64) -> Result<Value> {
    let proposal = client.get(&format!("/{repository}/+merge/{id}")).await?;
    Ok(proposal)
}

async fn find_git_ref(
    client: &LaunchpadClient,
    role: &str,
    repository: &str,
    requested_path: &str,
) -> Result<GitRef> {
    let requested_path = normalise_ref(requested_path);
    let context = format!("cannot resolve {role} ref {requested_path} in repository {repository}");
    let resource = get_repository(client, repository).await.map_err(|source| {
        Error::context(
            format!("cannot resolve {role} repository {repository} for ref {requested_path}"),
            source,
        )
    })?;
    let canonical_repository = render::text_field(&resource, "unique_name")
        .filter(|name| !name.trim().is_empty())
        .ok_or_else(|| {
            Error::invalid(format!(
                "{context}; Launchpad repository has no unique name"
            ))
        })?;
    let git_refs = list_git_refs(client, canonical_repository)
        .await
        .map_err(|source| {
            Error::context(
                format!("{context} (canonical repository {canonical_repository})"),
                source.into(),
            )
        })?;
    git_refs
        .into_iter()
        .find(|git_ref| git_ref.path.as_deref() == Some(requested_path.as_str()))
        .ok_or_else(|| {
            Error::invalid(format!(
                "{context} (canonical repository {canonical_repository}); ref was not found"
            ))
        })
}

async fn proposal_details(client: &LaunchpadClient, proposal: &Value) -> Result<Value> {
    let source_repository = linked_resource(client, proposal, "source_git_repository_link").await?;
    let target_repository = linked_resource(client, proposal, "target_git_repository_link").await?;
    Ok(json!({
        "kind": "branch_merge_proposal",
        "id": render::resource_id(proposal),
        "url": render::scalar_field(proposal, "web_link"),
        "status": render::scalar_field(proposal, "queue_status"),
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
        .map(|segment| utf8_percent_encode(segment, GIT_PATH_SEGMENT_ENCODE_SET).to_string())
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
    use std::collections::HashMap;

    use chrono::DateTime;
    use lpcli::client::LaunchpadClient;
    use serde_json::{Value, json};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use url::Url;

    use super::{
        ProposalLookup, ProposalLookupContext, eligible_proposals, encode_path,
        fetch_git_plain_file, find_git_ref, format_discussion_result, matches_discussion_filters,
        normalise_inline_comment, proposal_lookup_result, related_repository_branch_proposals,
        repository_branch_proposals, select_preview_diff, select_proposal, thread_state,
        validate_current_preview_diff, view_merge_proposal_for_branch,
    };
    use crate::request::{DiscussionFormat, Request, ResourceTarget};

    async fn read_request_headers(stream: &mut TcpStream) -> String {
        let mut request = Vec::new();
        loop {
            let mut chunk = [0; 512];
            let size = stream.read(&mut chunk).await.unwrap();
            assert!(size > 0, "client closed before completing request headers");
            request.extend_from_slice(&chunk[..size]);
            if request.windows(4).any(|part| part == b"\r\n\r\n") {
                return String::from_utf8(request).unwrap();
            }
        }
    }

    #[tokio::test]
    async fn rejects_web_login_responses_without_rejecting_source_text() {
        let cases = [
            (
                "Text/Html; charset=utf-8",
                "<html>Invalid OpenID transaction</html>",
                Some("HTML"),
            ),
            ("text/plain", "Invalid OpenID transaction", Some("OpenID")),
            ("text/plain", "Example: Invalid OpenID transaction", None),
        ];
        for (content_type, body, expected_error) in cases {
            let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                read_request_headers(&mut stream).await;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            });
            let url = Url::parse(&format!("http://{address}/repo/plain/README?h=main")).unwrap();
            let result = fetch_git_plain_file(
                &url,
                "cannot read Launchpad repository file repo:README at branch main",
            )
            .await;
            server.await.unwrap();
            if let Some(expected_error) = expected_error {
                let error = result.unwrap_err().to_string();
                assert!(error.contains("repo:README at branch main"));
                assert!(error.contains(expected_error));
            } else {
                assert_eq!(result.unwrap(), body);
            }
        }
    }

    #[tokio::test]
    async fn redirected_plain_file_reports_anonymous_access_without_assuming_privacy() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_request_headers(&mut stream).await;
            let redirect = format!(
                "HTTP/1.1 302 Found\r\nLocation: http://{address}/login\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            stream.write_all(redirect.as_bytes()).await.unwrap();
            let (mut stream, _) = listener.accept().await.unwrap();
            read_request_headers(&mut stream).await;
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 5\r\nConnection: close\r\n\r\nlogin")
                .await
                .unwrap();
        });
        let url = Url::parse(&format!("http://{address}/repo/plain/package.json")).unwrap();
        let error = fetch_git_plain_file(&url, "cannot read repo:package.json")
            .await
            .unwrap_err()
            .to_string();
        server.await.unwrap();
        assert!(error.contains("redirected"));
        assert!(error.contains("anonymous Git HTTP"));
        assert!(!error.contains("private"));
    }

    #[test]
    fn git_plain_paths_preserve_repository_names_and_escape_unsafe_bytes() {
        let public = Url::parse(&format!(
            "https://git.launchpad.net/{}/plain/{}",
            encode_path("launchpad-ui"),
            encode_path("package.json")
        ))
        .unwrap();
        assert_eq!(public.path(), "/launchpad-ui/plain/package.json");
        let canonical = Url::parse(&format!(
            "https://git.launchpad.net/{}/plain/src/main.rs",
            encode_path("~owner/my-project/+git/my.repo")
        ))
        .unwrap();
        assert_eq!(
            canonical.path(),
            "/~owner/my-project/+git/my.repo/plain/src/main.rs"
        );
        assert_eq!(encode_path("a b/c?#.txt"), "a%20b/c%3F%23.txt");
    }

    #[tokio::test]
    async fn missing_discussion_respects_structured_and_both_formats() {
        let client = LaunchpadClient::new(None);
        let request: Request = serde_json::from_str(
            r#"{"op":"merge_proposal_discussion","repository":"launchpad-ui","branch":"missing"}"#,
        )
        .unwrap();
        for format in [DiscussionFormat::Structured, DiscussionFormat::Both] {
            let lookup = proposal_lookup_result(
                &client,
                &request,
                ProposalLookup::Missing {
                    repository: "~owner/launchpad-ui/+git/launchpad-ui".to_owned(),
                    inspected_repositories: vec![
                        "~other/launchpad-ui/+git/launchpad-ui".to_owned(),
                    ],
                },
                ProposalLookupContext {
                    requested_repository: "launchpad-ui",
                    branch: "missing",
                    inferred: false,
                    kind: "merge_proposal_discussion",
                    metadata: None,
                },
            )
            .await
            .unwrap();
            let result = format_discussion_result(lookup, format);
            let details = Value::Object(result.details);
            assert_eq!(details["found"], false);
            assert_eq!(
                details["repository"],
                "~owner/launchpad-ui/+git/launchpad-ui"
            );
            assert_eq!(details["requested_repository"], "launchpad-ui");
            assert_eq!(
                details["inspected_repositories"],
                json!(["~other/launchpad-ui/+git/launchpad-ui"])
            );
            if format == DiscussionFormat::Structured {
                assert_eq!(
                    serde_json::from_str::<Value>(&result.text).unwrap(),
                    details
                );
            } else {
                assert!(result.text.starts_with("# No merge proposal found"));
                let (_, json_block) = result.text.split_once("```json\n").unwrap();
                let (json_block, _) = json_block.split_once("\n```").unwrap();
                assert_eq!(serde_json::from_str::<Value>(json_block).unwrap(), details);
            }
        }
    }

    #[tokio::test]
    async fn absent_branch_lookup_reports_canonical_and_inspected_repositories() {
        let canonical = "~owner/launchpad-ui/+git/launchpad-ui";
        let related = "~other/launchpad-ui/+git/launchpad-ui";
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let refs_path = format!("/devel/{canonical}/refs");
        let related_ref_path = format!("/devel/{related}?ws.op=getRefByPath&");
        let server = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let request = read_request_headers(&mut stream).await;
                let path = request.split_ascii_whitespace().nth(1).unwrap_or_default();
                let body = if path == "/devel/+git?ws.op=getByPath&path=launchpad-ui" {
                    json!({
                        "unique_name": canonical,
                        "name": "launchpad-ui",
                        "target_link": format!("http://{address}/devel/launchpad-ui"),
                        "landing_targets_collection_link": format!("http://{address}/landing_targets"),
                    })
                } else if path == refs_path || path == "/landing_targets" {
                    json!({ "entries": [], "next_collection_link": null })
                } else if path.starts_with("/devel/+git?ws.op=getRepositories&") {
                    json!({
                        "entries": [{
                            "unique_name": related,
                            "name": "launchpad-ui",
                            "self_link": format!("http://{address}/devel/{related}"),
                        }],
                        "next_collection_link": null,
                    })
                } else if path.starts_with(&related_ref_path) {
                    Value::Null
                } else {
                    panic!("unexpected request: {path}");
                }
                .to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let client = LaunchpadClient::new(None).with_base_url(format!("http://{address}/devel"));
        let request: Request = serde_json::from_str(
            r#"{"op":"merge_proposal_for_branch","repository":"lp:launchpad-ui","branch":"missing"}"#,
        )
        .unwrap();
        let result = view_merge_proposal_for_branch(&client, &request).await;
        server.abort();
        let details = result.unwrap().details;
        assert_eq!(details["found"], false);
        assert_eq!(details["repository"], canonical);
        assert_eq!(details["requested_repository"], "lp:launchpad-ui");
        assert_eq!(details["inspected_repositories"], json!([related]));
    }

    #[tokio::test]
    async fn fallback_only_caps_when_more_than_250_proposals_exist() {
        let canonical = "~owner/project/+git/repo";
        for count in [250, 251] {
            let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                loop {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    let request = read_request_headers(&mut stream).await;
                    let path = request.split_ascii_whitespace().nth(1).unwrap_or_default();
                    let body = if path == format!("/devel/{canonical}/refs") {
                        json!({ "entries": [], "next_collection_link": null }).to_string()
                    } else if path == "/landing_targets" {
                        let entries = (0..count)
                            .map(|index| {
                                json!({
                                    "source_git_path": if index == 250 {
                                        "refs/heads/feature"
                                    } else {
                                        "refs/heads/other"
                                    },
                                })
                            })
                            .collect::<Vec<_>>();
                        json!({ "entries": entries, "next_collection_link": null }).to_string()
                    } else {
                        panic!("unexpected request: {path}");
                    };
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    stream.write_all(response.as_bytes()).await.unwrap();
                }
            });
            let client =
                LaunchpadClient::new(None).with_base_url(format!("http://{address}/devel"));
            let repository = json!({
                "unique_name": canonical,
                "landing_targets_collection_link": format!("http://{address}/landing_targets"),
            });
            let lookup =
                repository_branch_proposals(&client, &repository, "refs/heads/feature").await;
            server.abort();
            let (matches, capped) = lookup.unwrap();
            assert!(matches.is_empty());
            assert_eq!(capped, count > 250);
        }
    }

    #[tokio::test]
    async fn resolves_git_ssh_alias_refs_through_canonical_repository() {
        let canonical = "~launchpad-committers/mobot/+git/mobot";
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let refs_path = format!("/devel/{canonical}/refs");
        let server = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let request = read_request_headers(&mut stream).await;
                let path = request.split_ascii_whitespace().nth(1).unwrap_or_default();
                let (status, body) = if path == "/devel/+git?ws.op=getByPath&path=mobot" {
                    ("200 OK", json!({ "unique_name": canonical }).to_string())
                } else if path == refs_path {
                    (
                        "200 OK",
                        json!({
                            "entries": [{
                                "path": "refs/heads/feature",
                                "self_link": format!("http://{address}/devel/{canonical}/+ref/feature"),
                            }],
                            "next_collection_link": null,
                        }).to_string(),
                    )
                } else {
                    ("404 Not Found", "{}".to_owned())
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let client = LaunchpadClient::new(None).with_base_url(format!("http://{address}/devel"));
        let result = find_git_ref(
            &client,
            "source",
            "git+ssh://goulinkh@git.launchpad.net/mobot",
            "feature",
        )
        .await;
        server.abort();
        let git_ref = result.unwrap();
        assert_eq!(git_ref.path.as_deref(), Some("refs/heads/feature"));
        assert_eq!(
            git_ref.self_link.as_deref(),
            Some(format!("http://{address}/devel/{canonical}/+ref/feature").as_str())
        );
    }

    #[tokio::test]
    async fn missing_related_git_ref_returns_no_proposals() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_request_headers(&mut stream).await;
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 4\r\nConnection: close\r\n\r\nnull")
                .await
                .unwrap();
        });
        let client = LaunchpadClient::new(None).with_base_url(format!("http://{address}/devel"));
        let repository = json!({
            "self_link": format!("http://{address}/devel/~owner/project/+git/repo"),
        });
        let result =
            related_repository_branch_proposals(&client, &repository, "refs/heads/never-created")
                .await;
        server.await.unwrap();
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn selects_preview_diff_for_numeric_merge_proposal() {
        let mut target = ResourceTarget::parse("510799").unwrap();
        select_preview_diff(&mut target, Some(1_142_878)).unwrap();
        assert!(target.diff);
        assert_eq!(target.preview_diff_id, Some(1_142_878));
    }

    #[test]
    fn selects_preview_diff_for_merge_proposal_url() {
        let mut target =
            ResourceTarget::parse("lp://~owner/project/+git/repository/+merge/510799").unwrap();
        select_preview_diff(&mut target, Some(1_142_878)).unwrap();
        assert!(target.diff);
        assert_eq!(target.preview_diff_id, Some(1_142_878));
    }

    #[test]
    fn accepts_matching_resource_preview_diff() {
        let mut target =
            ResourceTarget::parse("lp://~owner/project/+git/repository/+merge/42/diff/17").unwrap();
        select_preview_diff(&mut target, Some(17)).unwrap();
        assert!(target.diff);
        assert_eq!(target.preview_diff_id, Some(17));
    }

    #[test]
    fn rejects_conflicting_resource_preview_diff() {
        let mut target =
            ResourceTarget::parse("lp://~owner/project/+git/repository/+merge/42/diff/17").unwrap();
        let error = select_preview_diff(&mut target, Some(18)).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("target and preview_diff_id select different snapshots")
        );
    }

    #[test]
    fn rejects_preview_diff_for_non_merge_proposal() {
        let mut target = ResourceTarget::parse("lp://bugs/1").unwrap();
        let error = select_preview_diff(&mut target, Some(17)).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("preview_diff_id requires a merge proposal target")
        );
    }

    #[test]
    fn prefers_active_proposal_unless_latest_is_requested() {
        let proposals = vec![
            json!({
                "self_link": "https://api.launchpad.net/devel/~owner/project/+git/repository/+merge/10",
                "queue_status": "Needs review",
                "date_created": "2026-01-01T00:00:00Z",
                "target_git_path": "refs/heads/main",
            }),
            json!({
                "self_link": "https://api.launchpad.net/devel/~owner/project/+git/repository/+merge/20",
                "queue_status": "Merged",
                "date_created": "2026-02-01T00:00:00Z",
                "target_git_path": "refs/heads/main",
            }),
            json!({
                "self_link": "https://api.launchpad.net/devel/~owner/project/+git/repository/+merge/30",
                "queue_status": "Superseded",
                "date_created": "2026-03-01T00:00:00Z",
                "target_git_path": "refs/heads/main",
            }),
        ];
        let preferred: Request = serde_json::from_str(
            r#"{"op":"merge_proposal_for_branch","repository":"project","branch":"feature"}"#,
        )
        .unwrap();
        let eligible = eligible_proposals(proposals.clone(), &preferred);
        assert_eq!(eligible.len(), 2);
        let (selected, _, _) = select_proposal(eligible, &preferred).unwrap();
        assert_eq!(selected["queue_status"], "Needs review");

        let latest: Request = serde_json::from_str(
            r#"{"op":"merge_proposal_for_branch","repository":"project","branch":"feature","latest":true}"#,
        )
        .unwrap();
        let eligible = eligible_proposals(proposals, &latest);
        let (selected, _, _) = select_proposal(eligible, &latest).unwrap();
        assert_eq!(selected["queue_status"], "Merged");
    }

    #[test]
    fn classifies_thread_state_from_preview_diff() {
        assert_eq!(thread_state(true, false), "open");
        assert_eq!(thread_state(false, false), "superseded");
        assert_eq!(thread_state(false, true), "outdated");
    }

    #[test]
    fn filters_comments_by_timestamp_and_normalised_identity() {
        let comment = normalise_inline_comment(
            &json!({
                "person": {
                    "name": "alice",
                    "display_name": "Alice Example",
                    "web_link": "https://launchpad.net/~alice"
                },
                "date": "2026-09-21T10:00:00+00:00",
                "text": "Review comment"
            }),
            0,
            &HashMap::new(),
        );
        let since = DateTime::parse_from_rfc3339("2026-09-21T09:59:00Z").unwrap();
        assert!(matches_discussion_filters(
            &comment,
            "author",
            Some(&since),
            Some("alice"),
        ));
        assert!(matches_discussion_filters(
            &comment,
            "author",
            Some(&since),
            Some("Alice Example"),
        ));
        assert!(!matches_discussion_filters(
            &comment,
            "author",
            Some(&since),
            Some("bob"),
        ));
        let malformed = json!({
            "author": {
                "username": "alice",
                "display_name": "Alice Example"
            },
            "created_at": "not-a-timestamp"
        });
        assert!(matches_discussion_filters(
            &malformed,
            "author",
            Some(&since),
            None,
        ));
    }

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
