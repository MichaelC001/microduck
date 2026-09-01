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
    // Nothing installed is not a network problem, and saying so through `unreachable` reported a
    // healthy Hub as unreachable on a board whose only fault was having no set yet. An empty
    // `repo` says it on its own, unambiguously — there is no other way to get one.
    let Some(source) = installed(root) else {
        return crate::proto::PolicyCheckResult::default();
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

    // `download_to`, not `get_bytes`. The latter is for manifests and API replies and enforces a
    // one-megabyte ceiling with the message "implausibly large for metadata" — which a policy is
    // not, and which today's are only just under. A retrain that produced a slightly larger
    // network would have failed to install with a sentence about metadata.
    let (progress, mut drain) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move { while drain.recv().await.is_some() {} });
    for name in &files {
        let url = format!(
            "https://huggingface.co/{}/resolve/{version}/{name}",
            source.repo
        );
        http::download_to(&client, &url, &staging.join(name), None, &progress)
            .await
            .inspect_err(|_| {
                // Half a set is worse than none: leaving it would make the next attempt look
                // like a resume of something that was never coherent.
                let _ = std::fs::remove_dir_all(&staging);
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
    prune(root, &name, previous.as_deref());
    Ok((version, previous))
}

/// Drop older sets, keeping the live one and the one it replaced.
///
/// Every install is a new directory of about seven megabytes, in a place nothing else tidies, so
/// somebody moving back and forth between revisions would fill an eMMC. The previous one is kept
/// deliberately: **rollback does not run hooks** (`Engine::post_swap` is on the apply path only),
/// so reverting the daemon does not revert its policies, and pointing `current` back at the kept
/// set is the recovery when a policy is what went wrong.
///
/// Best effort. A set that cannot be removed is disk space, not a failed install, and undoing a
/// good install over it would be the wrong trade.
fn prune(root: &Path, keep: &str, previous: Option<&str>) {
    let previous = previous.map(|v| format!("{SET_PREFIX}{v}"));
    let Ok(entries) = std::fs::read_dir(root.join("releases")) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // `seed-` only. Anything else under here belongs to whatever installed it, which is the
        // rule the seeder opens with and this must not be the exception to.
        if !name.starts_with(SET_PREFIX) || name == keep || Some(&name) == previous.as_ref() {
            continue;
        }
        if let Err(e) = std::fs::remove_dir_all(entry.path()) {
            tracing::warn!(set = %name, error = %e, "could not remove an old policy set");
        }
    }
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

    /// Installs accumulate a directory each, so moving between revisions a few times would fill
    /// an eMMC with sets nothing else tidies.
    #[test]
    fn old_sets_are_pruned_to_the_live_one_and_its_predecessor() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for name in ["seed-v1", "seed-v2", "seed-v3", "from-a-tool"] {
            std::fs::create_dir_all(root.join("releases").join(name)).unwrap();
        }

        prune(root, "seed-v3", Some("v2"));

        let mut left: Vec<String> = std::fs::read_dir(root.join("releases"))
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(
            left,
            vec![
                "from-a-tool".to_string(),
                "seed-v2".to_string(),
                "seed-v3".to_string()
            ],
            "the live set, the one it replaced, and anything that is not ours"
        );
    }

    /// A first install has no predecessor, and must not read that as licence to keep nothing —
    /// nor to delete a set some other tool put there.
    #[test]
    fn pruning_without_a_predecessor_keeps_what_is_not_ours() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for name in ["seed-v1", "from-a-tool"] {
            std::fs::create_dir_all(root.join("releases").join(name)).unwrap();
        }

        prune(root, "seed-v1", None);

        assert!(root.join("releases/seed-v1").exists());
        assert!(root.join("releases/from-a-tool").exists());
    }

    fn here() -> Expectations {
        Expectations::here(Some(1))
    }

    fn manifest(json: serde_json::Value) -> PolicyManifest {
        serde_json::from_value(json).expect("a manifest")
    }

    /// **The real convention, taken from a policy actually published to the Hub.** These are the
    /// fields `RemiFabre/microduck-flamingo-cycle` carries, and this asserts we read the ones we
    /// act on rather than a shape we invented.
    #[test]
    fn a_real_community_manifest_parses_and_passes() {
        let m = manifest(serde_json::json!({
            "schema_version": 2, "model_api": 1, "name": "flamingo-cycle",
            "kind": "perpetual", "obs_len": 61, "action_len": 14, "action_scale": 1.0,
            "entry_pose": "standing", "duration_s": null,
            "description": "Stand on one foot, either side, on command.",
            "command": { "head": "unused (zeros)" },
            "robot": { "model": "microduck", "hw_rev": 1, "servos": "xl330" },
            "training": { "task_id": "Mjlab-FlamingoCycleHard-Flat-MicroDuck" }
        }));
        assert_eq!(m.name.as_deref(), Some("flamingo-cycle"));
        assert_eq!(m.kind.as_deref(), Some("perpetual"));
        assert_eq!(m.incompatibility(here()), None);
    }

    /// The whole point of reading the manifest: a 51-D policy is refused before 800 KB is
    /// downloaded and before the robot is asked to run it. `robotd` would refuse it at load
    /// anyway — this is the same answer, arriving where somebody can act on it.
    #[test]
    fn a_policy_the_manifest_says_is_the_wrong_shape_is_refused() {
        let m = manifest(serde_json::json!({ "obs_len": 51, "action_len": 14 }));
        let why = m.incompatibility(here()).expect("refused");
        assert!(why.contains("51") && why.contains("61"), "{why}");
    }

    /// The model-API rule from `updater-design.md` §5.5, finally doing something: a policy needing
    /// a newer daemon is refused with the remedy in it, and an older one still loads.
    #[test]
    fn a_policy_needing_a_newer_daemon_says_so() {
        let newer = manifest(serde_json::json!({ "model_api": 2 }));
        let why = newer.incompatibility(here()).expect("refused");
        assert!(why.contains("update the daemon"), "{why}");

        let older = manifest(serde_json::json!({ "model_api": 1 }));
        assert_eq!(older.incompatibility(Expectations::here(Some(2))), None);
    }

    /// A policy published for a different robot is not this robot's to run.
    #[test]
    fn a_policy_for_another_robot_is_refused() {
        let m = manifest(serde_json::json!({ "robot": { "model": "reachy" } }));
        assert!(m.incompatibility(here()).unwrap().contains("reachy"));
    }

    /// **Absence is not evidence.** A repo with no manifest, or one that omits the fields we act
    /// on, must not be refused — most of the Hub follows no convention of ours, and the shape gate
    /// at load was always going to be the real check.
    #[test]
    fn a_manifest_that_claims_nothing_refuses_nothing() {
        assert_eq!(PolicyManifest::default().incompatibility(here()), None);
        let sparse = manifest(serde_json::json!({ "name": "something", "unknown_field": 3 }));
        assert_eq!(sparse.incompatibility(here()), None);
    }

    /// Origin is the org, and nothing else.
    #[test]
    fn origin_is_decided_by_the_org() {
        assert_eq!(
            origin_of_repo("pollen-robotics/microduck-policies"),
            "official"
        );
        assert_eq!(
            origin_of_repo("RemiFabre/microduck-flamingo-cycle"),
            "community"
        );
        // Not a prefix match: an org that merely starts the same way is somebody else.
        assert_eq!(origin_of_repo("pollen-robotics-fake/x"), "community");
        assert_eq!(origin_of_repo("nonsense"), "community");
    }

    /// One `.onnx` is the answer, and it is the answer for every microduck policy published so
    /// far — they all carry a single `policy.onnx` beside a README and a manifest.
    #[test]
    fn the_sole_policy_in_a_repo_is_the_one_to_take() {
        let files = vec![
            ".gitattributes".to_string(),
            "README.md".to_string(),
            "manifest.json".to_string(),
            "policy.onnx".to_string(),
        ];
        assert_eq!(sole_policy(&files, "org/x").unwrap(), "policy.onnx");
    }

    /// Several is a refusal naming them, not a guess. Picking wrong here means running the wrong
    /// network on a real robot, which is not a coin to toss.
    #[test]
    fn a_repo_with_several_policies_asks_which() {
        let files = vec!["walk.onnx".to_string(), "run.onnx".to_string()];
        let why = sole_policy(&files, "org/x").unwrap_err().to_string();
        assert!(
            why.contains("walk.onnx") && why.contains("run.onnx"),
            "{why}"
        );
        assert!(why.contains("<file>"), "and how to say which: {why}");

        let none: Vec<String> = vec!["README.md".to_string()];
        assert!(
            sole_policy(&none, "org/x")
                .unwrap_err()
                .to_string()
                .contains("no .onnx")
        );
    }

    /// A board with nothing installed has no repo to ask about, and says so rather than inventing
    /// one — the repo is a property of the set, not of this daemon.
    ///
    /// **And it is not a network failure.** Reporting it through `unreachable` made a board whose
    /// only fault was having no set yet print "the Hub could not be reached", which sent the
    /// reader after a problem that did not exist.
    #[tokio::test]
    async fn a_board_with_no_set_is_not_a_hub_that_cannot_be_reached() {
        let tmp = tempfile::tempdir().unwrap();
        let result = check(tmp.path()).await;
        assert!(result.repo.is_none());
        assert!(result.installed.is_none());
        assert!(
            result.unreachable.is_none(),
            "nothing installed is not a network problem: {:?}",
            result.unreachable
        );
    }

    /// A set whose provenance record is missing still counts as installed, and the version comes
    /// from the directory name — which is the shape of a board seeded before the record existed.
    /// The seeder back-fills it, but this must not report "nothing installed" in the meantime.
    #[test]
    fn a_set_without_a_record_is_still_a_set_on_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("releases/seed-v1")).unwrap();
        std::fs::write(root.join("releases/seed-v1/alpha_walking.onnx"), "w").unwrap();
        swap_current(root, "seed-v1").unwrap();

        assert!(installed(root).is_none(), "no record, so no repo to name");
        assert_eq!(
            installed_files(root),
            vec!["alpha_walking.onnx".to_string()],
            "but the policies are plainly there"
        );
    }
}

// ── the community library ────────────────────────────────────────────────────
//
// One policy, from any Hub repo, into a slot. Separate from the official set above and
// deliberately so: a set is nine files that version together and fill every slot, and this is one
// file somebody wants to try in one of them.
//
// Nothing here is signed, per `docs/design/policy-channel-design.md` §2. A policy is not a
// binary: `robotd` holds the only write handle to the bus behind joint clamps, a fall reflex and
// an intent deadman, and refuses any graph that is not obs[1,61] -> actions[1,14] while the robot
// is standing still. That sandbox is the boundary, not a signature.

/// Where fetched policies live. Outside every release directory, per `updater-design.md` §5.7 —
/// a policy somebody chose must survive an update and a rollback.
pub const LIBRARY_ROOT: &str = "/var/lib/robot/policies";

/// The org whose policies are "official". One constant: a robot that can be *told* which org to
/// trust has a badge that means nothing.
pub const OFFICIAL_ORG: &str = "pollen-robotics";

/// `"official"` or `"community"`, from the repo that published it.
pub fn origin_of_repo(repo: &str) -> &'static str {
    match repo.split_once('/') {
        Some((org, _)) if org == OFFICIAL_ORG => "official",
        _ => "community",
    }
}

/// What a repo's `manifest.json` says about the policy in it.
///
/// **Untrusted.** It is a stranger's description of a stranger's file, and every field is taken as
/// a claim rather than a fact. It is worth reading anyway: a policy that *says* it is 51-D can be
/// refused before 800 KB is downloaded and before the robot is asked to run it, which is a much
/// better error than the same refusal arriving at load. A manifest that lies is caught there, by
/// the check that has always been the real one.
///
/// The shape is the convention the published microduck policies already use, and everything is
/// optional because a repo is under no obligation to carry any of it.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct PolicyManifest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub kind: Option<String>,
    /// Seconds the policy runs, for one that ends itself. The convention's field name.
    pub duration_s: Option<f64>,
    pub action_scale: Option<f64>,
    /// Seconds the policy needs to get back to `command.idle` from wherever it holds.
    ///
    /// Only a perpetual policy has one — an episodic policy is already back by the time its
    /// `duration_s` is up. It is what the daemon drives the idle command for before handing over
    /// to the gait, so that a robot holding a foot in the air is not simply let go of.
    pub unwind_s: Option<f64>,
    pub command: Option<ManifestCommand>,
    pub obs_len: Option<usize>,
    pub action_len: Option<usize>,
    pub model_api: Option<u32>,
    pub robot: Option<ManifestRobot>,
}

/// The command block, as the published convention describes it.
///
/// `twist`, `head` and `body` are prose for a person — "flag: 0 = stand on two feet" — and are
/// not read. `idle` is the one machine-readable part and the one that matters: it is the command
/// that means "stop doing the thing", which is what a skill unwinds to.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct ManifestCommand {
    pub idle: Option<[f64; 3]>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct ManifestRobot {
    pub model: Option<String>,
}

/// What this robot expects of a policy.
///
/// A value rather than constants read in place, so the manifest check can be tested against
/// expectations that are not this board's. The defaults come from `duck_ipc_proto`, which is
/// where the shape contract is published precisely because it is a contract with whoever
/// publishes a policy — `duck_control` asserts at compile time that its own constants agree.
#[derive(Debug, Clone, Copy)]
pub struct Expectations {
    pub obs_len: usize,
    pub action_len: usize,
    pub model_api: u32,
    pub robot_model: &'static str,
}

impl Expectations {
    /// What this daemon believes, with the model API the running `robotd` reports.
    ///
    /// `None` — an unreachable robot — takes the contract's own version rather than refusing
    /// everything: fetching a policy onto a board whose control loop is down is a reasonable
    /// thing to be doing, and the load will check it properly when the loop comes back.
    pub fn here(model_api: Option<u32>) -> Self {
        Self {
            obs_len: crate::proto::POLICY_OBS_LEN,
            action_len: crate::proto::POLICY_ACTION_LEN,
            model_api: model_api.unwrap_or(1),
            robot_model: crate::proto::ROBOT_MODEL,
        }
    }
}

impl PolicyManifest {
    /// Refuse a policy the manifest itself says will not work here.
    ///
    /// Only refuses on a claim that is *present and wrong*. A manifest with no `obs_len` is not
    /// evidence of anything, and refusing on absence would reject every repo that does not follow
    /// a convention nobody has published.
    pub fn incompatibility(&self, expected: Expectations) -> Option<String> {
        if let Some(obs) = self.obs_len
            && obs != expected.obs_len
        {
            return Some(format!(
                "its manifest says observation width {obs}, and this robot builds {}",
                expected.obs_len
            ));
        }
        if let Some(actions) = self.action_len
            && actions != expected.action_len
        {
            return Some(format!(
                "its manifest says {actions} actions, and this robot has {}",
                expected.action_len
            ));
        }
        if let Some(api) = self.model_api
            && api > expected.model_api
        {
            return Some(format!(
                "it needs model API {api} and this daemon speaks {} — update the daemon first",
                expected.model_api
            ));
        }
        if let Some(model) = self.robot.as_ref().and_then(|r| r.model.as_deref())
            && !model.eq_ignore_ascii_case(expected.robot_model)
        {
            return Some(format!(
                "it is for a {model}, and this is a {}",
                expected.robot_model
            ));
        }
        None
    }
}

/// Everything in a repo revision, as the Hub lists it.
async fn tree(client: &reqwest::Client, repo: &str, revision: &str) -> Result<Vec<String>, Error> {
    let url = format!("https://huggingface.co/api/models/{repo}/tree/{revision}");
    let bytes = http::get_bytes(client, &url, None).await?;
    let listing: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|e| Error::Network(format!("listing {repo}@{revision}: {e}")))?;
    Ok(listing
        .as_array()
        .map(|files| {
            files
                .iter()
                .filter_map(|f| f.get("path")?.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default())
}

/// The commit a revision points at, so a moving branch can be noticed later.
async fn commit_of(client: &reqwest::Client, repo: &str, revision: &str) -> Option<String> {
    let url = format!("https://huggingface.co/api/models/{repo}/revision/{revision}");
    let bytes = http::get_bytes(client, &url, None).await.ok()?;
    let info: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    info.get("sha")?.as_str().map(str::to_owned)
}

/// Pick the policy file out of a repo listing.
///
/// Exactly one `.onnx` is the answer, and it is the answer for every microduck policy published
/// so far — they all carry a single `policy.onnx`. Several is a refusal naming them rather than a
/// guess: choosing wrong here means running the wrong network on a real robot.
fn sole_policy(files: &[String], repo: &str) -> Result<String, Error> {
    let mut candidates: Vec<&String> = files.iter().filter(|f| f.ends_with(".onnx")).collect();
    candidates.sort();
    match candidates.len() {
        1 => Ok(candidates[0].clone()),
        0 => Err(Error::Network(format!("{repo} has no .onnx in it"))),
        _ => Err(Error::Network(format!(
            "{repo} has {} policies — name one with `<repo>:<file>`: {}",
            candidates.len(),
            candidates
                .iter()
                .map(|c| c.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

/// Fetch one policy into the library and say what it turned out to be.
pub async fn fetch(
    library: &Path,
    repo: &str,
    revision: Option<&str>,
    file: Option<&str>,
    expected: Expectations,
) -> Result<crate::proto::PolicyFetchResult, Error> {
    // A repo is `org/name`, and nothing else. Checked before it is pasted into a URL and before
    // any of it becomes a directory name.
    let Some((org, name)) = repo.split_once('/') else {
        return Err(Error::Network(format!("{repo} is not an org/name repo")));
    };
    for part in [org, name] {
        if part.is_empty() || part.contains(['.', '/', '\\']) {
            return Err(Error::Network(format!("{repo} is not an org/name repo")));
        }
    }
    let revision = revision.unwrap_or("main");
    if revision.contains(['/', '\\']) || revision.starts_with('.') {
        return Err(Error::Network(format!("{revision} is not a revision")));
    }

    let client = http::client()?;

    // The manifest first, so a policy that says it cannot work here costs one small request
    // rather than a download and a refusal from the control loop.
    let manifest_url = format!("https://huggingface.co/{repo}/resolve/{revision}/manifest.json");
    let manifest: PolicyManifest = match http::get_bytes(&client, &manifest_url, None).await {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        // No manifest is not a fault. Plenty of repos will not have one, and the shape gate at
        // load is the check that was always going to decide this.
        Err(_) => PolicyManifest::default(),
    };
    if let Some(why) = manifest.incompatibility(expected) {
        return Err(Error::Network(format!(
            "{repo} will not run on this robot: {why}"
        )));
    }

    let file = match file {
        Some(file) => {
            if file.contains('/') || file.starts_with('.') || !file.ends_with(".onnx") {
                return Err(Error::Network(format!("{file} is not a policy file name")));
            }
            file.to_owned()
        }
        None => sole_policy(&tree(&client, repo, revision).await?, repo)?,
    };

    let dir = library.join(org).join(name).join(revision);
    std::fs::create_dir_all(&dir).map_err(|e| Error::Io {
        path: dir.clone(),
        source: e,
    })?;

    // Staged beside the destination, so a download interrupted halfway is never a file a slot
    // could be pointed at.
    let staged = dir.join(format!("{file}.part"));
    let (progress, mut drain) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move { while drain.recv().await.is_some() {} });
    let url = format!("https://huggingface.co/{repo}/resolve/{revision}/{file}");
    http::download_to(&client, &url, &staged, None, &progress)
        .await
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&staged);
        })?;
    let path = dir.join(&file);
    std::fs::rename(&staged, &path).map_err(|e| Error::Io {
        path: path.clone(),
        source: e,
    })?;

    let commit = commit_of(&client, repo, revision).await;
    let record = format!(
        "repo={repo}\nversion={revision}\ncommit={}\nfile={file}\nfetched={}\n",
        commit.as_deref().unwrap_or("unknown"),
        now_utc(),
    );
    let _ = std::fs::write(dir.join(SOURCE_FILE), record);

    Ok(crate::proto::PolicyFetchResult {
        path: path.display().to_string(),
        repo: repo.to_owned(),
        revision: revision.to_owned(),
        commit,
        file,
        origin: origin_of_repo(repo).to_owned(),
        name: manifest.name,
        description: manifest.description,
        kind: manifest.kind,
        duration_s: manifest.duration_s,
        action_scale: manifest.action_scale,
        unwind_s: manifest.unwind_s,
        idle: manifest.command.and_then(|c| c.idle),
    })
}

/// An RFC-3339 timestamp without pulling in a date library for one line.
fn now_utc() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("@{secs}")
}

/// Hub models matching a query.
///
/// No tag filter yet: `microduck` in the name is what the published policies have in common, and
/// a tag is something to add once there is something to tag. Every field is the publisher's.
pub async fn search(query: &str) -> Result<crate::proto::PolicySearchResult, Error> {
    let client = http::client()?;
    let encoded: String = query
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | ' '))
        .map(|c| if c == ' ' { '+' } else { c })
        .collect();
    let url = format!("https://huggingface.co/api/models?search={encoded}&limit=25");
    let bytes = http::get_bytes(&client, &url, None).await?;
    let hits: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|e| Error::Network(format!("searching for {query}: {e}")))?;

    let models = hits
        .as_array()
        .map(|models| {
            models
                .iter()
                .filter_map(|m| {
                    let id = m.get("modelId")?.as_str()?.to_owned();
                    let origin = origin_of_repo(&id).to_owned();
                    Some(crate::proto::PolicySearchHit {
                        id,
                        origin,
                        likes: m.get("likes").and_then(|v| v.as_u64()),
                        downloads: m.get("downloads").and_then(|v| v.as_u64()),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(crate::proto::PolicySearchResult { models })
}
