use std::path::Path;

use percent_encoding::percent_decode_str;
use serde::Deserialize;
use url::Url;

use crate::error::Error;
use crate::result::Result;

const MAX_COMMENTS: usize = 20;
const MAX_RESULTS: usize = 50;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    ResourceView,
    RepoView,
    FileRead,
    SearchBugs,
    SearchMergeProposals,
    BugCreate,
    MergeProposalCreate,
    Comment,
    SetMergeProposalStatus,
    MergeProposalCheckout,
    MergeProposalPush,
}

impl Operation {
    pub fn requires_authentication(self) -> bool {
        matches!(
            self,
            Self::BugCreate
                | Self::MergeProposalCreate
                | Self::Comment
                | Self::SetMergeProposalStatus
        )
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl OneOrMany {
    pub fn append_to(
        &self,
        query: &mut url::form_urlencoded::Serializer<'_, url::UrlQuery<'_>>,
        name: &str,
    ) {
        match self {
            Self::One(value) => {
                query.append_pair(name, value);
            }
            Self::Many(values) => {
                for value in values {
                    query.append_pair(name, value);
                }
            }
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Request {
    pub op: Operation,
    pub target: Option<String>,
    pub repository: Option<String>,
    pub target_repository: Option<String>,
    pub path: Option<String>,
    pub branch: Option<String>,
    pub query: Option<String>,
    pub status: Option<OneOrMany>,
    pub importance: Option<OneOrMany>,
    pub tags: Option<Vec<String>>,
    pub limit: Option<usize>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub information_type: Option<String>,
    pub source_ref: Option<String>,
    pub target_ref: Option<String>,
    pub commit_message: Option<String>,
    pub needs_review: Option<bool>,
    pub body: Option<String>,
    pub subject: Option<String>,
    pub vote: Option<String>,
    pub directory: Option<String>,
    pub force_with_lease: Option<bool>,
}

impl Request {
    pub fn validate(&self) -> Result<()> {
        let required: &[(&str, &Option<String>)] = match self.op {
            Operation::ResourceView => &[("target", &self.target)],
            Operation::RepoView => &[("repository", &self.repository)],
            Operation::FileRead => &[("repository", &self.repository), ("path", &self.path)],
            Operation::SearchBugs => &[("target", &self.target)],
            Operation::SearchMergeProposals => &[("repository", &self.repository)],
            Operation::BugCreate => &[
                ("target", &self.target),
                ("title", &self.title),
                ("description", &self.description),
            ],
            Operation::MergeProposalCreate => &[
                ("repository", &self.repository),
                ("source_ref", &self.source_ref),
                ("target_ref", &self.target_ref),
            ],
            Operation::Comment => &[("target", &self.target), ("body", &self.body)],
            Operation::SetMergeProposalStatus => &[("target", &self.target)],
            Operation::MergeProposalCheckout => &[("target", &self.target)],
            Operation::MergeProposalPush => &[],
        };
        for (name, value) in required {
            if value.as_deref().is_none_or(str::is_empty) {
                return Err(Error::invalid(format!(
                    "{name} is required for {:?}",
                    self.op
                )));
            }
        }
        if self.op == Operation::SetMergeProposalStatus {
            self.status()?;
        }
        if self.limit == Some(0) {
            return Err(Error::invalid("limit must be greater than zero"));
        }
        if self.op == Operation::FileRead {
            validate_repository_path(self.path("path")?)?;
        }
        Ok(())
    }

    pub fn string<'value>(&self, value: &'value Option<String>, name: &str) -> Result<&'value str> {
        value
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| Error::invalid(format!("{name} is required for {:?}", self.op)))
    }

    pub fn target(&self) -> Result<&str> {
        self.string(&self.target, "target")
    }

    pub fn repository(&self) -> Result<&str> {
        self.string(&self.repository, "repository")
    }

    pub fn path(&self, name: &str) -> Result<&str> {
        self.string(&self.path, name)
    }

    pub fn limit(&self) -> usize {
        self.limit.unwrap_or(10).min(MAX_RESULTS)
    }

    pub fn status(&self) -> Result<&str> {
        match &self.status {
            Some(OneOrMany::One(status)) if !status.trim().is_empty() => Ok(status),
            _ => Err(Error::invalid(format!(
                "status must be a string for {:?}",
                self.op
            ))),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResourceKind {
    Bug { id: u64 },
    MergeProposal { repository: String, id: u64 },
    Repository,
    Generic,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceTarget {
    pub path: String,
    pub kind: ResourceKind,
    pub include_comments: bool,
    pub comment_limit: usize,
    pub diff: bool,
}

impl ResourceTarget {
    pub fn parse(raw: &str) -> Result<Self> {
        let (path, query) = split_target(raw)?;
        let include_comments = query
            .iter()
            .find(|(name, _)| name == "comments")
            .is_none_or(|(_, value)| value != "0");
        let comment_limit = query
            .iter()
            .find(|(name, _)| name == "limit")
            .map(|(_, value)| {
                value
                    .parse::<usize>()
                    .map(|limit| limit.clamp(1, MAX_COMMENTS))
                    .map_err(|_| Error::invalid("limit must be an integer"))
            })
            .transpose()?
            .unwrap_or(MAX_COMMENTS);
        let (path, diff) = strip_diff_suffix(path);
        let path = normalise_bug_path(path);
        let kind = classify_resource(&path)?;
        Ok(Self {
            path,
            kind,
            include_comments,
            comment_limit,
            diff,
        })
    }
}

pub fn validate_repository_path(path: &str) -> Result<()> {
    let path = Path::new(path);
    if path.is_absolute()
        || path
            .components()
            .any(|component| component.as_os_str() == "..")
    {
        return Err(Error::invalid(
            "path must be repository-relative and cannot contain '..'",
        ));
    }
    Ok(())
}

fn split_target(raw: &str) -> Result<(String, Vec<(String, String)>)> {
    let raw = raw.trim();
    if raw.starts_with("lp://") || raw.starts_with("http://") || raw.starts_with("https://") {
        let url = Url::parse(raw).map_err(|source| Error::Url {
            url: raw.to_owned(),
            source,
        })?;
        let mut path = String::new();
        if url.scheme() == "lp" {
            if let Some(host) = url.host_str() {
                path.push_str(host);
            }
        } else if !matches!(
            url.host_str(),
            Some("launchpad.net" | "bugs.launchpad.net" | "code.launchpad.net")
        ) {
            return Err(Error::invalid("target URL is not a Launchpad URL"));
        }
        let url_path = url.path().trim_start_matches('/');
        if !path.is_empty() && !url_path.is_empty() {
            path.push('/');
        }
        path.push_str(url_path);
        let path = percent_decode_str(&path)
            .decode_utf8()
            .map_err(|source| Error::UrlEncoding {
                reason: source.to_string(),
            })?
            .into_owned();
        let query = url
            .query_pairs()
            .map(|(name, value)| (name.into_owned(), value.into_owned()))
            .collect();
        return Ok((path, query));
    }
    Ok((raw.trim_start_matches('/').to_owned(), Vec::new()))
}

fn strip_diff_suffix(path: String) -> (String, bool) {
    for suffix in ["/diff/all", "/diff"] {
        if let Some(path) = path.strip_suffix(suffix) {
            return (path.to_owned(), true);
        }
    }
    (path, false)
}

fn normalise_bug_path(path: String) -> String {
    if let Some(id) = path.strip_prefix("bug/") {
        format!("bugs/{id}")
    } else if let Some(id) = path.strip_prefix("+bug/") {
        format!("bugs/{id}")
    } else {
        path
    }
}

fn classify_resource(path: &str) -> Result<ResourceKind> {
    if let Some(id) = path.strip_prefix("bugs/") {
        let id = id
            .parse()
            .map_err(|_| Error::invalid("bug target must end with a numeric ID"))?;
        return Ok(ResourceKind::Bug { id });
    }
    if let Some((repository, id)) = path.rsplit_once("/+merge/") {
        let id = id
            .parse()
            .map_err(|_| Error::invalid("merge proposal target must end with a numeric ID"))?;
        return Ok(ResourceKind::MergeProposal {
            repository: repository.to_owned(),
            id,
        });
    }
    if path.contains("/+git/") {
        return Ok(ResourceKind::Repository);
    }
    Ok(ResourceKind::Generic)
}

#[cfg(test)]
mod tests {
    use super::{ResourceKind, ResourceTarget, validate_repository_path};

    #[test]
    fn parses_bug_url_options() {
        let target = ResourceTarget::parse("lp://bugs/1?comments=0&limit=200").unwrap();
        assert_eq!(target.kind, ResourceKind::Bug { id: 1 });
        assert!(!target.include_comments);
        assert_eq!(target.comment_limit, 20);
    }

    #[test]
    fn parses_merge_proposal_diff() {
        let target =
            ResourceTarget::parse("lp://~owner/project/+git/repository/+merge/42/diff").unwrap();
        assert_eq!(
            target.kind,
            ResourceKind::MergeProposal {
                repository: "~owner/project/+git/repository".to_owned(),
                id: 42,
            }
        );
        assert!(target.diff);
    }

    #[test]
    fn rejects_parent_repository_path() {
        let error = validate_repository_path("src/../secret").unwrap_err();
        assert!(error.to_string().contains("cannot contain '..'"));
    }
}
