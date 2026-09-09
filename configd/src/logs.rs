//! The tail of one daemon's journal, for a client that has no shell on the robot.
//!
//! [`units`](crate::units) answers "is `robotd` running"; this answers the question that follows a
//! `failed` — *why*. Until it existed the only answer was `journalctl` over ssh, which needs a
//! network the robot may not have and an address a phone cannot reach. The robot whose journal
//! somebody actually needs is the one whose wifi never came up.
//!
//! **`configd` reads it because reading the journal needs privilege**, the same reason
//! [`units`](crate::units) is here: it runs as root (see `systemd/configd.service`), and
//! `robotctl` does not. On the robot itself nobody should use this — `journalctl` is right there,
//! with `-g`, `--since` and a pager. This is for the transports that have none of that.
//!
//! ## Why it shells out to `journalctl`
//!
//! Reading the journal in-process means linking `libsystemd`, which means vendored C in a
//! cross-compiled build for one read-only query. `journalctl` is on every board this ships to,
//! it is already how the updater talks to systemd (`updater::engine` spawns `systemctl`), and
//! the sandbox in this service's unit permits it: `ProtectSystem=strict` leaves `/usr/bin`
//! readable, and root can read the journal without any capability.
//!
//! ## Why it takes a unit name and not arguments
//!
//! This call is reachable from a phone in radio range (`btd::route`). `journalctl` arguments are
//! not a language to hand such a peer — `-D` reads another journal directory, `_PID=` matches
//! anything on the box — so the unit is chosen from a fixed list and everything else is refused
//! by name. What that costs is the searching a person with a shell would do. What it buys is that
//! the worst any client can ask for is the tail of a daemon this project ships.

use duck_ipc_proto as proto;

/// Units that are not part of a daemon release but whose journals answer *our* failures.
///
/// Both earn their place by being the thing that is broken when the robot cannot be reached at
/// all: BlueZ, when no phone can see the robot, and NetworkManager, when wifi never came up.
/// Neither is ours, which is exactly why their logs are otherwise unreachable — nothing in
/// `system.services` reports on them either.
pub const ALSO_LOGGABLE: [&str; 2] = ["bluetooth.service", "NetworkManager.service"];

/// Every unit whose journal this service will read, in the order a reader wants them.
///
/// Derived from [`crate::units::MANAGED`] rather than repeated, so a unit added to a release
/// becomes readable here without anyone remembering this file — the mistake that list's own doc
/// records having made once.
pub fn loggable() -> impl Iterator<Item = &'static str> {
    crate::units::MANAGED.into_iter().chain(ALSO_LOGGABLE)
}

/// Longest single line kept, in bytes.
//
// Off Linux there is no `read` to call these, and only the tests do — the same shape
// `units::describe` takes, where the Linux half is the real one and the other half exists so a
// laptop can still build and test the crate.
#[cfg(any(target_os = "linux", test))]
///
/// journald accepts a stdout line up to its own `LineMax` (48 KiB by default), so one daemon
/// logging a serialised blob could otherwise fill a whole reply with one entry and push out the
/// hundred lines around it — which is the opposite of what a tail is for. Truncated with an
/// ellipsis, so a reader can see that the line went on.
const MAX_LINE_BYTES: usize = 2 * 1024;

/// Resolve a caller's unit name against [`loggable`].
///
/// `robotd` and `robotd.service` both work: a caller typing the daemon's name should not have to
/// know that systemd spells it with a suffix. The answer is the canonical name, so the reply says
/// what was actually read.
pub fn resolve(name: &str) -> Option<&'static str> {
    let wanted = name.trim();
    let wanted = wanted.strip_suffix(".service").unwrap_or(wanted);
    // Case-insensitive for `NetworkManager` alone, really: nobody types that capitalisation from
    // memory, and refusing `networkmanager` would be a puzzle rather than a guard.
    loggable().find(|unit| {
        unit.strip_suffix(".service")
            .unwrap_or(unit)
            .eq_ignore_ascii_case(wanted)
    })
}

/// What to tell a caller that named something else. Names the real list, the way `pad.bind`
/// refuses an unknown skill: a typo should come back as the answer, not as a shrug.
pub fn refusal(name: &str) -> proto::Error {
    proto::Error::new(
        proto::code::INVALID_PARAMS,
        format!(
            "no journal here for {name:?}; readable units are {}",
            loggable().collect::<Vec<_>>().join(", ")
        ),
    )
}

/// Read the tail of a unit's journal.
///
/// `unit` must have come from [`resolve`] — that is what keeps a caller's string out of
/// `journalctl`'s argument list.
#[cfg(target_os = "linux")]
pub async fn read(
    unit: &'static str,
    lines: usize,
    boot: i32,
) -> Result<proto::LogsResult, String> {
    let lines = lines.clamp(1, proto::MAX_LOG_LINES);

    let output = tokio::process::Command::new("journalctl")
        .arg("--unit")
        .arg(unit)
        // `--boot=-1` as one argument rather than `-b -1`: an offset that starts with a dash is
        // otherwise ambiguous with an option, and which of the two `journalctl` decides it is has
        // changed between versions.
        .arg(format!("--boot={boot}"))
        .arg("--lines")
        .arg(lines.to_string())
        .arg("--no-pager")
        // The timestamp is the point — a log line with no time answers nothing — and the
        // `robotd[1234]:` prefix carries the PID, which is how a restart shows up in a tail.
        // The hostname is the one field that is the same on every line and known already.
        .arg("--output=short-iso")
        .arg("--no-hostname")
        .output()
        .await
        .map_err(|e| format!("could not run journalctl: {e}"))?;

    if !output.status.success() {
        // journalctl's own words, which for the case a caller will actually hit — a boot that is
        // not in the journal — are already the right answer ("Data from the specified boot (-1) is
        // not available"). Ours would be a worse paraphrase.
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.lines().next().unwrap_or("no output").trim();
        return Err(format!("journalctl refused: {detail}"));
    }

    Ok(assemble(unit, &String::from_utf8_lossy(&output.stdout)))
}

/// Off the board there is no journal to read, and inventing one would make a laptop look like a
/// robot. An error rather than an empty answer, for the same reason [`crate::units::describe`]
/// reports `Unknown` off Linux: "there is nothing here to read" and "this daemon said nothing"
/// are different answers.
#[cfg(not(target_os = "linux"))]
pub async fn read(
    unit: &'static str,
    _lines: usize,
    _boot: i32,
) -> Result<proto::LogsResult, String> {
    Err(format!(
        "no systemd journal on this host, so nothing to read for {unit}"
    ))
}

/// Turn `journalctl`'s output into a reply that fits the wire.
///
/// Split out from [`read`] so the trimming is testable without a journal, which is the half with
/// the decisions in it: **oldest lines go first**, because the newest are the ones somebody asked
/// for, and the budget is measured on the serialised form rather than estimated, since JSON
/// escaping is what makes an estimate wrong.
#[cfg(any(target_os = "linux", test))]
fn assemble(unit: &'static str, stdout: &str) -> proto::LogsResult {
    let mut lines: Vec<String> = stdout
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .map(truncate_line)
        .collect();

    let mut truncated = false;
    while serde_json::to_string(&lines).map_or(0, |json| json.len()) > proto::MAX_LOG_BYTES
        && !lines.is_empty()
    {
        lines.remove(0);
        truncated = true;
    }

    proto::LogsResult {
        unit: unit.to_owned(),
        lines,
        truncated,
    }
}

/// One line, bounded. Cuts on a character boundary, since the reply has to be valid UTF-8.
#[cfg(any(target_os = "linux", test))]
fn truncate_line(line: &str) -> String {
    if line.len() <= MAX_LINE_BYTES {
        return line.to_owned();
    }
    let mut end = MAX_LINE_BYTES;
    while end > 0 && !line.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &line[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_daemon_is_named_with_or_without_the_suffix() {
        assert_eq!(resolve("robotd"), Some("robotd.service"));
        assert_eq!(resolve("robotd.service"), Some("robotd.service"));
        assert_eq!(resolve(" btd "), Some("btd.service"));
    }

    /// Nobody types that capitalisation from memory.
    #[test]
    fn networkmanager_is_matched_whatever_the_case() {
        assert_eq!(resolve("networkmanager"), Some("NetworkManager.service"));
        assert_eq!(resolve("NetworkManager"), Some("NetworkManager.service"));
    }

    /// The guard the BLE route depends on: anything not on the list is refused, including the
    /// shapes somebody would try if they were treating this as a `journalctl` command line.
    #[test]
    fn anything_else_is_refused() {
        for name in [
            "sshd",
            "",
            "robotd.service extra",
            "--directory=/run/log/journal",
            "_PID=1",
            "*",
        ] {
            assert_eq!(resolve(name), None, "{name} resolved");
        }
    }

    /// A unit added to a release becomes readable without touching this file.
    #[test]
    fn every_managed_unit_is_loggable() {
        for unit in crate::units::MANAGED {
            assert_eq!(resolve(unit), Some(unit), "{unit} is not loggable");
        }
    }

    #[test]
    fn the_refusal_names_the_units_it_would_accept() {
        let message = refusal("sshd").message;
        assert!(message.contains("sshd"), "{message}");
        assert!(message.contains("robotd.service"), "{message}");
        assert!(message.contains("NetworkManager.service"), "{message}");
    }

    #[test]
    fn a_tail_comes_back_oldest_first_with_blank_lines_dropped() {
        let result = assemble("robotd.service", "one\ntwo\n\nthree\n");
        assert_eq!(result.lines, ["one", "two", "three"]);
        assert!(!result.truncated);
    }

    /// Over budget, the *newest* lines survive — the ones somebody asked for.
    #[test]
    fn an_oversized_tail_keeps_the_end_and_says_it_was_cut() {
        let stdout: String = (0..4000)
            .map(|i| format!("2026-09-09T12:00:00+0200 robotd[1]: line {i}\n"))
            .collect();

        let result = assemble("robotd.service", &stdout);

        assert!(result.truncated);
        assert!(
            serde_json::to_string(&result.lines).unwrap().len() <= proto::MAX_LOG_BYTES,
            "reply is over the budget the transport can carry"
        );
        assert!(
            result.lines.last().unwrap().ends_with("line 3999"),
            "dropped from the wrong end: {:?}",
            result.lines.last()
        );
    }

    /// One daemon logging a serialised blob must not push out the lines around it.
    #[test]
    fn a_single_enormous_line_is_cut_rather_than_taking_the_whole_reply() {
        let stdout = format!("prefix {}\nafter\n", "x".repeat(60 * 1024));
        let result = assemble("robotd.service", &stdout);

        assert_eq!(result.lines.len(), 2);
        assert!(result.lines[0].len() <= MAX_LINE_BYTES + '…'.len_utf8());
        assert!(result.lines[0].ends_with('…'));
        assert_eq!(result.lines[1], "after");
        assert!(!result.truncated);
    }

    /// Cutting a multi-byte character in half would make the reply invalid UTF-8, and the line
    /// that will hit this is a log message with an arrow or an accent in it — which ours have.
    #[test]
    fn a_line_is_cut_on_a_character_boundary() {
        let line = "é".repeat(MAX_LINE_BYTES);
        let cut = truncate_line(&line);
        assert!(cut.ends_with('…'));
        assert!(cut.len() <= MAX_LINE_BYTES + '…'.len_utf8());
    }
}
