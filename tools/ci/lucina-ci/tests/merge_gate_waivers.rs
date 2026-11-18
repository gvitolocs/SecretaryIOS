//! `check-fast --merge-gate` forwards the merge job's waivers to diffgate (finding GATE-01: `lagent merge --allow rule=reason`
//! waived the refusals of the merge gate's own diffgate pass, and the `diffgate` step of the fast check then refused those
//! very lines again with "5 refusal(s), 0 waived" — the real log is
//! `/home/nez/scratch/lucina-agents-lanes/fixtures/merge/job_4ee819df2427.log`, its line 9 the allow it was given, its line
//! 160 the refusal).
//!
//! The real binary runs over a throwaway repository whose `tools/ci/merge-waiver-rules.txt` is the real one (the step reads its
//! forwarding list from the repository under test, so what these tests forward is what the owner listed), with a stand-in
//! `diffgate` first on PATH that writes the arguments it was called with, one per line, to a file. What reached diffgate is the
//! proof: the step runs its command through `bash -c`, so the reason has to survive the shell on the way. A run that left no
//! file at all is a run in which diffgate never started, so nothing was waived.
//!
//! Every run here is `--no-build`, which keeps a run to a fraction of a second. The merge gate still refuses the inventory and
//! secret steps of a repository that holds nothing of this crate, so these tests judge the `diffgate` step's own line and never
//! the exit code.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The rules the step forwards, read from the real list: the list is data, and a rule added to it is forwarded by the
/// same code these tests exercise.
const FORWARD_LIST: &str = "tools/ci/merge-waiver-rules.txt";

/// The allow of job_4ee819df2427 line 9, as its coordinator wrote it — the reason the step refused anyway (line 160:
/// `diffgate: 5 refusal(s), 0 waived, 7 files, +1617/-244 lines`).
const GAL_REASON: &str = "real-server variants kept as #[ignore = needs LUCINA_MEDIA_BIN], reported ignored not passed; \
                         the same assertions run against the in-process Fake; the ignored variants run in a scheduled \
                         non-gating job; WM-C004 / GAL-01; coordinator allow 2026-11-17";

struct Case {
    d: PathBuf,
    repo: PathBuf,
    bin: PathBuf,
    /// What the stand-in `diffgate` was called with, or no file at all when it never ran.
    argv: PathBuf,
    waivers: PathBuf,
    base: String,
}

/// `bq` takes `GIT_CONFIG_KEY_0` out of a job's environment because its name holds KEY and leaves `GIT_CONFIG_COUNT` behind,
/// which no git can read: not the one a test runs to build its throwaway repository, and not the one the spawned
/// `check-fast` calls. Every git here runs with the trio gone, whatever the caller's shell carries.
const LANE_GIT_CONFIG: [&str; 3] = ["GIT_CONFIG_COUNT", "GIT_CONFIG_KEY_0", "GIT_CONFIG_VALUE_0"];

fn drop_lane_git_config(c: &mut Command) {
    for n in LANE_GIT_CONFIG {
        c.env_remove(n);
    }
}

/// git, with no inherited config.
fn git(d: &Path, args: &[&str]) -> String {
    let mut c = Command::new("git");
    c.args([
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "-c",
        "commit.gpgsign=false",
    ])
    .args(args)
    .current_dir(d);
    drop_lane_git_config(&mut c);
    let o = c.output().unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

/// The stand-in goes in through a child, never from this thread: it is exec'd by the step, and a file this thread still
/// holds open for writing makes that exec answer "Text file busy" (ETXTBSY). The same helper in
/// tests/merge_gate_allows.rs explains the measurement.
fn write_exec(path: &Path, script: &str) {
    let mut child = Command::new("sh")
        .args(["-c", "cat > \"$1\" && chmod 0755 \"$1\"", "sh"])
        .arg(path)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let mut w = child.stdin.take().unwrap();
    w.write_all(script.as_bytes()).unwrap();
    drop(w);
    assert!(
        child.wait().unwrap().success(),
        "sh cannot write {}: the stand-in is never exec'd",
        path.display()
    );
}

impl Case {
    /// A repository whose `main` is empty and whose `work` branch holds one change and this rules list, and a PATH of stand-in
    /// tools in front of it. `None` copies the repository's own list; `Some` writes a test-only one.
    /// The folder goes when the test ends, panic included.
    fn setup(tag: &str, rules: Option<&[&str]>) -> Case {
        let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("mgw-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        let repo = d.join("repo");
        fs::create_dir_all(repo.join("tools/ci")).unwrap();
        let list = match rules {
            None => fs::read_to_string(repo_root().join(FORWARD_LIST))
                .unwrap_or_else(|e| panic!("{} is not readable from the test: {e}", FORWARD_LIST)),
            Some(rs) => format!(
                "# a test-only list, written by {}\n{}\n",
                tag,
                rs.join("\n")
            ),
        };
        fs::write(repo.join(FORWARD_LIST), list).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(
            &repo,
            &["commit", "-q", "--allow-empty", "-m", "empty base"],
        );
        git(&repo, &["checkout", "-q", "-b", "work"]);
        fs::write(repo.join("NOTES.txt"), "a change\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "the change"]);
        let base = git(&repo, &["rev-parse", "main"]);
        let bin = d.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let (argv, waivers) = (bin.join("diffgate.argv"), d.join("waivers.jsonl"));
        write_exec(
            &bin.join("diffgate"),
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nexit 0\n",
                argv.display()
            ),
        );
        // The secret step of a throwaway repository is not what these tests are about, and gitleaks over a real file
        // costs more than the diffgate step they judge.
        write_exec(&bin.join("gitleaks"), "#!/bin/sh\nexit 0\n");
        Case {
            d,
            repo,
            bin,
            argv,
            waivers,
            base,
        }
    }

    fn write_lines(&self, lines: &[String]) {
        fs::write(&self.waivers, format!("{}\n", lines.join("\n"))).unwrap();
    }

    /// One run of the real step over the fixture. `merge_gate` is `--merge-gate`; `waivers` is `LAGENT_WAIVERS_FILE` and
    /// `allow` `LAGENT_ALLOW`, each taken off the child's environment when the test does not name it — the two sources of a
    /// SecretaryIOS merge are separate findings, and a run that does not name one means it to hold nothing.
    fn gate(&self, merge_gate: bool, waivers: Option<&Path>, allow: Option<&str>) -> String {
        let path = format!(
            "{}:{}",
            self.bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut c = Command::new(env!("CARGO_BIN_EXE_lucina-ci"));
        c.args([
            "check-fast",
            "--base",
            "main",
            "--no-build",
            "--mem-cap-gib",
            "1",
        ])
        .current_dir(&self.repo)
        .env("PATH", path)
        .env_remove("LUCINA_CHECK_FAST_MERGE_CAP_S")
        .env_remove("LAGENT_ALLOW")
        .env_remove("LAGENT_WAIVERS_FILE");
        drop_lane_git_config(&mut c);
        if merge_gate {
            c.arg("--merge-gate");
        }
        if let Some(a) = allow {
            c.env("LAGENT_ALLOW", a);
        }
        if let Some(p) = waivers {
            c.env("LAGENT_WAIVERS_FILE", p);
        }
        let o = c.output().unwrap();
        format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        )
    }

    /// The merge gate, with this waivers file and nothing in `LAGENT_ALLOW`.
    fn run(&self, waivers: Option<&Path>) -> String {
        self.gate(true, waivers, None)
    }

    /// The merge gate with the lead's `LAGENT_ALLOW` entry beside the job's file.
    fn run_with(&self, waivers: Option<&Path>, allow: &str) -> String {
        self.gate(true, waivers, Some(allow))
    }

    /// The step outside `--merge-gate`: what a worker or a lane verify step runs.
    fn run_plain_check(&self, waivers: Option<&Path>) -> String {
        self.gate(false, waivers, None)
    }

    fn argv(&self) -> Option<Vec<String>> {
        fs::read_to_string(&self.argv)
            .ok()
            .map(|t| t.lines().map(str::to_string).collect())
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.d);
    }
}

fn step_line<'a>(out: &'a str, name: &str) -> &'a str {
    out.lines()
        .find(|l| l.split_whitespace().nth(1) == Some(name))
        .unwrap_or_else(|| panic!("no line for step {name}:\n{out}"))
}

/// What the step runs today, with no waiver of any kind.
fn plain(base: &str) -> Vec<String> {
    ["--repo", ".", "--base", base]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// The same call with one `--allow` per entry of `allows`, in order.
fn forwarded(base: &str, allows: &[&str]) -> Vec<String> {
    let mut v = plain(base);
    for a in allows {
        v.push("--allow".into());
        v.push((*a).to_string());
    }
    v
}

/// One waivers line as the merge job writes it (crates/lucina-agentd/src/pipeline/repo.rs `waiver_lines`), the reason
/// JSON-escaped the way its writer escapes it.
fn wline(rule: &str, reason: &str) -> String {
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        "{{\"rule\":\"{}\",\"reason\":\"{}\",\"by\":\"nez@nez_opt\",\"job\":\"job_4ee819df2427\"}}",
        esc(rule),
        esc(reason)
    )
}

/// The repository this test file sits in: the forwarding list is the real one, not a copy written here.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

/// The repo's own forwarding list, as the step reads it.
fn the_list() -> Vec<String> {
    fs::read_to_string(repo_root().join(FORWARD_LIST))
        .unwrap_or_else(|e| panic!("{} is not readable from the test: {e}", FORWARD_LIST))
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect()
}

/// `LAGENT_WAIVERS_FILE` not set — every run that is not a merge with an allow. The command is the plain one, and the
/// output says nothing about waivers.
#[test]
fn no_waivers_file_leaves_the_diffgate_step_exactly_as_it_was() {
    let c = Case::setup("unset", None);
    let out = c.run(None);
    assert_eq!(
        c.argv().expect("diffgate ran"),
        plain(&c.base),
        "the step calls diffgate as it always did"
    );
    assert!(
        step_line(&out, "diffgate").starts_with("ok"),
        "{}",
        step_line(&out, "diffgate")
    );
    assert!(
        !out.contains("waiver") && !out.contains("--allow"),
        "an ordinary run says nothing about waivers:\n{out}"
    );
}

/// The counted form of diffgate (`RULE=N:reason`) reaches it byte for byte through the shell: quotes, the closing bracket
/// of the attribute and the semicolons of the reason are diffgate's to read, not the shell's.
#[test]
fn a_counted_waiver_of_a_listed_rule_reaches_diffgate_byte_for_byte() {
    let c = Case::setup("counted", None);
    let reason = "7:seven tests are #[ignore = \"slow\"]; see a/b";
    c.write_lines(&[wline("skipped-test", reason)]);
    let out = c.run(Some(&c.waivers));
    assert_eq!(
        c.argv().expect("diffgate ran"),
        forwarded(
            &c.base,
            &["skipped-test=7:seven tests are #[ignore = \"slow\"]; see a/b"]
        ),
        "the whole allow is one argument, as written\n{out}"
    );
    assert!(
        out.contains("diffgate waivers: 1 forwarded (skipped-test: 7:seven tests are #[ignore = \"slow\"]; see a/b), 0 not forwarded"),
        "the step says what it forwarded:\n{out}"
    );
}

/// The counted reason the coordinator gave for `tmp-write` (PF-C022): the count, the colons and the parentheses of a
/// sentence that names a rule and a fence are part of the reason, and the rule of the list carries all of it to diffgate's
/// command line unchanged. Its list here is the test's own, so the test holds whatever the repository's list names.
#[test]
fn a_counted_reason_of_colons_and_parentheses_reaches_diffgate_byte_for_byte() {
    let reason = "7:seven sandbox test lines name /tmp on purpose (six assert it is refused or unused, one writes inside \
                  the fence private tmpfs); coordinator allow 2026-11-17";
    let c = Case::setup("counted-colons", Some(&["tmp-write"][..]));
    c.write_lines(&[wline("tmp-write", reason)]);
    let out = c.run(Some(&c.waivers));
    assert_eq!(
        c.argv().expect("diffgate ran"),
        forwarded(
            &c.base,
            &[
                "tmp-write=7:seven sandbox test lines name /tmp on purpose (six assert it is refused or unused, one writes inside the fence private tmpfs); coordinator allow 2026-11-17"
            ]
        ),
        "the reason reaches diffgate as the merge job wrote it\n{out}"
    );
    assert!(
        out.contains(&format!(
            "diffgate waivers: 1 forwarded (tmp-write: {reason}), 0 not forwarded"
        )),
        "the step says what it forwarded:\n{out}"
    );
}

/// The uncounted reason of the merge that failed, line 9 of job_4ee819df2427, forwarded as it stands.
#[test]
fn an_uncounted_waiver_of_a_listed_rule_reaches_diffgate_unchanged() {
    let c = Case::setup("uncounted", None);
    c.write_lines(&[wline("skipped-test", GAL_REASON)]);
    let out = c.run(Some(&c.waivers));
    let want = format!("skipped-test={GAL_REASON}");
    assert_eq!(
        c.argv().expect("diffgate ran"),
        forwarded(&c.base, &[want.as_str()])
    );
    assert!(
        step_line(&out, "diffgate").starts_with("ok"),
        "{}",
        step_line(&out, "diffgate")
    );
}

/// A reason is free text a human typed: an apostrophe, a command substitution and a redirect are content, never the
/// shell's instructions.
#[test]
fn a_reason_holding_quotes_and_a_command_is_one_unchanged_argument() {
    let c = Case::setup("quoted", None);
    let marker = c.d.join("it-ran");
    let reason = format!(
        "it's the coordinator's word; touch '{}'  $(echo hi) `echo there` \"slow\"",
        marker.display()
    );
    c.write_lines(&[wline("skipped-test", &reason)]);
    let out = c.run(Some(&c.waivers));
    let want = format!("skipped-test={reason}");
    assert_eq!(
        c.argv().expect("diffgate ran"),
        forwarded(&c.base, &[want.as_str()]),
        "the reason is one argument, byte for byte:\n{out}"
    );
    assert!(
        !marker.exists(),
        "the reason ran a command through the step's shell:\n{out}"
    );
}

/// Every rule of the list travels: today it holds `skipped-test`, and a rule added there is forwarded by this same path
/// (adding it is a gate change the reviewer reads). The list is read, never copied here, so the test does not have to
/// change when the owner adds a name to it.
#[test]
fn every_rule_of_the_forwarding_list_is_forwarded_to_diffgate() {
    let list = the_list();
    assert!(
        list.iter().any(|r| r == "skipped-test"),
        "{FORWARD_LIST} does not name the rule of the failing merge: {list:?}"
    );
    let c = Case::setup("list", None);
    let lines: Vec<String> = list
        .iter()
        .map(|r| wline(r, "5:five of them; the coordinator allow of today"))
        .collect();
    let allows: Vec<String> = list
        .iter()
        .map(|r| format!("{r}=5:five of them; the coordinator allow of today"))
        .collect();
    c.write_lines(&lines);
    let out = c.run(Some(&c.waivers));
    let mut want = plain(&c.base);
    for a in &allows {
        want.push("--allow".into());
        want.push(a.clone());
    }
    assert_eq!(
        c.argv().expect("diffgate ran"),
        want,
        "one --allow per listed rule\n{out}"
    );
}

/// `gate-change` and `spec-change` are the merge tool's own decisions, not rules diffgate knows: forwarding one would fail
/// the whole call with "--allow: unknown rule", so they stay out of the command and the line says they were left.
#[test]
fn the_merge_tool_s_own_rules_are_not_forwarded() {
    let c = Case::setup("not-forwarded", None);
    c.write_lines(&[
        wline("gate-change", "tools/ci/ of this merge; coordinator allow"),
        wline(
            "spec-change",
            "a statement moved with its proof; coordinator allow",
        ),
    ]);
    let out = c.run(Some(&c.waivers));
    assert_eq!(
        c.argv().expect("diffgate ran"),
        plain(&c.base),
        "diffgate is called as if no allow had been given"
    );
    assert!(
        out.contains("diffgate waivers: 0 forwarded, 2 not forwarded (gate-change, spec-change)"),
        "the step says what it left:\n{out}"
    );
}

/// One line of the gate log, counting both sides.
#[test]
fn the_step_prints_one_line_about_the_waivers_it_was_given() {
    let c = Case::setup("summary", None);
    c.write_lines(&[
        wline("skipped-test", "3:three #[ignore] variants; WM-C004"),
        wline("gate-change", "tools/ci/ of this merge; coordinator allow"),
    ]);
    let out = c.run(Some(&c.waivers));
    let lines: Vec<&str> = out
        .lines()
        .filter(|l| l.starts_with("diffgate waivers: "))
        .collect();
    assert_eq!(
        lines,
        [
            "diffgate waivers: 1 forwarded (skipped-test: 3:three #[ignore] variants; WM-C004), 1 not forwarded (gate-change)"
        ],
        "exactly one line, and it counts both sides:\n{out}"
    );
}

/// A waivers file the step cannot read exactly is a refusal of its own: not a silent "nothing waived", and never a skip.
fn assert_refused(tag: &str, content: &str, says: &str) {
    let c = Case::setup(tag, None);
    fs::write(&c.waivers, content).unwrap();
    let out = c.run(Some(&c.waivers));
    assert!(
        step_line(&out, "diffgate").starts_with("FAIL"),
        "the malformed file was not a refusal: {}\n{out}",
        step_line(&out, "diffgate")
    );
    assert!(out.contains(says), "nothing says {says:?}:\n{out}");
    assert!(
        !out.contains("diffgate waivers: 0 forwarded"),
        "a refusal is not reported as a step that waived nothing of a readable file:\n{out}"
    );
}

#[test]
fn a_waivers_file_that_cannot_be_read_fails_the_step() {
    let c = Case::setup("missing", None);
    let gone = c.d.join("no-such-dir").join("waivers.jsonl");
    let out = c.run(Some(&gone));
    assert!(
        step_line(&out, "diffgate").starts_with("FAIL"),
        "a path that holds no file was not a refusal:\n{out}"
    );
    assert!(
        out.contains("cannot be read"),
        "the message does not say why:\n{out}"
    );
    assert!(
        c.argv().is_none(),
        "diffgate ran on a file that is not there"
    );
}

#[test]
fn an_empty_waivers_file_fails_the_step() {
    assert_refused("empty", "", "holds no waiver line");
}

#[test]
fn a_line_that_is_not_json_fails_the_step() {
    assert_refused(
        "not-json",
        "the reason was written without braces\n",
        "line 1: not a JSON object",
    );
}

#[test]
fn a_line_missing_one_of_the_four_fields_fails_the_step() {
    assert_refused(
        "no-field",
        "{\"rule\":\"skipped-test\",\"reason\":\"why\",\"by\":\"nez@nez_opt\"}\n",
        "no `job` field",
    );
}

#[test]
fn a_field_that_is_not_a_string_fails_the_step() {
    assert_refused(
        "not-a-string",
        "{\"rule\":\"skipped-test\",\"reason\":7,\"by\":\"nez@nez_opt\",\"job\":\"job_4ee819df2427\"}\n",
        "the value of `reason` is not a string",
    );
}

#[test]
fn an_extra_field_fails_the_step() {
    assert_refused(
        "extra-field",
        "{\"rule\":\"skipped-test\",\"reason\":\"why\",\"by\":\"nez@nez_opt\",\"job\":\"job_1\",\"who\":\"me\"}\n",
        "5 fields",
    );
}

#[test]
fn a_blank_reason_fails_the_step() {
    assert_refused(
        "blank-reason",
        "{\"rule\":\"skipped-test\",\"reason\":\"   \",\"by\":\"nez@nez_opt\",\"job\":\"job_1\"}\n",
        "`reason` is empty",
    );
}

/// The point of reading the file at all: a waiver this step cannot read waives NOTHING, not even the sound lines that
/// stand beside it. The good line is the one job_4ee819df2427 was given, so the refusal it would have waived is the
/// difference between this merge and the one that failed.
#[test]
fn one_bad_line_waives_nothing_even_beside_a_good_one() {
    let c = Case::setup("half-bad", None);
    c.write_lines(&[
        wline("skipped-test", GAL_REASON),
        "and then a line the merge job did not write".into(),
    ]);
    let out = c.run(Some(&c.waivers));
    assert!(
        step_line(&out, "diffgate").starts_with("FAIL"),
        "the bad line was not a refusal:\n{out}"
    );
    assert!(
        c.argv().is_none(),
        "diffgate ran and waived from a file this step cannot read:\n{out}"
    );
}

/// A rule that reaches diffgate from the file is reported the way the step reports every waiver it takes: counted on its
/// own line and shown under it.
#[test]
fn a_forwarded_waiver_is_counted_on_the_step_line() {
    let c = Case::setup("counted-line", None);
    c.write_lines(&[wline("skipped-test", "3:three #[ignore] variants; WM-C004")]);
    let out = c.run(Some(&c.waivers));
    assert!(
        step_line(&out, "diffgate").contains("(1 waived)"),
        "{}",
        step_line(&out, "diffgate")
    );
    assert!(
        out.contains("waived: skipped-test = 3:three #[ignore] variants; WM-C004"),
        "the waiver is shown: {out}"
    );
}

/// SecretaryIOS takes the same allows in `LAGENT_ALLOW` as well (its own answer to GATE-01). A rule both sources name
/// travels once: diffgate answers a waiver with the refusals the earlier entry left, so a second copy of the same allow
/// reports itself failed on a diff this merge was allowed to pass.
#[test]
fn a_rule_both_sources_name_reaches_diffgate_once() {
    let c = Case::setup("both-sources", None);
    let reason = "7:seven #[ignore] variants; coordinator allow";
    c.write_lines(&[wline("skipped-test", reason)]);
    let out = c.run_with(Some(&c.waivers), &format!("skipped-test={reason}"));
    assert_eq!(
        c.argv().expect("diffgate ran"),
        forwarded(&c.base, &[&format!("skipped-test={reason}")]),
        "one --allow for the rule, whichever source it came from\n{out}"
    );
    assert!(
        out.contains(&format!(
            "diffgate waivers: 1 forwarded (skipped-test: {reason}), 0 not forwarded"
        )),
        "the file's waiver is counted as forwarded: {out}"
    );
    assert_eq!(
        step_line(&out, "diffgate").matches("(1 waived)").count(),
        1,
        "{}",
        step_line(&out, "diffgate")
    );
}

/// A rule only the file names, beside a rule only the variable names: both travel, in the order the step puts them.
#[test]
fn the_two_sources_of_a_waiver_travel_together() {
    let c = Case::setup("two-sources", None);
    c.write_lines(&[wline("skipped-test", "3:three variants; WM-C004")]);
    let out = c.run_with(Some(&c.waivers), "guard-deleted=the lead checked it");
    assert_eq!(
        c.argv().expect("diffgate ran"),
        forwarded(
            &c.base,
            &[
                "guard-deleted=the lead checked it",
                "skipped-test=3:three variants; WM-C004"
            ]
        ),
        "{out}"
    );
}

/// Outside `--merge-gate` the file waives nothing, as `LAGENT_ALLOW` already does not: a worker or a lane verify step must
/// not be able to waive diffgate for itself by naming a file of its own making.
#[test]
fn outside_the_merge_gate_the_waivers_file_is_ignored_and_says_so() {
    let c = Case::setup("not-merge-gate", None);
    c.write_lines(&[wline("skipped-test", GAL_REASON)]);
    let out = c.run_plain_check(Some(&c.waivers));
    assert!(
        !c.argv().unwrap_or_default().iter().any(|a| a == "--allow"),
        "no waiver outside the merge gate: {out}"
    );
    assert!(
        out.contains("LAGENT_WAIVERS_FILE is set but ignored"),
        "the run says it took nothing from the file:\n{out}"
    );
    assert!(!out.contains("waived:"), "nothing is waived: {out}");
    assert!(
        !out.contains("diffgate waivers:"),
        "a file the step does not read is not reported as forwarded:\n{out}"
    );
}

/// `tmp-write` joins the forwarding list (PF-C022, the coordinator's allow of 2026-11-17): the counted waiver of the
/// seven sandbox test lines that name `/tmp` travels to diffgate exactly as the list holds it, count, colons and
/// parentheses included. This is the entry of `tools/ci/merge-waiver-rules.txt` itself, so the case runs the real list.
#[test]
fn tmp_write_is_on_the_list_and_travels_as_the_list_holds_it() {
    let list = the_list();
    assert!(
        list.iter().any(|r| r == "tmp-write"),
        "{FORWARD_LIST} does not hold tmp-write: {list:?}"
    );
    let reason = "7:seven sandbox test lines name /tmp on purpose (six assert it is refused or unused, one writes inside the fence private tmpfs); coordinator allow 2026-11-17";
    let c = Case::setup("tmp-write", None);
    c.write_lines(&[wline("tmp-write", reason)]);
    let out = c.run(Some(&c.waivers));
    assert_eq!(
        c.argv().expect("diffgate ran"),
        forwarded(&c.base, &[&format!("tmp-write={reason}")]),
        "the whole counted allow, unchanged, as one argument\n{out}"
    );
    assert!(
        out.contains(&format!(
            "diffgate waivers: 1 forwarded (tmp-write: {reason}), 0 not forwarded"
        )),
        "the step says it forwarded it:\n{out}"
    );
}
