//! T-0083 sync slice: ROADMAP §3.8's owner case, driven end to end on two
//! isolated roots that stand in for two machines.
//!
//! One sentence: `cargo xtask sync --check` edits one `opencode.jsonc` custom
//! provider once on root A, syncs it to root B, and asserts — stage by stage,
//! one pass/fail line each — that both roots converge, that each resolves the
//! reference from its own store, that the root without the variable says so **by
//! name**, that a concurrent edit keeps both copies, and that a broken provider
//! list is one `revert` away from being right again.
//!
//! ## Why two roots and not two daemons
//!
//! The transport is T-0086's (deltas over the mesh). What this ticket owns is
//! the mechanism, and the mechanism's hard parts are all local: which files may
//! travel, where they live on *this* machine, what a payload carries, who wins a
//! concurrent edit, and where the secret comes from. Two roots in one scratch
//! directory exercise every one of those with the *shipped* binaries — the
//! `arreo` CLI in a per-root environment — and the only thing missing is the
//! socket T-0086 will put between them.
//!
//! ## What it never touches
//!
//! The user's own configs. Every path is under `target/test-scratch/T-0083/`,
//! each root's `XDG_CONFIG_HOME` and `PI_CODING_AGENT_DIR` point inside it, and
//! the daemon is never started. The provider key is a dummy value written by the
//! slice itself.
//!
//! ## The injection stage, and why it uses a pty
//!
//! `docs/harness-centralization.md` §3.2: the value is injected when Arreo
//! spawns the harness, so the stage that proves it spawns one. xtask already
//! owns a pty (the same `portable-pty` backend the product uses), the
//! environment comes from the shipped `keychain::plan`, and the child prints
//! only the *name's* value — which is how a stage asserts that alpha's harness
//! gets alpha's key and beta's gets beta's, from one identical file.

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use arreo_core::sync::keychain::{plan, SecretStore};

const ALPHA: &str = "alpha-machine";
const BETA: &str = "beta-machine";
/// The variable name the worked case uses. A dummy — the same name
/// `docs/harness-centralization.md` uses — and no real key is ever involved.
const KEY: &str = "VBK_PROD_KEY";
const ALPHA_VALUE: &str = "alpha-key-value-3f19";
const BETA_VALUE: &str = "beta-key-value-8a02";

/// The owner's file (`docs/harness-centralization.md` §4 step 1), verbatim: one
/// custom provider whose key is a reference, everything else portable intent.
const WORKED_CASE: &str = r#"{
  "$schema": "https://opencode.ai/config.json",
  "provider": {
    "verboo": {
      "name": "Verboo Code",
      "npm": "@ai-sdk/openai-compatible",
      "options": {
        "baseURL": "https://code.verboo.ai/router/v1",
        "apiKey": "{env:VBK_PROD_KEY}"
      },
      "models": {
        "deepseek-v4-flash-0731": {
          "id": "deepseek-v4-flash-0731",
          "name": "Verboo deepseek-v4-flash-0731",
          "tool_call": true,
          "interleaved": "reasoning_content",
          "modalities": { "input": ["text"], "output": ["text"] },
          "limit": { "context": 1048576, "output": 65536 }
        }
      }
    }
  }
}
"#;

/// One stand-in machine: its roots, its store, its secrets, its CLI calls.
struct Root {
    name: &'static str,
    dir: PathBuf,
    cli: PathBuf,
}

impl Root {
    fn cfg(&self) -> PathBuf {
        self.dir.join("cfg")
    }

    fn store(&self) -> PathBuf {
        self.dir.join("store.db")
    }

    fn opencode(&self) -> PathBuf {
        self.cfg().join("opencode/opencode.jsonc")
    }

    fn secrets(&self) -> PathBuf {
        self.cfg().join("arreo/secrets.json")
    }

    /// Run the shipped CLI in this root's environment.
    ///
    /// The environment *is* the machine: `XDG_CONFIG_HOME` and
    /// `PI_CODING_AGENT_DIR` are what a preset's symbolic path resolves against,
    /// and `env_remove(KEY)` is what makes "this machine does not have the
    /// variable" true rather than inherited from whoever ran the slice.
    fn cli(&self, args: &[&str]) -> (bool, String) {
        self.cli_with_stdin(args, None)
    }

    fn cli_with_stdin(&self, args: &[&str], stdin: Option<&str>) -> (bool, String) {
        let mut command = Command::new(&self.cli);
        command
            .arg("sync")
            .args(args)
            .arg("--machine")
            .arg(self.name)
            .arg("--store")
            .arg(self.store())
            .env("HOME", self.dir.join("home"))
            .env("XDG_CONFIG_HOME", self.cfg())
            .env("XDG_DATA_HOME", self.dir.join("data"))
            .env("XDG_STATE_HOME", self.dir.join("state"))
            .env("XDG_CACHE_HOME", self.dir.join("cache"))
            .env("PI_CODING_AGENT_DIR", self.dir.join("agent"))
            .env_remove(KEY)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command.stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(e) => return (false, format!("spawn failed: {e}")),
        };
        if let Some(text) = stdin {
            if let Some(mut pipe) = child.stdin.take() {
                let _ = pipe.write_all(text.as_bytes());
            }
        }
        match child.wait_with_output() {
            Ok(output) => {
                let mut text = String::from_utf8_lossy(&output.stdout).to_string();
                text.push_str(&String::from_utf8_lossy(&output.stderr));
                (output.status.success(), text)
            }
            Err(e) => (false, format!("wait failed: {e}")),
        }
    }

    fn write(&self, path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("root dir");
        }
        std::fs::write(path, content).expect("write");
    }

    fn read(&self, path: &Path) -> String {
        std::fs::read_to_string(path).unwrap_or_default()
    }

    /// The environment the shipped keychain bridge computes for this root.
    fn injection(&self, content: &str) -> Vec<(String, String)> {
        let secrets = SecretStore::open(self.secrets()).expect("secrets");
        plan(content, &secrets, self.name).environment()
    }
}

/// A stage's pass/fail, printed and (optionally) kept as evidence.
struct Stages {
    passed: usize,
    failed: usize,
    evidence: Option<std::fs::File>,
}

impl Stages {
    fn check(&mut self, stage: &str, ok: bool, detail: &str) {
        let line = format!("{} {stage}: {detail}", if ok { "PASS" } else { "FAIL" });
        println!("{line}");
        if let Some(file) = &mut self.evidence {
            let _ = writeln!(file, "{line}");
        }
        if ok {
            self.passed += 1;
        } else {
            self.failed += 1;
        }
    }
}

pub fn run(rest: &[String]) -> ExitCode {
    if rest.iter().any(|a| a == "--help" || a == "-h") {
        println!("usage: cargo xtask sync --check [--evidence PATH]");
        return ExitCode::SUCCESS;
    }
    if !rest.iter().any(|a| a == "--check") {
        eprintln!("xtask sync: only --check exists (the transport half is T-0086)");
        return ExitCode::from(2);
    }
    let evidence = rest
        .windows(2)
        .find(|w| w[0] == "--evidence")
        .map(|w| PathBuf::from(&w[1]));
    let scratch = scratch_root().join("check");
    let _ = std::fs::remove_dir_all(&scratch);
    for dir in ["a", "b"] {
        for part in ["cfg", "home", "agent", "data", "state", "cache"] {
            if let Err(e) = std::fs::create_dir_all(scratch.join(dir).join(part)) {
                eprintln!("xtask sync: cannot create the isolated roots: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    let (_, cli, _) = crate::harness::bins();
    let alpha = Root {
        name: ALPHA,
        dir: scratch.join("a"),
        cli: cli.clone(),
    };
    let beta = Root {
        name: BETA,
        dir: scratch.join("b"),
        cli,
    };
    let mut stages = Stages {
        passed: 0,
        failed: 0,
        evidence: evidence.as_ref().and_then(|path| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .ok()
        }),
    };
    if let Some(file) = &mut stages.evidence {
        let _ = writeln!(
            file,
            "\n=== cargo xtask sync --check — roots under {}, run at {} ===",
            scratch.display(),
            chrono_free_stamp()
        );
    }
    println!(
        "xtask sync --check: two isolated roots under {} (no daemon, no network)",
        scratch.display()
    );

    stage_edit_and_push(&alpha, &beta, &mut stages);
    stage_local_refusals(&alpha, &mut stages);
    stage_conflict(&alpha, &beta, &mut stages);
    stage_sibling(&alpha, &beta, &mut stages);
    stage_comments(&alpha, &beta, &mut stages);
    stage_revert(&alpha, &beta, &mut stages);

    println!(
        "xtask sync --check: {} passed, {} failed{}",
        stages.passed,
        stages.failed,
        if stages.failed == 0 {
            ""
        } else {
            "  <-- see FAIL lines"
        }
    );
    if stages.failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Stages 1–7: the §3.8 worked case — edit once, sync, converge, resolve
/// locally, say so by name when the variable is absent, inject at spawn.
fn stage_edit_and_push(alpha: &Root, beta: &Root, stages: &mut Stages) {
    // 1. The edit, on alpha only.
    alpha.write(&alpha.opencode(), WORKED_CASE);
    stages.check(
        "edit",
        alpha.opencode().exists() && !beta.opencode().exists(),
        &format!(
            "wrote {} on {}; {} has no such file",
            alpha.opencode().display(),
            ALPHA,
            BETA
        ),
    );

    // 2. The presets decide. The syncable file is listed; the credential store,
    //    the session database and the sibling extension are not candidates at all.
    let (ok, listing) = alpha.cli(&["list", "--json"]);
    let listed = |name: &str| listing.contains(&format!("\"file\":\"{name}\""));
    stages.check(
        "presets",
        ok && listed("opencode.jsonc")
            && listed("models.json")
            && listed("config.yml")
            && !listed("auth.json")
            && !listed("opencode.db")
            && !listed("opencode.json"),
        &format!(
            "list --json: opencode.jsonc/models.json/config.yml are SYNC and auth.json/opencode.db/\
             opencode.json are not (exit={ok})"
        ),
    );

    // Step 0 of the design, once per machine and never synced: each machine
    // stores the provider key in its own store. Alpha's is here; beta's absence
    // is the next stage's whole point.
    alpha.cli_with_stdin(&["secret", "set", KEY], Some(ALPHA_VALUE));

    // 3. The push: validated, counted, recorded.
    let (ok, out) = alpha.cli(&["push", "opencode.jsonc"]);
    stages.check("push", ok && out.contains("revision 1"), &one_line(&out));

    // 4. The payload: neutral references, never the value.
    let payload_path = alpha.dir.join("payload-1.json");
    let (ok, out) = alpha.cli(&[
        "payload",
        "opencode.jsonc",
        "--out",
        payload_path.to_str().expect("utf8"),
    ]);
    let payload = alpha.read(&payload_path);
    let detail = if ok {
        format!(
            "{} carries ${{ARREO_ENV:{KEY}}} and no value",
            payload_path.display()
        )
    } else {
        format!("payload failed: {}", one_line(&out))
    };
    stages.check(
        "payload-neutral",
        ok && payload.contains("${ARREO_ENV:VBK_PROD_KEY}")
            && !payload.contains(ALPHA_VALUE)
            && !payload.contains("alpha-key-value"),
        &detail,
    );

    // 5. Beta has the reference but not the variable, and refuses **by name**.
    let (ok, out) = beta.cli(&["apply", payload_path.to_str().expect("utf8")]);
    let named = out.contains(&format!("{KEY} is not set on {BETA}"));
    let detail = if named {
        format!("{BETA}: {}", one_line(&out))
    } else {
        format!(
            "missing the by-name refusal; output was: {}",
            one_line(&out)
        )
    };
    stages.check(
        "refused-by-name",
        !ok && named && !beta.opencode().exists(),
        &detail,
    );

    // 6. Beta supplies its own value; the same payload now converges.
    beta.cli_with_stdin(&["secret", "set", KEY], Some(BETA_VALUE));
    let (ok, out) = beta.cli(&["apply", payload_path.to_str().expect("utf8")]);
    let identical = alpha.read(&alpha.opencode()) == beta.read(&beta.opencode());
    let (_, alpha_history) = alpha.cli(&["history", "opencode.jsonc", "--json"]);
    let (_, beta_history) = beta.cli(&["history", "opencode.jsonc", "--json"]);
    let one_revision = |text: &str| text.matches("\"id\":").count() == 1;
    stages.check(
        "converged",
        ok && identical
            && one_revision(&alpha_history)
            && one_revision(&beta_history)
            && beta.opencode().exists(),
        &format!(
            "both roots hold byte-identical files ({} bytes), one revision each; {}",
            beta.read(&beta.opencode()).len(),
            one_line(&out)
        ),
    );

    // 7. Each root resolves the reference from its own store, and the harness it
    //    spawns sees its own machine's value. This is the keychain bridge: one
    //    file, two environments.
    let content = alpha.read(&alpha.opencode());
    let alpha_env = alpha.injection(&content);
    let beta_env = beta.injection(&content);
    let alpha_seen = spawn_with_env(&alpha_env);
    let beta_seen = spawn_with_env(&beta_env);
    let inherited = spawn_with_env(&[]);
    stages.check(
        "injected",
        alpha_env.len() == 1
            && beta_env.len() == 1
            && alpha_seen == ALPHA_VALUE
            && beta_seen == BETA_VALUE
            && alpha_seen != beta_seen
            && inherited == "UNSET",
        &format!(
            "pty child under {ALPHA} sees {}'s key, under {BETA} sees {}'s; with no injection it \
             sees {inherited}",
            if alpha_seen == ALPHA_VALUE {
                ALPHA
            } else {
                "??"
            },
            if beta_seen == BETA_VALUE { BETA } else { "??" }
        ),
    );
}

/// Stage 8: the LOCAL class, refused before anything is read or scanned.
fn stage_local_refusals(alpha: &Root, stages: &mut Stages) {
    // `auth.json` does not exist on alpha. A refusal that ran the read first
    // would say "no such file"; the class check comes first, so it says what the
    // file *is*.
    let (ok, out) = alpha.cli(&["push", "auth.json"]);
    let by_class = out.contains("not syncable") && !out.contains("no such file");
    // The same for a credential file no preset knows, by the deny-list: the
    // content is clean, so nothing but the list can refuse it.
    let custom = alpha.dir.join("cfg/arreo/credentials.json");
    alpha.write(&custom, "{\n  \"apiKey\": \"{env:VBK_PROD_KEY}\"\n}\n");
    let (ok_custom, out_custom) = alpha.cli(&["push", custom.to_str().expect("utf8")]);
    stages.check(
        "local-refused",
        !ok && by_class && !ok_custom && out_custom.contains("not syncable"),
        &format!(
            "auth.json: {} | credentials.json: {}",
            one_line(&out),
            one_line(&out_custom)
        ),
    );
}

/// Stages 9–10: both machines edit, both copies are kept, and the merge unions
/// what can be unioned.
fn stage_conflict(alpha: &Root, beta: &Root, stages: &mut Stages) {
    alpha.write(
        &alpha.opencode(),
        &WORKED_CASE.replace(
            "\"provider\": {",
            "\"plugin\": [\"alpha.js\"],\n  \"provider\": {\n    \"alphaOnly\": { \"name\": \"Alpha\" },",
        ),
    );
    let (payload_ok, _) = alpha.cli(&[
        "payload",
        "opencode.jsonc",
        "--out",
        alpha.dir.join("payload-2.json").to_str().expect("utf8"),
    ]);
    let payload = alpha.dir.join("payload-2.json");
    beta.write(
        &beta.opencode(),
        &WORKED_CASE.replace(
            "\"provider\": {",
            "\"plugin\": [\"beta.js\"],\n  \"provider\": {\n    \"betaOnly\": { \"name\": \"Beta\" },",
        ),
    );
    beta.cli(&["push", "opencode.jsonc"]);
    let beta_before = beta.read(&beta.opencode());
    let (apply_ok, apply_out) = beta.cli(&["apply", payload.to_str().expect("utf8")]);
    let conflict_copy = std::fs::read_dir(beta.opencode().parent().expect("dir"))
        .expect("dir")
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().to_string())
        .find(|name| name.starts_with(&format!("opencode.conflict-{ALPHA}-")))
        .unwrap_or_default();
    stages.check(
        "conflict-keep-both",
        payload_ok
            && apply_ok
            && apply_out.contains("both kept")
            && !conflict_copy.is_empty()
            && beta.read(&beta.opencode()) == beta_before
            && beta.read(&beta.opencode()).contains("betaOnly")
            && !beta.read(&beta.opencode()).contains("alphaOnly"),
        &format!(
            "{} kept {conflict_copy}; the live file still holds this machine's edit; {}",
            BETA,
            one_line(&apply_out)
        ),
    );

    let (merge_ok, merge_out) = beta.cli(&["merge", "opencode.jsonc"]);
    let merged = beta.read(&beta.opencode());
    let union_plugins = merged.contains("\"alpha.js\"") && merged.contains("\"beta.js\"");
    let both_providers = merged.contains("alphaOnly") && merged.contains("betaOnly");
    stages.check(
        "merge-union",
        merge_ok && union_plugins && both_providers,
        &format!(
            "provider objects and the plugin array both unioned ({})",
            one_line(&merge_out)
        ),
    );
}

/// Stage 11: the sibling extension — the hazard T-0075 verified — refuses a
/// write, and the merge reconciles it.
fn stage_sibling(alpha: &Root, beta: &Root, stages: &mut Stages) {
    let sibling = beta.cfg().join("opencode/opencode.json");
    beta.write(
        &sibling,
        "{\n  \"$schema\": \"https://opencode.ai/config.json\",\n  \"provider\": {\n    \
         \"siblingOnly\": { \"name\": \"Sibling\" }\n  }\n}\n",
    );
    alpha.write(
        &alpha.opencode(),
        &alpha
            .read(&alpha.opencode())
            .replace("\"Verboo Code\"", "\"Verboo Code v2\""),
    );
    alpha.cli(&["push", "opencode.jsonc"]);
    let payload = alpha.dir.join("payload-3.json");
    alpha.cli(&[
        "payload",
        "opencode.jsonc",
        "--out",
        payload.to_str().expect("utf8"),
    ]);
    let (apply_ok, apply_out) = beta.cli(&["apply", payload.to_str().expect("utf8")]);
    let refused =
        !apply_ok && apply_out.contains("opencode.json") && apply_out.contains("arreo sync merge");
    let (merge_ok, merge_out) = beta.cli(&["merge", "opencode.jsonc"]);
    let reconciled = std::fs::read_dir(sibling.parent().expect("dir"))
        .expect("dir")
        .filter_map(Result::ok)
        .any(|e| e.file_name().to_string_lossy().contains("reconciled"));
    stages.check(
        "sibling-reconcile",
        refused
            && merge_ok
            && !sibling.exists()
            && reconciled
            && beta.read(&beta.opencode()).contains("siblingOnly"),
        &format!(
            "write refused while the sibling existed ({}); merge folded it in and renamed it aside \
             ({} )",
            one_line(&apply_out),
            one_line(&merge_out)
        ),
    );
}

/// Stage 12: JSONC comments survive a copy and refuse a merge — never a silent
/// reformat.
fn stage_comments(alpha: &Root, beta: &Root, stages: &mut Stages) {
    let commented = format!("// the owner's note about the provider\n{}", WORKED_CASE);
    beta.write(&beta.opencode(), &commented);
    beta.cli(&["push", "opencode.jsonc"]);
    alpha.write(
        &alpha.opencode(),
        &alpha
            .read(&alpha.opencode())
            .replace("\"Verboo Code v2\"", "\"Verboo Code v3\""),
    );
    alpha.cli(&["push", "opencode.jsonc"]);
    let payload = alpha.dir.join("payload-4.json");
    alpha.cli(&[
        "payload",
        "opencode.jsonc",
        "--out",
        payload.to_str().expect("utf8"),
    ]);
    let (apply_ok, apply_out) = beta.cli(&["apply", payload.to_str().expect("utf8")]);
    let (merge_ok, merge_out) = beta.cli(&["merge", "opencode.jsonc"]);
    let kept = beta.read(&beta.opencode()).contains("the owner's note");
    stages.check(
        "comments-refuse",
        apply_ok
            && apply_out.contains("both kept")
            && !merge_ok
            && merge_out.contains("comments")
            && kept,
        &format!(
            "conflict kept both ({}); merge refused, naming the comments ({}); the note is still in \
             the file",
            one_line(&apply_out),
            one_line(&merge_out)
        ),
    );
}

/// Stages 13–14: the undo, and the idempotent push.
fn stage_revert(alpha: &Root, beta: &Root, stages: &mut Stages) {
    let first = "{\n  \"theme\": \"dark\"\n}\n";
    let second = "{\n  \"theme\": \"light\"\n}\n";
    let tui = alpha.cfg().join("opencode/tui.jsonc");
    alpha.write(&tui, first);
    alpha.cli(&["push", "tui.jsonc"]);
    alpha.cli(&[
        "payload",
        "tui.jsonc",
        "--out",
        alpha.dir.join("tui-1.json").to_str().expect("utf8"),
    ]);
    beta.cli(&[
        "apply",
        alpha.dir.join("tui-1.json").to_str().expect("utf8"),
    ]);
    alpha.write(&tui, second);
    alpha.cli(&["push", "tui.jsonc"]);
    alpha.cli(&[
        "payload",
        "tui.jsonc",
        "--out",
        alpha.dir.join("tui-2.json").to_str().expect("utf8"),
    ]);
    let (converged_ok, _) = beta.cli(&[
        "apply",
        alpha.dir.join("tui-2.json").to_str().expect("utf8"),
    ]);
    let broke = beta.read(&beta.cfg().join("opencode/tui.jsonc")) == second;

    // The 3 a.m. command.
    let (revert_ok, revert_out) = alpha.cli(&["revert", "tui.jsonc"]);
    let restored = alpha.read(&tui) == first;
    alpha.cli(&[
        "payload",
        "tui.jsonc",
        "--out",
        alpha.dir.join("tui-3.json").to_str().expect("utf8"),
    ]);
    let (back_ok, _) = beta.cli(&[
        "apply",
        alpha.dir.join("tui-3.json").to_str().expect("utf8"),
    ]);
    let (unchanged_ok, unchanged_out) = alpha.cli(&["push", "tui.jsonc"]);
    stages.check(
        "revert",
        converged_ok
            && broke
            && revert_ok
            && restored
            && back_ok
            && beta.read(&beta.cfg().join("opencode/tui.jsonc")) == first,
        &format!(
            "revert restored the first revision on {ALPHA} and it propagated to {BETA} ({}); the \
             next push reports: {}",
            one_line(&revert_out),
            one_line(&unchanged_out)
        ),
    );
    stages.check(
        "push-idempotent",
        unchanged_ok && unchanged_out.contains("unchanged"),
        &format!(
            "a second push of the same bytes: {}",
            one_line(&unchanged_out)
        ),
    );
}

/// Spawn a pty child with exactly `env` as the environment's additions, and
/// return what it printed for `$KEY` — or `UNSET`.
///
/// A pty, not a pipe, because the injection point is the harness spawn
/// (`docs/harness-centralization.md` §3.2: Arreo owns the pty). The child's
/// parent here is xtask, so an empty `env` printing `UNSET` also proves the
/// variable is not inherited from whoever ran the slice.
fn spawn_with_env(env: &[(String, String)]) -> String {
    let pty = native_pty_system();
    let pair = match pty.openpty(PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    }) {
        Ok(pair) => pair,
        Err(e) => return format!("openpty failed: {e}"),
    };
    let mut command = CommandBuilder::new("/bin/sh");
    command.arg("-c");
    command.arg(format!("printf %s \"${{{KEY}:-UNSET}}\""));
    for (name, value) in env {
        command.env(name, value);
    }
    let mut child = match pair.slave.spawn_command(command) {
        Ok(child) => child,
        Err(e) => return format!("spawn failed: {e}"),
    };
    // The slave handle is closed in the parent so the master can see EOF once
    // the child exits; the master stays alive (it is the pty) until the read is
    // done.
    drop(pair.slave);
    let mut reader = match pair.master.try_clone_reader() {
        Ok(reader) => reader,
        Err(e) => return format!("reader failed: {e}"),
    };
    // Wait for the child first, so the read below cannot block: the pty buffers
    // what was written, and a child that does not exit inside the deadline is
    // killed rather than hanging the slice.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut exited = false;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => {
                exited = true;
                break;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(_) => break,
        }
    }
    if !exited {
        let _ = child.kill();
    }
    let mut text = String::new();
    let mut buffer = [0_u8; 256];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => text.push_str(&String::from_utf8_lossy(&buffer[..n])),
            // EIO after the child exits is how a pty master says "no more".
            Err(_) => break,
        }
    }
    // The pty echoes nothing here (the child only writes), so the output is the
    // value itself; a stray CR would break the comparison, so trim it.
    text.trim_end_matches(['\r', '\n']).to_string()
}

/// One line for a stage detail, so evidence stays greppable.
fn one_line(text: &str) -> String {
    let joined = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" / ");
    if joined.len() > 220 {
        format!("{}…", &joined[..220])
    } else {
        joined
    }
}

fn scratch_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level below the workspace root")
        .join("target")
        .join("test-scratch")
        .join("T-0083")
}

/// A stamp for the evidence file, without pulling a date library into xtask.
///
/// Seconds since the epoch: enough to tell one run from the next, and the
/// human form is in the CLI traces beside it (`arreo sync history` prints one).
fn chrono_free_stamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("unix {now}s")
}
