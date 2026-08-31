//! The official policy set: what is installed, what the Hub offers, and moving between them.
//!
//! **Why this is in `updaterd` at all.** It is not an update in the component sense — there is no
//! manifest, no signature, no health gate and no rollback, because a policy is not a binary
//! (`docs/design/policy-channel-design.md` §2). What it needs is a network stack and root, and
//! this is the daemon that has both: `robotd` has neither by design, and `robotctl` must not link
//! an HTTP client because it is the tool that has to work when everything else is broken.
//!
//! **Why it exists.** A board installs its set from a pin that ships inside the daemon release
//! (`scripts/seed-policies.sh`), which makes the pin a *floor* rather than a ceiling: without
//! something to move past it, a retrained gait would still need a daemon release to reach a
//! robot, which is the thing the whole channel was meant to stop. This is that something.
//!
//! The layout is the seeder's, and deliberately: `releases/<name>/` beside a `current` symlink,
//! swapped by rename. So a set installed here and a set installed by the seeder are the same
//! kind of thing, and each carries a `.source` record saying where it came from — which is how
//! [`check`] knows what repo to ask without anyone configuring it twice.

use std::path::{Path, PathBuf};

use crate::Error;
use crate::source::http;

/// Where the sets live. Matches `robotd_params::POLICY_DIR`'s parent and the seeder's default.
pub const POLICY_ROOT: &str = "/opt/robot/policies";

/// The provenance record the seeder writes beside a set.
const SOURCE_FILE: &str = ".source";

/// Sets installed from here are named for their revision, and the prefix marks them as *ours* —
/// the seeder uses the same one, and both refuse to disturb a `current` that has neither.
const SET_PREFIX: &str = "seed-";

/// What a policy set records about where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub repo: String,
    pub version: String,
}

impl Source {
    /// Parse the `key=value` file the seeder writes. Unknown keys are ignored, because a set
    /// installed by a newer tool must not be unreadable to an older daemon.
    pub fn parse(text: &str) -> Option<Self> {
        let mut repo = None;
        let mut version = None;
        for line in text.lines() {
            match line.split_once('=') {
                Some(("repo", v)) => repo = Some(v.trim().to_owned()),
                Some(("version", v)) => version = Some(v.trim().to_owned()),
                _ => {}
            }
        }
        Some(Self {
            repo: repo?,
            version: version?,
        })
    }

    fn render(&self) -> String {
        format!("repo={}\nversion={}\n", self.repo, self.version)
    }
}

/// The set `current` points at, and what it says about itself.
pub fn installed(root: &Path) -> Option<Source> {
    let text = std::fs::read_to_string(root.join("current").join(SOURCE_FILE)).ok()?;
    Source::parse(&text)
}

/// Every `.onnx` in the installed set — the list a replacement has to cover.
///
/// Read from disk rather than hardcoded here, because the daemon that knows which files a slot
/// can want is `robotd`, and the set on the board is the closest thing this process has to that
/// list. A repo that has gained a policy since is handled by [`fetch_set`], which takes whatever
/// the revision offers.
fn installed_files(root: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(root.join("current")) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".onnx"))
        .collect();
    names.sort();
    names
}

/// Tags in a Hub repo, newest first.
///
/// "Newest" is the repo's own order reversed, not a semver sort: a policy repo is not obliged to
/// use semver, and inventing an ordering for `bouncy-2` against `v10` would be a guess presented
/// as a fact. The Hub lists refs oldest-first.
async fn tags(client: &reqwest::Client, repo: &str) -> Result<Vec<String>, Error> {
    let url = format!("https://huggingface.co/api/models/{repo}/refs");
    let bytes = http::get_bytes(client, &url, None).await?;
    let refs: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|e| Error::Network(format!("parsing refs for {repo}: {e}")))?;
    let mut names: Vec<String> = refs
        .get("tags")
        .and_then(|t| t.as_array())
        .map(|tags| {
            tags.iter()
                .filter_map(|t| t.get("name")?.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    names.reverse();
    Ok(names)
}

/// What is installed against what the repo offers.
pub async fn check(root: &Path) -> crate::proto::PolicyCheckResult {
    let Some(source) = installed(root) else {
        return crate::proto::PolicyCheckResult {
            unreachable: Some("no policy set is installed, so there is nothing to check".into()),
            ..Default::default()
        };
    };

    let mut result = crate::proto::PolicyCheckResult {
        repo: Some(source.repo.clone()),
        installed: Some(source.version.clone()),
        ..Default::default()
    };

    let client = match http::client() {
        Ok(client) => client,
        Err(e) => {
            result.unreachable = Some(e.to_string());
            return result;
        }
    };
    match tags(&client, &source.repo).await {
        // An unreachable Hub is a fact about the network, not a failure of the question. The
        // robot is walking either way, and a caller shown "could not reach the Hub" beside what
        // is installed knows more than one shown an error.
        Err(e) => result.unreachable = Some(e.to_string()),
        Ok(versions) => {
            result.available = versions.first().cloned();
            result.versions = versions;
        }
    }
    result
}

/// Download one revision of a repo into `root`, and point `current` at it.
///
/// Nothing partial goes live: the files land in a staging directory and the symlink moves only
/// once every one of them has arrived. Same rule the seeder follows, and for the same reason —
/// a half-written set is one a restarting `robotd` could read.
pub async fn install(
    root: &Path,
    version: Option<&str>,
) -> Result<(String, Option<String>), Error> {
    let source = installed(root).ok_or_else(|| {
        Error::Network("no policy set is installed, so there is no repo to install from".into())
    })?;
    let client = http::client()?;

    let version = match version {
        Some(version) => version.to_owned(),
        None => tags(&client, &source.repo)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| Error::Network(format!("{} has no tags to install", source.repo)))?,
    };

    let previous = Some(source.version.clone()).filter(|v| *v != version);
    if previous.is_none() {
        return Ok((version, None));
    }

    let files = installed_files(root);
    if files.is_empty() {
        return Err(Error::Network(
            "the installed set has no .onnx files, so there is no list to fetch".into(),
        ));
    }

    let staging = root.join("releases").join(".staging-install");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| Error::Io {
        path: staging.clone(),
        source: e,
    })?;

    for name in &files {
        let url = format!(
            "https://huggingface.co/{}/resolve/{version}/{name}",
            source.repo
        );
        let bytes = http::get_bytes(&client, &url, None)
            .await
            .inspect_err(|_| {
                // Half a set is worse than none: leaving it would make the next attempt look
                // like a resume of something that was never coherent.
                let _ = std::fs::remove_dir_all(&staging);
            })?;
        std::fs::write(staging.join(name), bytes).map_err(|e| Error::Io {
            path: staging.join(name),
            source: e,
        })?;
    }

    let recorded = Source {
        repo: source.repo.clone(),
        version: version.clone(),
    };
    std::fs::write(staging.join(SOURCE_FILE), recorded.render()).map_err(|e| Error::Io {
        path: staging.join(SOURCE_FILE),
        source: e,
    })?;

    let name = format!("{SET_PREFIX}{version}");
    let dest = root.join("releases").join(&name);
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::rename(&staging, &dest).map_err(|e| Error::Io {
        path: dest.clone(),
        source: e,
    })?;

    swap_current(root, &name)?;
    Ok((version, previous))
}

/// Point `current` at `releases/<name>`, atomically.
///
/// Relative to the link's own directory, like the seeder writes it and like the updater's own
/// `current` — an absolute target resolves against the wrong place the moment the root moves.
fn swap_current(root: &Path, name: &str) -> Result<(), Error> {
    let staged = root.join("current.new");
    let _ = std::fs::remove_file(&staged);
    let target: PathBuf = ["releases", name].iter().collect();
    std::os::unix::fs::symlink(&target, &staged).map_err(|e| Error::Io {
        path: staged.clone(),
        source: e,
    })?;
    std::fs::rename(&staged, root.join("current")).map_err(|e| Error::Io {
        path: root.join("current"),
        source: e,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_source_record_round_trips() {
        let source = Source {
            repo: "pollen-robotics/microduck-policies".into(),
            version: "v1".into(),
        };
        assert_eq!(Source::parse(&source.render()), Some(source));
    }

    /// The seeder writes a `fetched=` line this does not care about, and a later tool may write
    /// more. An older daemon must still be able to read where a set came from — the alternative
    /// is a robot that cannot answer "what am I running" because something added a field.
    #[test]
    fn unknown_keys_in_a_source_record_are_ignored() {
        let text = "repo=org/set\nversion=v3\nfetched=2026-08-31T00:00:00Z\nfuture=whatever\n";
        assert_eq!(
            Source::parse(text),
            Some(Source {
                repo: "org/set".into(),
                version: "v3".into()
            })
        );
    }

    /// A record missing either half is not a record. Guessing a repo would send `check` to ask
    /// the wrong one, and guessing a version would report an upgrade that is really a sidegrade.
    #[test]
    fn an_incomplete_source_record_is_no_record() {
        assert_eq!(Source::parse("repo=org/set\n"), None);
        assert_eq!(Source::parse("version=v1\n"), None);
        assert_eq!(Source::parse(""), None);
    }

    /// `current` is a relative symlink beside `releases/`, so a root that moves — a test
    /// directory, a board with a different layout — keeps working.
    #[test]
    fn current_is_swapped_to_a_relative_target() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("releases/seed-v2")).unwrap();
        std::fs::write(root.join("releases/seed-v2/alpha_walking.onnx"), "w").unwrap();

        swap_current(root, "seed-v2").unwrap();
        assert_eq!(
            std::fs::read_link(root.join("current")).unwrap(),
            Path::new("releases/seed-v2")
        );
        assert!(root.join("current/alpha_walking.onnx").exists());
    }

    /// Swapping over an existing link replaces it rather than landing inside what it points at,
    /// which is the mistake a plain `mv` onto a symlink-to-directory makes.
    #[test]
    fn swapping_replaces_an_existing_link() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for name in ["seed-v1", "seed-v2"] {
            std::fs::create_dir_all(root.join("releases").join(name)).unwrap();
        }
        swap_current(root, "seed-v1").unwrap();
        swap_current(root, "seed-v2").unwrap();

        assert_eq!(
            std::fs::read_link(root.join("current")).unwrap(),
            Path::new("releases/seed-v2")
        );
        assert!(!root.join("releases/seed-v1/releases").exists());
    }

    /// The fetch list comes from what is on the board, so a set is replaced file for file.
    #[test]
    fn the_fetch_list_is_what_the_installed_set_holds() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("releases/seed-v1")).unwrap();
        for name in ["alpha_walking.onnx", "roulade.onnx"] {
            std::fs::write(root.join("releases/seed-v1").join(name), "x").unwrap();
        }
        std::fs::write(
            root.join("releases/seed-v1/.source"),
            "repo=o/r\nversion=v1\n",
        )
        .unwrap();
        swap_current(root, "seed-v1").unwrap();

        assert_eq!(
            installed_files(root),
            vec!["alpha_walking.onnx".to_string(), "roulade.onnx".to_string()],
            "the .source record is not a policy"
        );
    }

    /// A board with nothing installed has no repo to ask about, and says so rather than
    /// inventing one — the repo is a property of the set, not of this daemon.
    #[tokio::test]
    async fn a_board_with_no_set_reports_that_rather_than_guessing() {
        let tmp = tempfile::tempdir().unwrap();
        let result = check(tmp.path()).await;
        assert!(result.repo.is_none());
        assert!(result.installed.is_none());
        assert!(result.unreachable.unwrap().contains("nothing to check"));
    }
}
