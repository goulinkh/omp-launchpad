"""JSON bridge executed inside lp-shell, where `lp` is already authenticated."""

from datetime import date, datetime
from itertools import islice
import json
import os
from urllib.parse import parse_qs, unquote, urlsplit


MAX_ITEMS = 50
MAX_COMMENTS = 20
MAX_DIFF_BYTES = 2 * 1024 * 1024


def value(obj, name, default=None):
    try:
        return getattr(obj, name)
    except Exception:
        return default


def scalar(item):
    if item is None or isinstance(item, (str, int, float, bool)):
        return item
    if isinstance(item, (datetime, date)):
        return item.isoformat()
    if isinstance(item, bytes):
        return item.decode("utf-8", errors="replace")
    if isinstance(item, (list, tuple)):
        return [scalar(entry) for entry in item]
    if isinstance(item, dict):
        return {str(key): scalar(entry) for key, entry in item.items()}
    return value(item, "web_link", None) or value(item, "self_link", None) or str(item)


def entries(collection, limit=MAX_ITEMS):
    return list(islice(collection, max(0, min(int(limit), MAX_ITEMS))))


def person_name(person):
    if person is None:
        return None
    return value(person, "display_name", None) or value(person, "name", None) or scalar(person)


def compact(text):
    return " ".join(str(text or "").split())


def bullet(label, val):
    if val is None or val == "" or val == []:
        return None
    if isinstance(val, list):
        val = ", ".join(str(item) for item in val)
    return f"- **{label}:** {val}"


def section(title, body):
    body = str(body or "").strip()
    return f"## {title}\n\n{body}" if body else None


def resource_type(resource):
    link = value(resource, "resource_type_link", "")
    return link.rsplit("#", 1)[-1] if "#" in link else "resource"


def normalize_target(raw):
    target = raw.strip()
    query = {}
    if target.startswith("lp://"):
        parsed = urlsplit(target)
        target = unquote((parsed.netloc + parsed.path).lstrip("/"))
        query = parse_qs(parsed.query)
    elif target.startswith(("https://launchpad.net/", "https://bugs.launchpad.net/", "https://code.launchpad.net/")):
        parsed = urlsplit(target)
        target = unquote(parsed.path.lstrip("/"))
        query = parse_qs(parsed.query)
    if target.startswith("bug/"):
        target = "bugs/" + target[4:]
    if target.startswith("+bug/"):
        target = "bugs/" + target[5:]
    diff = False
    for suffix in ("/diff/all", "/diff"):
        if target.endswith(suffix):
            target = target[: -len(suffix)]
            diff = True
            break
    comments = query.get("comments", ["1"])[0] != "0"
    limit_text = query.get("limit", [str(MAX_COMMENTS)])[0]
    try:
        limit = max(1, min(int(limit_text), MAX_COMMENTS))
    except ValueError:
        raise ValueError("limit must be an integer")
    return target, comments, limit, diff


def load_resource(lp, target):
    if target.startswith("bugs/") and target[5:].isdigit():
        return lp.bugs[int(target[5:])]
    return lp.load(target)


def render_comment(comment):
    author = person_name(value(comment, "author", None) or value(comment, "owner", None)) or "unknown"
    created = scalar(value(comment, "date_created", None))
    vote = value(comment, "vote", None)
    title = value(comment, "title", None) or value(comment, "subject", None)
    body = value(comment, "message_body", None) or value(comment, "content", None) or ""
    metadata = " · ".join(str(item) for item in (created, vote) if item)
    heading = f"### {author}" + (f" ({metadata})" if metadata else "")
    parts = [heading]
    if title and compact(title) != compact(body):
        parts.append(f"**{title}**")
    if body:
        parts.append(str(body).strip())
    return "\n\n".join(parts)


def render_bug(bug, include_comments, limit):
    tasks = entries(bug.bug_tasks)
    lines = [f"# Bug #{value(bug, 'id', '?')}: {value(bug, 'title', 'Untitled')}"]
    metadata = [
        bullet("Status", ", ".join(dict.fromkeys(str(value(task, "status", "Unknown")) for task in tasks))),
        bullet("Importance", ", ".join(dict.fromkeys(str(value(task, "importance", "Unknown")) for task in tasks))),
        bullet("Owner", person_name(value(bug, "owner", None))),
        bullet("Created", scalar(value(bug, "date_created", None))),
        bullet("Updated", scalar(value(bug, "date_last_updated", None))),
        bullet("Information type", value(bug, "information_type", None)),
        bullet("Tags", value(bug, "tags", None)),
        bullet("Heat", value(bug, "heat", None)),
        bullet("URL", value(bug, "web_link", None)),
    ]
    lines.extend(item for item in metadata if item)
    description = section("Description", value(bug, "description", None))
    if description:
        lines.append(description)

    task_lines = []
    for task in tasks:
        target = value(task, "bug_target_display_name", None) or value(task, "bug_target_name", "unknown")
        status = value(task, "status", "Unknown")
        importance = value(task, "importance", "Unknown")
        assignee = person_name(value(task, "assignee", None))
        suffix = f" · assignee: {assignee}" if assignee else ""
        task_lines.append(f"- **{target}:** {status} · {importance}{suffix}")
    if task_lines:
        lines.append(section("Tasks", "\n".join(task_lines)))

    if include_comments:
        comments = [render_comment(comment) for comment in entries(bug.messages, limit)]
        if comments:
            lines.append(section(f"Comments (up to {limit})", "\n\n".join(comments)))
    return "\n\n".join(lines), value(bug, "web_link", None)


def render_proposal(proposal, include_comments, limit):
    proposal_id = value(proposal, "self_link", "").rstrip("/").rsplit("/", 1)[-1]
    title = value(proposal, "commit_message", None) or compact(value(proposal, "description", ""))[:120] or "Untitled"
    lines = [f"# Merge proposal {proposal_id}: {title}"]
    metadata = [
        bullet("Status", value(proposal, "queue_status", None)),
        bullet("Source", value(proposal, "source_git_path", None) or value(value(proposal, "source_branch", None), "unique_name", None)),
        bullet("Target", value(proposal, "target_git_path", None) or value(value(proposal, "target_branch", None), "unique_name", None)),
        bullet("Registrant", person_name(value(proposal, "registrant", None))),
        bullet("Reviewer", person_name(value(proposal, "reviewer", None))),
        bullet("Created", scalar(value(proposal, "date_created", None))),
        bullet("Reviewed", scalar(value(proposal, "date_reviewed", None))),
        bullet("Merged", scalar(value(proposal, "date_merged", None))),
        bullet("URL", value(proposal, "web_link", None)),
    ]
    lines.extend(item for item in metadata if item)
    description = section("Description", value(proposal, "description", None))
    if description:
        lines.append(description)

    votes = []
    for vote in entries(proposal.votes):
        reviewer = person_name(value(vote, "reviewer", None)) or person_name(value(vote, "registrant", None)) or "unknown"
        verdict = value(vote, "vote", None) or value(vote, "review_type", None) or "Pending"
        votes.append(f"- **{reviewer}:** {verdict}")
    if votes:
        lines.append(section("Reviews", "\n".join(votes)))

    if include_comments:
        comments = [render_comment(comment) for comment in entries(proposal.all_comments, limit)]
        if comments:
            lines.append(section(f"Comments (up to {limit})", "\n\n".join(comments)))
    return "\n\n".join(lines), value(proposal, "web_link", None)


def render_repository(repository):
    name = value(repository, "unique_name", None) or value(repository, "name", "repository")
    lines = [f"# {name}"]
    for label, attr in (
        ("Owner", "owner"),
        ("Target", "target"),
        ("Default branch", "default_branch"),
        ("Status", "status"),
        ("Information type", "information_type"),
        ("Created", "date_created"),
        ("Updated", "date_last_modified"),
        ("Clone URL", "git_https_url"),
        ("SSH URL", "git_ssh_url"),
        ("URL", "web_link"),
    ):
        raw = value(repository, attr, None)
        if attr in ("owner", "target"):
            raw = person_name(raw) or value(raw, "display_name", None) or scalar(raw)
        item = bullet(label, scalar(raw))
        if item:
            lines.append(item)
    description = section("Description", value(repository, "description", None))
    if description:
        lines.append(description)
    return "\n\n".join(lines), value(repository, "web_link", None)


def render_generic(resource):
    kind = resource_type(resource)
    title = value(resource, "title", None) or value(resource, "display_name", None) or value(resource, "name", None) or kind
    lines = [f"# {title}", bullet("Type", kind)]
    skip = {"self_link", "resource_type_link", "http_etag", "web_link", "description", "title", "name", "display_name"}
    for attr in value(resource, "lp_attributes", []):
        if attr in skip:
            continue
        item = bullet(attr.replace("_", " ").title(), scalar(value(resource, attr, None)))
        if item:
            lines.append(item)
    web_link = value(resource, "web_link", None)
    if web_link:
        lines.append(bullet("URL", web_link))
    description = section("Description", value(resource, "description", None) or value(resource, "summary", None))
    if description:
        lines.append(description)
    return "\n\n".join(item for item in lines if item), web_link

def proposal_details(proposal):
    source_repository = value(proposal, "source_git_repository", None)
    target_repository = value(proposal, "target_git_repository", None)
    proposal_id = value(proposal, "self_link", "").rstrip("/").rsplit("/", 1)[-1]
    return {
        "id": proposal_id,
        "source_ref": value(proposal, "source_git_path", None),
        "target_ref": value(proposal, "target_git_path", None),
        "source_repository": value(source_repository, "unique_name", None),
        "target_repository": value(target_repository, "unique_name", None),
        "source_https_url": value(source_repository, "git_https_url", None),
        "source_ssh_url": value(source_repository, "git_ssh_url", None),
        "target_https_url": value(target_repository, "git_https_url", None),
        "target_ssh_url": value(target_repository, "git_ssh_url", None),
    }



def view_resource(lp, request):
    target, include_comments, limit, diff = normalize_target(request["target"])
    resource = load_resource(lp, target)
    kind = resource_type(resource)
    if diff:
        if kind != "branch_merge_proposal":
            raise ValueError("/diff is only valid for a merge proposal")
        preview = value(resource, "preview_diff", None)
        if preview is None:
            raise ValueError("This merge proposal has no preview diff")
        payload = preview.diff_text.open().read()
        payload = scalar(payload)
        encoded = payload.encode("utf-8")
        truncated = len(encoded) > MAX_DIFF_BYTES
        if truncated:
            payload = encoded[:MAX_DIFF_BYTES].decode("utf-8", errors="ignore") + "\n\n[Diff truncated at 2 MiB]"
        return {"text": payload, "source_url": value(resource, "web_link", None), "details": {"kind": kind, "diff": True, "truncated": truncated}}
    details = {"kind": kind, "target": target}
    if kind == "bug":
        text, source_url = render_bug(resource, include_comments, limit)
    elif kind == "branch_merge_proposal":
        text, source_url = render_proposal(resource, include_comments, limit)
        details.update(proposal_details(resource))
    elif kind == "git_repository":
        text, source_url = render_repository(resource)
    else:
        text, source_url = render_generic(resource)
    return {"text": text, "source_url": source_url, "details": details}


def resolve_target(lp, target):
    if "/" in target or target.startswith("~"):
        return lp.load(target)
    project = lp.projects[target]
    if project is not None:
        return project
    distribution = lp.distributions[target]
    if distribution is not None:
        return distribution
    raise ValueError(f"Launchpad target not found: {target}")


def search_bugs(lp, request):
    target = resolve_target(lp, request["target"])
    kwargs = {"search_text": request.get("query") or None, "omit_duplicates": True}
    if request.get("status"):
        kwargs["status"] = request["status"]
    if request.get("importance"):
        kwargs["importance"] = request["importance"]
    if request.get("tags"):
        kwargs["tags"] = request["tags"]
    found = entries(target.searchTasks(**kwargs), request.get("limit", 10))
    lines = [f"# Launchpad bug search", bullet("Target", request["target"]), bullet("Query", request.get("query")), bullet("Results", len(found))]
    for task in found:
        bug = task.bug
        lines.append(
            f"- [#{value(bug, 'id', '?')}: {value(bug, 'title', 'Untitled')}]({value(bug, 'web_link', '')})"
            f" — {value(task, 'status', 'Unknown')} · {value(task, 'importance', 'Unknown')}"
        )
    return {"text": "\n\n".join(item for item in lines if item), "details": {"count": len(found), "target": request["target"]}}


def search_proposals(lp, request):
    repository = lp.git_repositories.getByPath(path=request["repository"])
    if repository is None:
        raise ValueError(f"Launchpad repository not found: {request['repository']}")
    kwargs = {}
    if request.get("status"):
        kwargs["status"] = request["status"]
    found = entries(repository.getMergeProposals(**kwargs), request.get("limit", 10))
    lines = ["# Launchpad merge proposal search", bullet("Repository", value(repository, "unique_name", request["repository"])), bullet("Results", len(found))]
    for proposal in found:
        proposal_id = value(proposal, "self_link", "").rstrip("/").rsplit("/", 1)[-1]
        title = compact(value(proposal, "commit_message", None) or value(proposal, "description", "Untitled"))[:160]
        lines.append(f"- [MP {proposal_id}: {title}]({value(proposal, 'web_link', '')}) — {value(proposal, 'queue_status', 'Unknown')}")
    return {"text": "\n\n".join(item for item in lines if item), "details": {"count": len(found), "repository": value(repository, "unique_name", None)}}


def create_bug(lp, request):
    target = resolve_target(lp, request["target"])
    kwargs = {
        "target": target,
        "title": request["title"],
        "description": request["description"],
    }
    for key in ("information_type", "tags"):
        if request.get(key):
            kwargs[key] = request[key]
    bug = lp.bugs.createBug(**kwargs)
    text, source_url = render_bug(bug, False, 1)
    return {"text": "# Created Launchpad bug\n\n" + text, "source_url": source_url, "details": {"kind": "bug", "id": value(bug, "id", None)}}


def create_proposal(lp, request):
    repository = lp.git_repositories.getByPath(path=request["repository"])
    target_repository_path = request.get("target_repository") or request["repository"]
    target_repository = lp.git_repositories.getByPath(path=target_repository_path)
    if repository is None:
        raise ValueError(f"Launchpad repository not found: {request['repository']}")
    if target_repository is None:
        raise ValueError(f"Launchpad repository not found: {target_repository_path}")
    source = repository.getRefByPath(path=request["source_ref"])
    target = target_repository.getRefByPath(path=request["target_ref"])
    if source is None or target is None:
        raise ValueError("source_ref or target_ref was not found")
    kwargs = {
        "merge_target": target,
        "initial_comment": request.get("description") or "",
        "needs_review": request.get("needs_review", True),
    }
    if request.get("commit_message"):
        kwargs["commit_message"] = request["commit_message"]
    proposal = source.createMergeProposal(**kwargs)
    text, source_url = render_proposal(proposal, False, 1)
    return {"text": "# Created Launchpad merge proposal\n\n" + text, "source_url": source_url, "details": {"kind": "branch_merge_proposal"}}


def add_comment(lp, request):
    target, _comments, _limit, _diff = normalize_target(request["target"])
    resource = load_resource(lp, target)
    kind = resource_type(resource)
    if kind == "bug":
        result = resource.newMessage(content=request["body"], subject=request.get("subject") or None)
    elif kind == "branch_merge_proposal":
        kwargs = {"content": request["body"]}
        if request.get("subject"):
            kwargs["subject"] = request["subject"]
        if request.get("vote"):
            kwargs["vote"] = request["vote"]
        result = resource.createComment(**kwargs)
    else:
        raise ValueError("Comments are supported only for bugs and merge proposals")
    return {"text": f"Comment added to {value(resource, 'web_link', target)}", "source_url": value(resource, "web_link", None), "details": {"kind": kind, "comment": scalar(result)}}


def set_proposal_status(lp, request):
    target, _comments, _limit, _diff = normalize_target(request["target"])
    proposal = load_resource(lp, target)
    if resource_type(proposal) != "branch_merge_proposal":
        raise ValueError("set_merge_proposal_status requires a merge proposal target")
    proposal.setStatus(status=request["status"])
    return {"text": f"Merge proposal status set to {request['status']}: {value(proposal, 'web_link', target)}", "source_url": value(proposal, "web_link", None), "details": {"status": request["status"]}}


def dispatch(lp, request):
    operation = request.get("op")
    if operation == "resource_view":
        return view_resource(lp, request)
    if operation == "repo_view":
        repository = lp.git_repositories.getByPath(path=request["repository"])
        if repository is None:
            raise ValueError(f"Launchpad repository not found: {request['repository']}")
        text, source_url = render_repository(repository)
        return {"text": text, "source_url": source_url, "details": {"kind": "git_repository"}}
    if operation == "search_bugs":
        return search_bugs(lp, request)
    if operation == "search_merge_proposals":
        return search_proposals(lp, request)
    if operation == "bug_create":
        return create_bug(lp, request)
    if operation == "merge_proposal_create":
        return create_proposal(lp, request)
    if operation == "comment":
        return add_comment(lp, request)
    if operation == "set_merge_proposal_status":
        return set_proposal_status(lp, request)
    raise ValueError(f"Unsupported Launchpad operation: {operation}")


def main(lp):
    try:
        request = json.loads(os.environ["OMP_LAUNCHPAD_REQUEST"])
        result = dispatch(lp, request)
        payload = {"ok": True, **result}
    except Exception as error:
        payload = {"ok": False, "error": f"{type(error).__name__}: {error}"}
    print("__OMP_LAUNCHPAD__" + json.dumps(payload, default=scalar, ensure_ascii=False))


main(lp)
