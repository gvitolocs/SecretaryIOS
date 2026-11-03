//! Files and `.env` files as sources: `file:<path>` for one secret, `env:<path>` for several, and a
//! project's list of what it needs (design: docs/design/secret-keep.md).
//!
//! Plain parsing wherever it is exact; the local model only for a file holding more than its value.
//! Lucina also reads untrusted chats and web pages, so any path can be asked for. A path must pass
//! the check in [`lucina_secrets::source_path`] as asked and again with its links resolved, then
//! opens with `RESOLVE_NO_SYMLINKS`; the open file must be regular, the user's, with one link
//! (a hard link would carry `~/.ssh/id_ed25519` into a project folder), small and text.
//!
//! A source file stays unless `clear` was asked, and is cleared only after the read-back matches
//! and nothing is seen using it. A clear asked through the chat tool waits for the owner's answer.

use std::collections::HashSet;
use std::ffi::CString;
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use lucina_secrets::envfile::{self, EnvValue};
use lucina_secrets::source_path::{self, Rules};
use lucina_secrets::SecretValue;
use serde_json::{json, Value};
use zeroize::Zeroizing;

use super::{check_verbatim, local, local_stop, stop, Cleared, Job, Keeper, KeepConfig, KeyMove, State, Stop, MAX_VALUE_BYTES, MIN_VALUE_CHARS};
use crate::api::ApiError;

/// A source bigger than this is not a credential file.
pub const MAX_FILE_BYTES: usize = MAX_VALUE_BYTES;
/// Most keys one env job takes by name.
pub const MAX_KEYS: usize = 200;
/// A value shorter than this isn't registered with the redactor: `postgres` would mask a word.
const MIN_REDACTOR_CHARS: usize = 16;

/// The answers to a clear that came through the chat tool. Only his own choice of the first clears.
const CLEAR_OPTION: &str = "Clear it";
const LEAVE_OPTION: &str = "Leave it";

/// Where a project lists what it needs. Names only are read from these.
const EXAMPLE_FILES: &[&str] = &[".env.example", ".env.sample", ".env.template", ".env.dist", "env.example", "example.env", ".env.defaults"];
/// Compose files name what they need as `${NAME}`.
const COMPOSE_FILES: &[&str] = &["docker-compose.yml", "docker-compose.yaml", "compose.yml", "compose.yaml"];
/// Where values are. The first with a real value wins.
const VALUE_FILES: &[&str] = &[".env", ".env.local", ".env.production"];

fn refused() -> ApiError {
    // The same words for every reason: an injected request learns nothing about the disk.
    ApiError::new(403, "path_refused", "that path is not one I may read")
}

/// `~` and `~/x` only. The rest of the path is kept as asked, so `~//x` still fails as not clean.
pub fn expand(raw: &str, home: &Path) -> Option<PathBuf> {
    let raw = raw.trim();
    if let Some(rest) = raw.strip_prefix("~/") {
        return Some(PathBuf::from(format!("{}/{rest}", home.display())));
    }
    if raw.starts_with('~') {
        return None;
    }
    Some(PathBuf::from(raw))
}

fn bytes_of(p: &Path) -> Vec<u8> {
    p.as_os_str().as_bytes().to_vec()
}

fn canon_all(paths: &[PathBuf]) -> Vec<Vec<u8>> {
    paths.iter().filter_map(|p| std::fs::canonicalize(p).ok()).map(|p| bytes_of(&p)).collect()
}

/// What a path may be as asked: below a root as configured, or as it really is.
fn asked_rules(cfg: &KeepConfig) -> Rules {
    let mut roots: Vec<Vec<u8>> = cfg.source_roots.iter().map(|p| bytes_of(p)).collect();
    roots.extend(canon_all(&cfg.source_roots));
    Rules::new(roots, deny_trees(cfg))
}

/// What a resolved path may be: below a root as it really is.
fn resolved_rules(cfg: &KeepConfig) -> Rules {
    Rules::new(canon_all(&cfg.source_roots), deny_trees(cfg))
}

fn deny_trees(cfg: &KeepConfig) -> Vec<Vec<u8>> {
    let mut deny: Vec<Vec<u8>> = cfg.source_deny.iter().map(|p| bytes_of(p)).collect();
    deny.extend(canon_all(&cfg.source_deny));
    deny
}

/// The first check, before a job exists. Refuses without looking at the disk's contents.
pub fn check_asked(raw: &str, cfg: &KeepConfig) -> Result<PathBuf, ApiError> {
    let path = expand(raw, &cfg.home).ok_or_else(refused)?;
    if !source_path::allowed(path.as_os_str().as_bytes(), &asked_rules(cfg)) {
        tracing::warn!(code = "path_refused", "a source path was refused");
        return Err(refused());
    }
    Ok(path)
}

/// The same file, by what the kernel says: for noticing it changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Identity {
    dev: u64,
    ino: u64,
    mtime: i64,
    mtime_nsec: i64,
    size: u64,
    mode: u32,
}

impl Identity {
    fn of(m: &std::fs::Metadata) -> Self {
        Self { dev: m.dev(), ino: m.ino(), mtime: m.mtime(), mtime_nsec: m.mtime_nsec(), size: m.size(), mode: m.mode() }
    }
}

/// Is this open file one the keeper may read? Pure, so a file of another owner can be tested.
pub fn check_meta(regular: bool, uid: u32, euid: u32, nlink: u64, size: u64) -> Result<(), &'static str> {
    if !regular {
        return Err("not_a_file");
    }
    if uid != euid {
        return Err("not_owner");
    }
    if nlink != 1 {
        return Err("hard_linked");
    }
    if size > MAX_FILE_BYTES as u64 {
        return Err("too_long");
    }
    Ok(())
}

/// `open` that fails if any part of the path is a link, so a swap after the resolve finds nothing.
fn open_no_links(path: &Path, flags: i32) -> std::io::Result<std::fs::File> {
    use std::os::fd::FromRawFd;
    let c = CString::new(path.as_os_str().as_bytes()).map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    // SAFETY: open_how is plain data, zeroed is its documented default.
    let mut how: libc::open_how = unsafe { std::mem::zeroed() };
    how.flags = (flags | libc::O_CLOEXEC | libc::O_NONBLOCK | libc::O_NOCTTY) as u64;
    how.resolve = libc::RESOLVE_NO_SYMLINKS | libc::RESOLVE_NO_MAGICLINKS;
    // SAFETY: `c` and `how` outlive the call, and the size passed is `how`'s.
    let fd = unsafe { libc::syscall(libc::SYS_openat2, libc::AT_FDCWD, c.as_ptr(), &how as *const libc::open_how, std::mem::size_of::<libc::open_how>()) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: the kernel just handed us this descriptor and nothing else owns it.
    Ok(unsafe { std::fs::File::from_raw_fd(fd as i32) })
}

pub(super) struct Opened {
    pub text: Zeroizing<String>,
    pub id: Identity,
    /// With its links resolved.
    pub path: PathBuf,
}

/// Every read of a source goes through here.
pub(super) fn read_source(asked: &Path, cfg: &KeepConfig) -> Result<Opened, Stop> {
    let bad = || stop("path_refused");
    if !source_path::allowed(asked.as_os_str().as_bytes(), &asked_rules(cfg)) {
        return Err(bad());
    }
    let path = std::fs::canonicalize(asked).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => stop("source_not_found"),
        _ => stop("source_unreadable"),
    })?;
    if !source_path::allowed(path.as_os_str().as_bytes(), &resolved_rules(cfg)) {
        return Err(bad());
    }
    let mut file = open_no_links(&path, libc::O_RDONLY).map_err(|e| match e.raw_os_error() {
        Some(libc::ELOOP) | Some(libc::EXDEV) => bad(),
        Some(libc::ENOENT) => stop("source_not_found"),
        _ => stop("source_unreadable"),
    })?;
    let m = file.metadata().map_err(|_| stop("source_unreadable"))?;
    // SAFETY: geteuid has no preconditions.
    let euid = unsafe { libc::geteuid() };
    check_meta(m.is_file(), m.uid(), euid, m.nlink(), m.size()).map_err(stop)?;
    // Room for the limit up front: a growing buffer would leave old copies behind.
    let mut bytes = Zeroizing::new(Vec::with_capacity(MAX_FILE_BYTES + 1));
    (&mut file).take(MAX_FILE_BYTES as u64 + 1).read_to_end(&mut bytes).map_err(|_| stop("source_unreadable"))?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(stop("too_long"));
    }
    if bytes.contains(&0) || std::str::from_utf8(&bytes).is_err() {
        return Err(stop("binary"));
    }
    let text = String::from_utf8(std::mem::take(&mut *bytes)).map_err(|_| stop("binary"))?;
    Ok(Opened { text: Zeroizing::new(text), id: Identity::of(&m), path })
}

/// A file that is one value: a single line, or one PEM block.
pub fn single_value(text: &str) -> bool {
    let t = text.trim();
    !t.is_empty() && (t.lines().count() == 1 || (t.starts_with("-----BEGIN ") && t.ends_with("-----") && t.matches("-----BEGIN ").count() == 1))
}

fn usable(value: &str) -> bool {
    value.chars().count() >= MIN_VALUE_CHARS && !value.contains("[secret:")
}

/// `KEEP_<FOLDER>_`: names the default place's grant allows, which still say whose they are.
pub fn default_prefix(path: &Path) -> String {
    let folder = path.parent().and_then(Path::file_name).map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    let mut out = String::from("KEEP_");
    for c in folder.chars() {
        out.push(if c.is_ascii_alphanumeric() { c.to_ascii_uppercase() } else { '_' });
    }
    out.push('_');
    out
}

/// What the name of a key becomes.
fn stored_name(prefix: &str, key: &str) -> String {
    if prefix.is_empty() || key.starts_with(prefix) {
        key.to_owned()
    } else {
        format!("{prefix}{key}")
    }
}

/// One deposit that succeeded: what it was stored as.
struct Stored {
    moves: Vec<KeyMove>,
    id: Identity,
    path: PathBuf,
}

impl Stored {
    fn saved(&self) -> Vec<&KeyMove> {
        self.moves.iter().filter(|m| m.state == "saved").collect()
    }
}

/// What the file or its folder is used by. `failed`: it could not be checked, which counts as in use.
#[derive(Debug, Default, Clone)]
pub struct Users {
    pub names: Vec<String>,
    /// Processes that could not be inspected (another user's).
    pub blind: usize,
    pub failed: bool,
}

/// Who may use this file: what has it open, what names it or its folder, what runs from that folder.
/// A process that read it once at start and closed it can be seen only if it or its unit names it.
pub fn users_of(path: &Path, cfg: &KeepConfig) -> Users {
    let mut out = Users::default();
    let dir = path.parent().unwrap_or(path);
    let me = std::process::id();
    let file_b = path.as_os_str().as_bytes();
    let dir_b = dir.as_os_str().as_bytes();
    let Ok(procs) = std::fs::read_dir("/proc") else {
        out.failed = true;
        return out;
    };
    for entry in procs.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
        let base = entry.path();
        let comm = std::fs::read_to_string(base.join("comm")).unwrap_or_default().trim().to_owned();
        let label = format!("process {pid} ({comm})");
        match std::fs::read_dir(base.join("fd")) {
            Ok(fds) if pid != me => {
                if fds.flatten().any(|fd| std::fs::read_link(fd.path()).is_ok_and(|t| t == path)) {
                    out.names.push(format!("{label} has it open"));
                    continue;
                }
            }
            Ok(_) => {}
            Err(_) => {
                out.blind += 1;
                continue;
            }
        }
        if pid != me && std::fs::read_link(base.join("cwd")).is_ok_and(|c| c == dir) {
            out.names.push(format!("{label} runs in its folder"));
            continue;
        }
        for name in ["cmdline", "environ"] {
            // An environment holds other secrets: looked through, wiped, never kept.
            let Ok(raw) = std::fs::read(base.join(name)) else { continue };
            let raw = Zeroizing::new(raw);
            // The request itself names the path: the CLI, and the shell that started it.
            if name == "cmdline" && is_the_request(&raw) {
                continue;
            }
            if holds(&raw, file_b) || holds_dir(&raw, dir_b) {
                out.names.push(format!("{label} names it in its {name}"));
                break;
            }
        }
    }
    out.names.extend(unit_files_naming(&home_forms(path, &cfg.home), &home_forms(dir, &cfg.home), &cfg.unit_dirs));
    if let Ok(o) = std::process::Command::new("crontab").arg("-l").output() {
        let text = Zeroizing::new(o.stdout);
        if holds(&text, file_b) || holds_dir(&text, dir_b) {
            out.names.push("his crontab names it".to_owned());
        }
    }
    if let Ok(o) = std::process::Command::new("timeout")
        .args(["5", "docker", "ps", "--filter"])
        .arg(format!("label=com.docker.compose.project.working_dir={}", dir.display()))
        .args(["--format", "{{.Names}}"])
        .output()
    {
        if o.status.success() {
            for name in String::from_utf8_lossy(&o.stdout).lines().filter(|l| !l.trim().is_empty()) {
                out.names.push(format!("container {name} runs from its folder"));
            }
        }
    }
    out
}

/// `lucina-rs secret-keep save ...` as arguments, or inside a `sh -c` line.
fn is_the_request(cmdline: &[u8]) -> bool {
    holds(cmdline, b"secret-keep\0save") || holds(cmdline, b"secret-keep save")
}

/// The path as units and scripts write it.
fn home_forms(path: &Path, home: &Path) -> Vec<Vec<u8>> {
    let abs = path.display().to_string();
    let mut forms = vec![abs.clone()];
    if let Some(rest) = abs.strip_prefix(&format!("{}/", home.display())) {
        forms.extend([format!("%h/{rest}"), format!("$HOME/{rest}"), format!("${{HOME}}/{rest}"), format!("~/{rest}")]);
    }
    forms.into_iter().map(String::into_bytes).collect()
}

fn holds(hay: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && hay.windows(needle.len()).any(|w| w == needle)
}

/// The folder itself, not a longer name that starts like it.
fn holds_dir(hay: &[u8], dir: &[u8]) -> bool {
    if dir.is_empty() {
        return false;
    }
    hay.windows(dir.len()).enumerate().any(|(i, w)| {
        w == dir && hay.get(i + dir.len()).is_none_or(|c| !(c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.')))
    })
}

fn unit_files_naming(file_forms: &[Vec<u8>], dir_forms: &[Vec<u8>], dirs: &[PathBuf]) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for dir in dirs {
        walk(dir, 3, &mut |file| {
            let Ok(meta) = file.metadata() else { return };
            // Enabled units are links to the same file.
            let real = std::fs::canonicalize(file).unwrap_or_else(|_| file.to_path_buf());
            if meta.len() > 1_000_000 || !seen.insert(real) {
                return;
            }
            let Ok(raw) = std::fs::read(file) else { return };
            let raw = Zeroizing::new(raw);
            let name = file.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
            if file_forms.iter().any(|n| holds(&raw, n)) {
                out.push(format!("unit file {name} names it"));
            } else if dir_forms.iter().any(|n| holds_dir(&raw, n)) {
                out.push(format!("unit file {name} names its folder"));
            }
        });
    }
    out
}

fn walk(dir: &Path, depth: u8, f: &mut impl FnMut(&Path)) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            if depth > 0 {
                walk(&p, depth - 1, f);
            }
        } else {
            f(&p);
        }
    }
}

fn sync_dir(dir: &Path) {
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
}

fn overwrite(file: &mut std::fs::File, size: u64) -> std::io::Result<()> {
    let zeros = [0u8; 4096];
    let mut left = size as usize;
    while left > 0 {
        let n = left.min(zeros.len());
        file.write_all(&zeros[..n])?;
        left -= n;
    }
    file.sync_all()
}

/// Overwrite, then remove. A journal or an SSD may keep older blocks: that is written down in the design.
fn remove_source(path: &Path, id: Identity) -> Result<(), &'static str> {
    let mut f = open_no_links(path, libc::O_WRONLY).map_err(|_| "failed")?;
    if Identity::of(&f.metadata().map_err(|_| "failed")?) != id {
        return Err("changed");
    }
    overwrite(&mut f, id.size).map_err(|_| "failed")?;
    std::fs::remove_file(path).map_err(|_| "failed")?;
    if let Some(d) = path.parent() {
        sync_dir(d);
    }
    Ok(())
}

/// The keys out of the file, in a new file of the same mode that replaces it at once.
fn rewrite_env(path: &Path, id: Identity, cfg: &KeepConfig, moved: &[(String, String)]) -> Result<(), &'static str> {
    let now = read_source(path, cfg).map_err(|s| if s.code == "source_not_found" { "changed" } else { "failed" })?;
    if now.id != id {
        return Err("changed");
    }
    let new = envfile::without_keys(&now.text, moved);
    let dir = path.parent().ok_or("failed")?;
    let mut rnd = [0u8; 8];
    getrandom::fill(&mut rnd).map_err(|_| "failed")?;
    let tmp = dir.join(format!(".lucina-keep-{}", rnd.iter().map(|b| format!("{b:02x}")).collect::<String>()));
    let mode = id.mode & 0o7777;
    let write = (|| -> std::io::Result<()> {
        let mut t = std::fs::OpenOptions::new().write(true).create_new(true).mode(mode & 0o700).custom_flags(libc::O_NOFOLLOW).open(&tmp)?;
        t.set_permissions(std::fs::Permissions::from_mode(mode))?;
        t.write_all(new.as_bytes())?;
        t.sync_all()
    })();
    if write.is_err() {
        let _ = std::fs::remove_file(&tmp);
        return Err("failed");
    }
    // The old file stays open to wipe its blocks once nothing can reach it.
    let old = open_no_links(path, libc::O_WRONLY);
    if std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return Err("failed");
    }
    if let Ok(mut old) = old {
        let _ = overwrite(&mut old, id.size);
    }
    sync_dir(dir);
    Ok(())
}

impl Keeper {
    /// A file or env job: read, deposit, compare the read-back, maybe clear.
    pub(super) async fn process_source(&self, job: &Job, kind: &str, asked: &Path) -> Result<(String, Cleared), Stop> {
        let stored = self.store_source(job, kind, asked).await?;
        let mut cleared = Cleared::default();
        self.report_users(&stored.path, &mut cleared).await;
        if !job.clear {
            cleared.remains.push("the source file stays where it is".to_owned());
            return Ok((job.name.clone(), cleared));
        }
        if job.via == "chat" {
            // Tool calls come from a model that also reads untrusted text: this one takes his word.
            cleared.remains.push("the source file stays until you answer".to_owned());
            cleared.clear_state = Some("waiting_owner".to_owned());
            return Ok((job.name.clone(), cleared));
        }
        self.clear_source(job, kind, &stored, &mut cleared).await;
        Ok((job.name.clone(), cleared))
    }

    /// Read and deposit. Every key's outcome is on the job, also when this fails.
    async fn store_source(&self, job: &Job, kind: &str, asked: &Path) -> Result<Stored, Stop> {
        let opened = read_source(asked, &self.cfg)?;
        match kind {
            "file" => self.store_file(job, opened).await,
            "env" => self.store_env(job, opened).await,
            _ => Err(stop("bad_request")),
        }
    }

    async fn store_file(&self, job: &Job, opened: Opened) -> Result<Stored, Stop> {
        if source_path::key_refused(job.name.as_bytes()) {
            return Err(stop("refused_by_rule"));
        }
        let text = &opened.text;
        let value = if single_value(text) {
            SecretValue::new(text.trim().to_owned())
        } else if job.what.is_empty() {
            return Err(stop("not_one_value"));
        } else {
            if !self.cfg.local.is_loopback() {
                return Err(stop("local_model_not_local"));
            }
            let v = local::extract(&self.http, &self.cfg.local, &job.name, &job.what, "", text).await.map_err(local_stop)?;
            check_verbatim(&v, text)?;
            v
        };
        if !usable(value.expose_secret()) {
            return Err(stop("no_secret"));
        }
        let name = self.deposit(job, job.name.clone(), &value).await?;
        self.remember(&name, &value);
        let moves = vec![KeyMove { key: String::new(), name: name.clone(), state: "saved".to_owned(), reference: self.reference(job, &name) }];
        self.note_moves(&job.id, &moves);
        Ok(Stored { moves, id: opened.id, path: opened.path })
    }

    async fn store_env(&self, job: &Job, opened: Opened) -> Result<Stored, Stop> {
        let parsed = envfile::parse(&opened.text);
        let mut moves: Vec<KeyMove> = Vec::new();
        let mut first_stop: Option<&'static str> = None;
        let mut retry: Option<Stop> = None;
        let mut keys = parsed.keys();
        for wanted in &job.keys {
            if !keys.contains(wanted) {
                keys.push(wanted.clone());
            }
        }
        for key in &keys {
            let entry = parsed.last(key);
            let state: Option<&str> = match entry.map(|e| &e.value) {
                None => Some("not_in_file"),
                Some(_) if source_path::key_refused(key.as_bytes()) => Some("refused_by_rule"),
                Some(EnvValue::Refused) => Some("refused_by_rule"),
                Some(_) if !job.keys.is_empty() && !job.keys.contains(key) => Some("left"),
                Some(EnvValue::Empty) => Some("empty"),
                Some(EnvValue::Reference) => Some("reference"),
                Some(EnvValue::Unclear) => Some("unclear"),
                Some(EnvValue::Exact(v)) => {
                    if envfile::is_placeholder(v.expose_secret()) {
                        Some("placeholder")
                    } else if job.keys.is_empty() && !envfile::looks_secret(key, Some(v)) {
                        Some("left")
                    } else if !usable(v.expose_secret()) {
                        Some("too_short")
                    } else {
                        None
                    }
                }
            };
            if let Some(state) = state {
                moves.push(KeyMove { key: key.clone(), name: String::new(), state: state.to_owned(), reference: String::new() });
                continue;
            }
            let Some(EnvValue::Exact(value)) = entry.map(|e| &e.value) else { continue };
            let name = stored_name(&job.prefix, key);
            if source_path::key_refused(name.as_bytes()) {
                moves.push(KeyMove { key: key.clone(), name: String::new(), state: "refused_by_rule".to_owned(), reference: String::new() });
                continue;
            }
            match self.deposit(job, name.clone(), value).await {
                Ok(name) => {
                    self.remember(&name, value);
                    let reference = self.reference(job, &name);
                    moves.push(KeyMove { key: key.clone(), name, state: "saved".to_owned(), reference });
                }
                Err(s) if s.retry => {
                    retry = Some(s);
                    break;
                }
                Err(s) => {
                    first_stop.get_or_insert(s.code);
                    moves.push(KeyMove { key: key.clone(), name, state: s.code.to_owned(), reference: String::new() });
                }
            }
        }
        for line in &parsed.unclear_lines {
            moves.push(KeyMove { key: format!("line {line}"), name: String::new(), state: "unclear".to_owned(), reference: String::new() });
        }
        self.note_moves(&job.id, &moves);
        if let Some(s) = retry {
            return Err(s);
        }
        if !moves.iter().any(|m| m.state == "saved") {
            return Err(stop(first_stop.unwrap_or("no_secret")));
        }
        Ok(Stored { moves, id: opened.id, path: opened.path })
    }

    fn reference(&self, job: &Job, name: &str) -> String {
        let folder = if job.path == "/" { String::new() } else { job.path.clone() };
        format!("secret://{}/{}{folder}#{name}", job.project, job.environment)
    }

    fn remember(&self, name: &str, value: &SecretValue) {
        if value.expose_secret().chars().count() < MIN_REDACTOR_CHARS {
            return;
        }
        if let Err(why) = lucina_secrets::redactor().try_register(name, value.expose_secret()) {
            tracing::warn!(name = %name, error = %why, "kept secret not registered with the redactor");
        }
    }

    fn note_moves(&self, id: &str, moves: &[KeyMove]) {
        self.with(|jobs| {
            if let Some(j) = jobs.iter_mut().find(|j| j.id == id) {
                j.moves = moves.to_vec();
            }
        });
    }

    async fn report_users(&self, path: &Path, cleared: &mut Cleared) -> Users {
        let (path, cfg) = (path.to_path_buf(), self.cfg.clone());
        let users = tokio::task::spawn_blocking(move || users_of(&path, &cfg))
            .await
            .unwrap_or(Users { failed: true, ..Users::default() });
        cleared.users = users.names.clone();
        let mut note = |text: &str| {
            if !cleared.remains.iter().any(|r| r == text) {
                cleared.remains.push(text.to_owned());
            }
        };
        if users.failed {
            note("what uses the file could not be checked");
        }
        if users.blind > 0 {
            note("processes of other users can't be seen from here");
        }
        users
    }

    /// Only after a confirmed save, and only if nothing is seen using it.
    async fn clear_source(&self, job: &Job, kind: &str, stored: &Stored, cleared: &mut Cleared) {
        let users = self.report_users(&stored.path, cleared).await;
        let state = if users.failed || !users.names.is_empty() {
            cleared.remains.push("the source file stays: something may use it".to_owned());
            "in_use"
        } else {
            let done = match kind {
                "file" => remove_source(&stored.path, stored.id),
                _ => {
                    let moved: Vec<(String, String)> = stored.saved().iter().map(|m| (m.key.clone(), m.reference.clone())).collect();
                    rewrite_env(&stored.path, stored.id, &self.cfg, &moved)
                }
            };
            match done {
                Ok(()) => {
                    cleared.source = true;
                    "cleared"
                }
                Err("changed") => {
                    cleared.remains.push("the source file changed meanwhile, so it stays".to_owned());
                    "changed"
                }
                Err(_) => {
                    tracing::warn!(job = %job.id, name = %job.name, "the source file was not cleared");
                    cleared.remains.push("the source file could not be cleared".to_owned());
                    "failed"
                }
            }
        };
        cleared.clear_state = Some(state.to_owned());
    }

    /// Ask him about a clear that came through the chat tool. His answer starts [`Self::settle_clears`].
    pub(super) async fn ask_clear(&self, st: &crate::AppState, job: &Job) {
        let names: Vec<String> = job.moves.iter().filter(|m| m.state == "saved").map(|m| m.name.clone()).collect();
        let path = job.source.split_once(':').map_or("", |(_, p)| p);
        let users = job.cleared.as_ref().map(|c| c.users.clone()).unwrap_or_default();
        let body = json!({
            "question": format!("Remove the secret from its source file {path}, now that Infisical holds it as {}?", names.join(", ")),
            "options": [CLEAR_OPTION, LEAVE_OPTION],
            "recommendation": LEAVE_OPTION,
            "context": format!("Asked through the chat tool, so it may come from text in a chat or a web page and not from him. Read-back matched. Seen using it: {users:?}."),
            "reversible": false,
            "impact": "high",
            "urgency": "low",
            "topic": "secret-keep",
            "asking_session": job.id,
            "asking_client": "lucina-secret-keep",
        });
        let decision = match crate::decide::create(st, &body).await {
            Ok(d) => Some(d.decision_id),
            Err(e) => {
                tracing::warn!(job = %job.id, code = %e.code, "no decision for a clear");
                None
            }
        };
        let unasked = self.with(|jobs| {
            let j = jobs.iter_mut().find(|j| j.id == job.id)?;
            if decision.is_some() {
                j.decision = decision.clone();
                return None;
            }
            let c = j.cleared.get_or_insert_with(Cleared::default);
            c.clear_state = Some("failed".to_owned());
            c.remains.push("the question about clearing could not be asked".to_owned());
            Some(j.clone())
        });
        if let Some(j) = unasked {
            self.tell_cleared(&j);
        }
    }

    /// Jobs waiting for his answer on a clear.
    pub async fn settle_clears(&self, st: &crate::AppState) {
        let waiting: Vec<Job> = self.with(|jobs| {
            jobs.iter()
                .filter(|j| j.state == State::Saved && j.decision.is_some() && j.cleared.as_ref().and_then(|c| c.clear_state.as_deref()) == Some("waiting_owner"))
                .cloned()
                .collect()
        });
        for job in waiting {
            let id = job.decision.clone().unwrap_or_default();
            let d = crate::decide::get(st, &id).unwrap_or(Value::Null);
            let status = d["status"].as_str().unwrap_or("gone");
            if status == "pending" {
                continue;
            }
            let yes = status == "decided" && d["decided_by"] == "owner" && d["chosen_option"] == CLEAR_OPTION;
            let mut cleared = job.cleared.clone().unwrap_or_default();
            cleared.remains.retain(|r| r != "the source file stays until you answer");
            if yes {
                let Some((kind, key)) = job.source.split_once(':') else { continue };
                // Read and deposit again: a changed file answers `exists` and its key stays.
                match self.store_source(&job, kind, Path::new(key)).await {
                    Ok(stored) => self.clear_source(&job, kind, &stored, &mut cleared).await,
                    Err(s) => {
                        tracing::warn!(job = %job.id, code = s.code, "a clear found the source different");
                        cleared.clear_state = Some("changed".to_owned());
                        cleared.remains.push(format!("not cleared: {}", s.code));
                    }
                }
            } else {
                cleared.clear_state = Some("left_by_owner".to_owned());
                cleared.remains.push("the source file stays".to_owned());
            }
            let done = self.with(|jobs| {
                let j = jobs.iter_mut().find(|j| j.id == job.id)?;
                j.cleared = Some(cleared);
                Some(j.clone())
            });
            if let Some(j) = done {
                self.tell_cleared(&j);
            }
        }
    }

    /// What a project needs, what is found, what is missing: names only. `deposit` queues what's found.
    pub fn project(&self, body: &Value, via: &str, agent: bool) -> Result<Value, ApiError> {
        if !self.cfg.enabled {
            return Err(ApiError::new(503, "secret_keep_off", "saving secrets locally is not configured"));
        }
        let text = |k: &str| body[k].as_str().map(str::trim).unwrap_or("");
        let dir_raw = text("dir");
        // The folder is checked by a file inside it: the rule is about what may be read.
        let probe = format!("{}/.env", dir_raw.trim_end_matches('/'));
        let probe = check_asked(&probe, &self.cfg)?;
        let dir = probe.parent().ok_or_else(refused)?.to_path_buf();

        let read_names = |file: &str, out: &mut Vec<String>, notes: &mut Vec<Value>| match read_source(&dir.join(file), &self.cfg) {
            Ok(o) => {
                if COMPOSE_FILES.contains(&file) {
                    for n in compose_names(&o.text) {
                        if !out.contains(&n) {
                            out.push(n);
                        }
                    }
                } else {
                    for n in envfile::parse(&o.text).keys() {
                        if !out.contains(&n) {
                            out.push(n);
                        }
                    }
                }
                true
            }
            Err(s) if s.code == "source_not_found" => false,
            Err(s) => {
                notes.push(json!({"file": file, "refused": s.code}));
                false
            }
        };
        let (mut needs, mut notes, mut listed) = (Vec::new(), Vec::new(), Vec::new());
        for f in EXAMPLE_FILES.iter().chain(COMPOSE_FILES) {
            if read_names(f, &mut needs, &mut notes) {
                listed.push(*f);
            }
        }

        // Each value file, by key. A real value in an earlier file wins.
        struct Seen {
            file: &'static str,
            state: &'static str,
            secret: bool,
        }
        let mut seen: Vec<(String, Seen)> = Vec::new();
        let mut unclear = Vec::new();
        for file in VALUE_FILES {
            let opened = match read_source(&dir.join(file), &self.cfg) {
                Ok(o) => o,
                Err(s) if s.code == "source_not_found" => continue,
                Err(s) => {
                    notes.push(json!({"file": file, "refused": s.code}));
                    continue;
                }
            };
            let parsed = envfile::parse(&opened.text);
            if !parsed.unclear_lines.is_empty() {
                unclear.push(json!({"file": file, "lines": parsed.unclear_lines}));
            }
            for key in parsed.keys() {
                let Some(e) = parsed.last(&key) else { continue };
                let (state, secret) = match &e.value {
                    _ if source_path::key_refused(key.as_bytes()) => ("refused_by_rule", true),
                    EnvValue::Refused => ("refused_by_rule", true),
                    EnvValue::Empty => ("empty", envfile::looks_secret(&key, None)),
                    EnvValue::Reference => ("reference", envfile::looks_secret(&key, None)),
                    EnvValue::Unclear => ("unclear", envfile::looks_secret(&key, None)),
                    EnvValue::Exact(v) if envfile::is_placeholder(v.expose_secret()) => ("placeholder", envfile::looks_secret(&key, None)),
                    EnvValue::Exact(v) => ("found", envfile::looks_secret(&key, Some(v))),
                };
                match seen.iter_mut().find(|(k, _)| *k == key) {
                    Some((_, s)) if s.state != "found" && state == "found" => *s = Seen { file, state, secret },
                    Some(_) => {}
                    None => seen.push((key, Seen { file, state, secret })),
                }
            }
        }

        let mut rows = Vec::new();
        for name in &needs {
            let row = match seen.iter().find(|(k, _)| k == name) {
                Some((_, s)) => json!({"name": name, "state": s.state, "file": s.file, "secret": s.secret}),
                None if source_path::key_refused(name.as_bytes()) => json!({"name": name, "state": "refused_by_rule", "secret": true}),
                None => json!({"name": name, "state": "missing", "secret": envfile::looks_secret(name, None)}),
            };
            rows.push(row);
        }
        let extra: Vec<Value> = seen
            .iter()
            .filter(|(k, s)| !needs.contains(k) && s.state == "found" && s.secret)
            .map(|(k, s)| json!({"name": k, "file": s.file}))
            .collect();
        let missing: Vec<&Value> = rows.iter().filter(|r| r["secret"] == true && r["state"] != "found" && r["state"] != "refused_by_rule").map(|r| &r["name"]).collect();
        let mut out = json!({
            "dir": dir.display().to_string(),
            "listed_in": listed,
            "needs": rows,
            "missing": missing,
            "not_in_a_list": extra,
            "unclear_lines": unclear,
            "files_refused": notes,
            "note": "Names and states only. A new Infisical project is the owner's to make: the keeper may only add names where it has a deposit grant."
        });
        if body["deposit"].as_bool().unwrap_or(false) {
            let mut jobs = Vec::new();
            for file in VALUE_FILES {
                let keys: Vec<&String> = seen.iter().filter(|(k, s)| s.file == *file && s.state == "found" && s.secret && !source_path::key_refused(k.as_bytes())).map(|(k, _)| k).collect();
                if keys.is_empty() {
                    continue;
                }
                let job = self.enqueue_via(
                    &json!({
                        "source": format!("env:{}", dir.join(file).display()),
                        "keys": keys,
                        "prefix": text("prefix"),
                        "project": text("project"),
                        "environment": text("environment"),
                        "path": text("path"),
                    }),
                    via,
                    agent,
                )?;
                jobs.push(job.outcome());
            }
            out["jobs"] = json!(jobs);
        }
        Ok(out)
    }
}

/// `${NAME}`, `${NAME:-default}` in a compose file. The default is never kept.
fn compose_names(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find("${") {
        rest = &rest[i + 2..];
        let name: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
        if envfile::plain_name(&name) && !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_checks() {
        assert_eq!(check_meta(true, 1000, 1000, 1, 44), Ok(()));
        assert_eq!(check_meta(false, 1000, 1000, 1, 44), Err("not_a_file"));
        assert_eq!(check_meta(true, 0, 1000, 1, 44), Err("not_owner"));
        assert_eq!(check_meta(true, 1000, 1000, 2, 44), Err("hard_linked"));
        assert_eq!(check_meta(true, 1000, 1000, 1, MAX_FILE_BYTES as u64 + 1), Err("too_long"));
        assert_eq!(check_meta(true, 1000, 1000, 1, MAX_FILE_BYTES as u64), Ok(()));
    }

    #[test]
    fn expand_is_only_a_prefix() {
        let h = Path::new("/home/nez");
        assert_eq!(expand("~/.config/a/k", h).unwrap(), Path::new("/home/nez/.config/a/k"));
        assert_eq!(expand("~//etc/passwd", h).unwrap().display().to_string(), "/home/nez//etc/passwd");
        assert!(expand("~root/x", h).is_none());
        assert_eq!(expand("/a/b", h).unwrap(), Path::new("/a/b"));
    }

    #[test]
    fn one_value_or_not() {
        for yes in ["abc\n", "abc", "  abc  \n\n", "-----BEGIN KEY-----\nAAA\nBBB\n-----END KEY-----\n"] {
            assert!(single_value(yes), "{yes:?}");
        }
        for no in ["", "\n", "a\nb\n", "user: x\npass: y", "-----BEGIN A-----\n-----END A-----\n-----BEGIN B-----\n-----END B-----\n"] {
            assert!(!single_value(no), "{no:?}");
        }
    }

    #[test]
    fn names() {
        assert_eq!(default_prefix(Path::new("/home/nez/.config/flareon-list/.env")), "KEEP_FLAREON_LIST_");
        assert_eq!(stored_name("KEEP_A_", "TOKEN"), "KEEP_A_TOKEN");
        assert_eq!(stored_name("KEEP_A_", "KEEP_A_TOKEN"), "KEEP_A_TOKEN");
        assert_eq!(stored_name("", "TOKEN"), "TOKEN");
    }

    #[test]
    fn compose_gives_names_not_defaults() {
        let n = compose_names("a: ${DB_PASSWORD}\nb: ${API_KEY:-canary-default}\nc: $NOT\nd: ${}\ne: ${DB_PASSWORD}\n");
        assert_eq!(n, vec!["DB_PASSWORD", "API_KEY"]);
    }

    #[test]
    fn the_request_is_not_a_user() {
        assert!(is_the_request(b"lucina-rs\0secret-keep\0save\0--source\0file:/a/b\0"));
        assert!(is_the_request(b"bash\0-c\0lucina-rs secret-keep save --source file:/a/b\0"));
        assert!(!is_the_request(b"app\0--config\0/home/nez/.config/secret-keep-canary/save\0"));
        assert!(!is_the_request(b"lucina-rs\0secret-keep\0status\0file:/a/b\0"));
    }

    #[test]
    fn a_folder_is_not_a_longer_name() {
        assert!(holds_dir(b"x /a/b/c y", b"/a/b"));
        assert!(holds_dir(b"x=/a/b", b"/a/b"));
        assert!(!holds_dir(b"x /a/bc y", b"/a/b"));
        assert!(!holds_dir(b"x /a/b-c y", b"/a/b"));
    }
}
