//! Sincronizzazione Git. Il repository è dato non fidato: può essere stato
//! scritto da un altro dispositivo o da un collaboratore, quindi il checkout
//! non usa `checkout_head` ma materializza l'albero da sé, con questi limiti:
//!
//! * solo blob regolari (mode 100644/100755): symlink (120000) e submodule
//!   (160000) sono rifiutati;
//! * ogni nome di entry passa da `validate_component`: niente `..`, niente
//!   percorsi assoluti, niente `.git`;
//! * limiti di dimensione per blob, per checkout e per numero di file;
//! * niente hook (libgit2 non li esegue) e niente credential helper: le
//!   credenziali arrivano solo dalla memoria del processo;
//! * un pull con modifiche non committate è rifiutato, così l'inchiostro non
//!   ancora salvato non viene sovrascritto.
//!
//! Il merge avviene in memoria (`merge_commits` + `Checkout::NONE`): se ci
//! sono conflitti il disco non è stato toccato e l'errore elenca i file.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use git2::{
    Commit, ConfigLevel, Cred, FetchOptions, IndexAddOption, MergeOptions, ObjectType, PushOptions,
    RemoteCallbacks, Repository, StatusOptions, Tree,
};
use serde::Serialize;

use crate::error::{Error, Identity, RemoteUrl, Result};
use crate::paths::{atomic_write, safe_join, validate_component};

pub const REMOTE: &str = "origin";
pub const DEFAULT_BRANCH: &str = "main";
const FETCH_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_BLOB_BYTES: u64 = 16 * 1024 * 1024;
const MAX_CHECKOUT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_CHECKOUT_FILES: usize = 20_000;
const MAX_TREE_DEPTH: usize = 32;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitStatus {
    pub initialized: bool,
    pub branch: Option<String>,
    pub remote: Option<String>,
    pub dirty: bool,
    pub ahead: usize,
    pub behind: usize,
    pub files: usize,
    pub head: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PullReport {
    /// `up_to_date` | `fast_forward` | `merged` | `initial`
    pub outcome: String,
    pub branch: String,
    pub head: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitReport {
    pub oid: String,
    pub short: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PushReport {
    pub branch: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteInfo {
    pub url: Option<String>,
    pub initialized: bool,
}

/// Un repository esiste solo se `.git` è una directory reale.
pub fn is_repo(root: &Path) -> bool {
    match std::fs::symlink_metadata(root.join(".git")) {
        Ok(meta) => meta.is_dir() && !meta.file_type().is_symlink(),
        Err(_) => false,
    }
}

pub fn init(root: &Path, identity: &Identity) -> Result<()> {
    if is_repo(root) {
        return Err(Error::Rejected("repository già inizializzato".into()));
    }
    let repo = Repository::init(root)?;
    // Il branch iniziale è deciso da Inkpad, non dalla configurazione globale
    // della macchina (che su un dispositivo mobile non esiste).
    repo.set_head(&format!("refs/heads/{DEFAULT_BRANCH}"))?;
    let mut config = repo.config()?;
    config.set_str("core.autocrlf", "false")?;
    config.set_bool("core.filemode", false)?;
    config.set_str("user.name", &identity.name)?;
    config.set_str("user.email", &identity.email)?;
    Ok(())
}

pub fn remote_info(root: &Path) -> Result<RemoteInfo> {
    if !is_repo(root) {
        return Ok(RemoteInfo {
            url: None,
            initialized: false,
        });
    }
    let repo = open(root)?;
    Ok(RemoteInfo {
        url: remote_url(&repo)?,
        initialized: true,
    })
}

pub fn set_remote(root: &Path, url: &RemoteUrl) -> Result<()> {
    let repo = open(root)?;
    if repo.find_remote(REMOTE).is_ok() {
        repo.remote_set_url(REMOTE, url.as_str())?;
    } else {
        repo.remote(REMOTE, url.as_str())?;
    }
    Ok(())
}

pub fn status(root: &Path) -> Result<GitStatus> {
    if !is_repo(root) {
        return Ok(GitStatus {
            initialized: false,
            branch: None,
            remote: None,
            dirty: false,
            ahead: 0,
            behind: 0,
            files: 0,
            head: None,
        });
    }
    let repo = open(root)?;
    let branch = head_branch_opt(&repo)?;
    let remote = remote_url(&repo)?;

    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .exclude_submodules(true);
    let statuses = repo.statuses(Some(&mut opts))?;
    let files = statuses.len();
    let dirty = statuses
        .iter()
        .any(|entry| entry.status() != git2::Status::CURRENT);

    let (ahead, behind) = branch
        .as_deref()
        .map(|branch| ahead_behind(&repo, branch))
        .unwrap_or((0, 0));
    let head = repo
        .head()
        .ok()
        .and_then(|head| head.target())
        .map(short_oid);

    Ok(GitStatus {
        initialized: true,
        branch,
        remote,
        dirty,
        ahead,
        behind,
        files,
        head,
    })
}

pub fn commit_all(root: &Path, message: &str) -> Result<CommitReport> {
    let message = message.trim();
    if message.is_empty() {
        return Err(Error::Invalid("messaggio di commit vuoto".into()));
    }
    if message.chars().count() > 2_000 {
        return Err(Error::Invalid("messaggio di commit troppo lungo".into()));
    }
    let repo = open(root)?;
    assert_safe_config(&repo)?;

    let mut index = repo.index()?;
    // Solo i nostri file: un repository dell'utente può contenerne altri.
    let pathspec = ["notebooks", "inkpad.json"];
    index.add_all(pathspec.iter(), IndexAddOption::DEFAULT, None)?;
    index.update_all(pathspec.iter(), None)?;
    index.write()?;

    let tree_id = index.write_tree()?;
    let tree = repo.find_tree(tree_id)?;
    let parent = match repo.head() {
        Ok(head) => Some(head.peel_to_commit()?),
        Err(err)
            if err.code() == git2::ErrorCode::UnbornBranch
                || err.code() == git2::ErrorCode::NotFound =>
        {
            None
        }
        Err(err) => return Err(err.into()),
    };
    if let Some(parent) = &parent {
        if parent.tree_id() == tree_id {
            return Err(Error::Rejected("nessuna modifica da committare".into()));
        }
    }

    let signature = signature(&repo)?;
    let parents: Vec<&Commit> = parent.iter().collect();
    let oid = repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        message,
        &tree,
        &parents,
    )?;
    Ok(CommitReport {
        oid: oid.to_string(),
        short: short_oid(oid),
    })
}

/// Fetch + fast-forward, oppure merge senza conflitti. Un merge con conflitti
/// non scrive nulla e restituisce l'elenco dei file.
pub fn pull(root: &Path, token: Option<&str>) -> Result<PullReport> {
    let repo = open(root)?;
    assert_safe_config(&repo)?;
    assert_clean_worktree(&repo)?;

    let remote = remote_url(&repo)?;
    if remote
        .as_deref()
        .map(|url| url.starts_with("https://"))
        .unwrap_or(false)
        && token.is_none()
    {
        return Err(Error::AuthRequired);
    }
    if remote.is_none() {
        return Err(Error::NoRepo);
    }

    fetch(&repo, token)?;

    // Primo pull su un vault vuoto: si adotta il branch remoto.
    if repo.head().is_err() {
        let branch = DEFAULT_BRANCH.to_string();
        let target = repo
            .find_reference(&format!("refs/remotes/{REMOTE}/{branch}"))
            .and_then(|reference| reference.peel_to_commit())
            .map_err(|_| Error::Invalid(format!("il remote non ha un branch '{branch}'")))?;
        adopt_branch(&repo, root, &branch, &target)?;
        return Ok(PullReport {
            outcome: "initial".into(),
            branch,
            head: short_oid(target.id()),
        });
    }

    let branch = head_branch(&repo)?;
    let local = repo.head()?.peel_to_commit()?;
    let remote_reference = repo
        .find_reference(&format!("refs/remotes/{REMOTE}/{branch}"))
        .map_err(|_| Error::Invalid(format!("il remote non ha un branch '{branch}'")))?;
    let remote_commit = remote_reference.peel_to_commit()?;

    if local.id() == remote_commit.id() {
        return Ok(PullReport {
            outcome: "up_to_date".into(),
            branch,
            head: short_oid(local.id()),
        });
    }

    let annotated = repo.find_annotated_commit(remote_commit.id())?;
    let (analysis, _) = repo.merge_analysis(&[&annotated])?;

    if analysis.is_up_to_date() {
        return Ok(PullReport {
            outcome: "up_to_date".into(),
            branch,
            head: short_oid(local.id()),
        });
    }
    if analysis.is_fast_forward() {
        fast_forward(&repo, root, &branch, &remote_commit)?;
        return Ok(PullReport {
            outcome: "fast_forward".into(),
            branch,
            head: short_oid(remote_commit.id()),
        });
    }
    if analysis.is_normal() {
        let head = merge(&repo, root, &branch, &local, &remote_commit)?;
        return Ok(PullReport {
            outcome: "merged".into(),
            branch,
            head,
        });
    }
    Err(Error::Rejected(
        "sincronizzazione non supportata: storie non collegate".into(),
    ))
}

pub fn push(root: &Path, token: Option<&str>) -> Result<PushReport> {
    let repo = open(root)?;
    assert_safe_config(&repo)?;
    let remote_url = remote_url(&repo)?.ok_or(Error::NoRepo)?;
    if remote_url.starts_with("https://") && token.is_none() {
        return Err(Error::AuthRequired);
    }
    let branch = head_branch(&repo)?;
    let mut remote = repo.find_remote(REMOTE)?;

    let rejection: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let rejection_cb = rejection.clone();
    let mut callbacks = RemoteCallbacks::new();
    callbacks.credentials(move |_url, user, _allowed| match token {
        Some(token) => Cred::userpass_plaintext(user.unwrap_or("x-access-token"), token),
        None => Err(git2::Error::from_str("token richiesto")),
    });
    callbacks.push_update_reference(move |_refname, message| match message {
        Some(message) => {
            *rejection_cb.lock().expect("lock") = Some(message.to_string());
            Err(git2::Error::from_str(message))
        }
        None => Ok(()),
    });

    let mut options = PushOptions::new();
    options.remote_callbacks(callbacks);
    remote.push(
        &[format!("refs/heads/{branch}:refs/heads/{branch}")],
        Some(&mut options),
    )?;

    if let Some(message) = rejection.lock().expect("lock").take() {
        return Err(Error::Rejected(format!(
            "push rifiutato dal remote: {message}"
        )));
    }
    Ok(PushReport { branch })
}

// --- interni -------------------------------------------------------------

fn open(root: &Path) -> Result<Repository> {
    if !is_repo(root) {
        return Err(Error::NoRepo);
    }
    let repo = Repository::open(root)?;
    let workdir = repo
        .workdir()
        .ok_or_else(|| Error::Rejected("repository bare non supportato".into()))?;
    let expected = std::fs::canonicalize(root)?;
    let actual = std::fs::canonicalize(workdir)?;
    if expected != actual {
        return Err(Error::Rejected(
            "il repository non coincide con il vault".into(),
        ));
    }
    Ok(repo)
}

fn remote_url(repo: &Repository) -> Result<Option<String>> {
    match repo.find_remote(REMOTE) {
        Ok(remote) => match remote.url() {
            Ok(url) => Ok(Some(url.to_string())),
            Err(_) => Ok(None),
        },
        Err(_) => Ok(None),
    }
}

fn head_branch(repo: &Repository) -> Result<String> {
    head_branch_opt(repo)?.ok_or_else(|| Error::Rejected("HEAD staccato non supportato".into()))
}

fn head_branch_opt(repo: &Repository) -> Result<Option<String>> {
    let head = match repo.head() {
        Ok(head) => head,
        Err(err)
            if err.code() == git2::ErrorCode::UnbornBranch
                || err.code() == git2::ErrorCode::NotFound =>
        {
            return Ok(Some(DEFAULT_BRANCH.to_string()))
        }
        Err(err) => return Err(err.into()),
    };
    if !head.is_branch() {
        return Ok(None);
    }
    let name = head
        .shorthand()
        .map_err(|_| Error::Rejected("nome del branch non valido".into()))?
        .to_string();
    validate_branch(&name)?;
    Ok(Some(name))
}

fn validate_branch(branch: &str) -> Result<()> {
    let invalid = branch.is_empty()
        || branch.len() > 128
        || branch.starts_with('-')
        || branch.starts_with('/')
        || branch.ends_with('/')
        || branch.ends_with(".lock")
        || branch.contains("..")
        || branch.contains("//")
        || branch.contains('\\')
        || branch
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || "~^:?*[".contains(c));
    if invalid {
        return Err(Error::Rejected("nome del branch non valido".into()));
    }
    Ok(())
}

fn signature(repo: &Repository) -> Result<git2::Signature<'static>> {
    let config = repo.config()?;
    let name = config.get_string("user.name").unwrap_or_default();
    let email = config.get_string("user.email").unwrap_or_default();
    let identity = Identity::new(&name, &email).map_err(|_| {
        Error::Invalid("identità Git non configurata: imposta nome ed email".into())
    })?;
    Ok(git2::Signature::now(&identity.name, &identity.email)?)
}

/// Config locale del repository: niente include (leggerebbe file arbitrari),
/// niente credential helper o comandi di filtro (eseguirebbero processi).
fn assert_safe_config(repo: &Repository) -> Result<()> {
    let local = repo.config()?.open_level(ConfigLevel::Local)?;
    let mut entries = local.entries(None)?;
    while let Some(entry) = entries.next() {
        let entry = entry?;
        let Ok(name) = entry.name() else { continue };
        let name = name.to_ascii_lowercase();
        let forbidden = name.starts_with("credential")
            || name.starts_with("include")
            || name.starts_with("filter.")
            || name == "core.sshcommand"
            || name == "core.hookspath"
            || (name.starts_with("diff.") && name.ends_with(".command"));
        if forbidden {
            return Err(Error::Rejected(format!(
                "configurazione Git non ammessa per la sincronizzazione: {name}"
            )));
        }
    }
    Ok(())
}

fn assert_clean_worktree(repo: &Repository) -> Result<()> {
    let mut options = StatusOptions::new();
    options.include_untracked(false).exclude_submodules(true);
    let statuses = repo.statuses(Some(&mut options))?;
    if statuses
        .iter()
        .any(|entry| entry.status() != git2::Status::CURRENT)
    {
        return Err(Error::Rejected(
            "ci sono modifiche non committate: fai un commit prima di sincronizzare".into(),
        ));
    }
    Ok(())
}

fn ahead_behind(repo: &Repository, branch: &str) -> (usize, usize) {
    let local = repo.refname_to_id(&format!("refs/heads/{branch}")).ok();
    let upstream = repo
        .refname_to_id(&format!("refs/remotes/{REMOTE}/{branch}"))
        .ok();
    match (local, upstream) {
        (Some(local), Some(upstream)) => repo.graph_ahead_behind(local, upstream).unwrap_or((0, 0)),
        _ => (0, 0),
    }
}

fn fetch(repo: &Repository, token: Option<&str>) -> Result<()> {
    let mut remote = repo.find_remote(REMOTE)?;
    let deadline = Instant::now() + FETCH_TIMEOUT;
    let timed_out = Arc::new(AtomicBool::new(false));

    let mut callbacks = RemoteCallbacks::new();
    callbacks.credentials(move |_url, user, _allowed| match token {
        Some(token) => Cred::userpass_plaintext(user.unwrap_or("x-access-token"), token),
        None => Err(git2::Error::from_str("token richiesto")),
    });
    let flag = timed_out.clone();
    callbacks.transfer_progress(move |_progress| {
        if Instant::now() > deadline {
            flag.store(true, Ordering::Relaxed);
            false
        } else {
            true
        }
    });

    let mut options = FetchOptions::new();
    options.remote_callbacks(callbacks);
    remote.fetch(
        &[format!("+refs/heads/*:refs/remotes/{REMOTE}/*")],
        Some(&mut options),
        None,
    )?;

    if timed_out.load(Ordering::Relaxed) {
        return Err(Error::Timeout);
    }
    Ok(())
}

fn adopt_branch(repo: &Repository, root: &Path, branch: &str, target: &Commit<'_>) -> Result<()> {
    let tree = target.tree()?;
    materialize(repo, root, &tree)?;
    let mut index = repo.index()?;
    index.read_tree(&tree)?;
    index.write()?;
    let refname = format!("refs/heads/{branch}");
    repo.reference(&refname, target.id(), true, "inkpad: primo pull")?;
    repo.set_head(&refname)?;
    Ok(())
}

fn fast_forward(repo: &Repository, root: &Path, branch: &str, target: &Commit<'_>) -> Result<()> {
    let old_tree = repo.head().ok().and_then(|head| head.peel_to_tree().ok());
    let tree = target.tree()?;
    materialize(repo, root, &tree)?;
    if let Some(old_tree) = &old_tree {
        remove_stale(repo, root, old_tree, &tree)?;
    }
    let mut index = repo.index()?;
    index.read_tree(&tree)?;
    index.write()?;
    let refname = format!("refs/heads/{branch}");
    repo.reference(&refname, target.id(), true, "inkpad: fast-forward")?;
    repo.set_head(&refname)?;
    Ok(())
}

fn merge(
    repo: &Repository,
    root: &Path,
    branch: &str,
    local: &Commit<'_>,
    remote: &Commit<'_>,
) -> Result<String> {
    let mut options = MergeOptions::new();
    options.fail_on_conflict(false);
    let mut index = repo.merge_commits(local, remote, Some(&options))?;

    if index.has_conflicts() {
        let mut paths = Vec::new();
        let conflicts = index.conflicts()?;
        for conflict in conflicts {
            let conflict = conflict?;
            for entry in [conflict.our, conflict.their, conflict.ancestor]
                .into_iter()
                .flatten()
            {
                if let Ok(path) = std::str::from_utf8(&entry.path) {
                    paths.push(path.to_string());
                }
            }
        }
        paths.sort();
        paths.dedup();
        // Nessuna scrittura: l'indice è solo in memoria.
        return Err(Error::Conflict(paths.join(", ")));
    }

    let tree_id = index.write_tree_to(repo)?;
    let tree = repo.find_tree(tree_id)?;
    materialize(repo, root, &tree)?;
    remove_stale(repo, root, &local.tree()?, &tree)?;

    let mut disk_index = repo.index()?;
    disk_index.read_tree(&tree)?;
    disk_index.write()?;

    let signature = signature(repo)?;
    let message = format!("inkpad: merge di {REMOTE}/{branch}");
    let oid = repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        &message,
        &tree,
        &[local, remote],
    )?;
    Ok(short_oid(oid))
}

struct CheckoutState {
    files: Vec<PathBuf>,
    total_bytes: u64,
}

fn materialize(repo: &Repository, root: &Path, tree: &Tree<'_>) -> Result<Vec<PathBuf>> {
    let mut state = CheckoutState {
        files: Vec::new(),
        total_bytes: 0,
    };
    walk_tree(repo, root, tree, Path::new(""), 0, &mut state)?;
    Ok(state.files)
}

fn walk_tree(
    repo: &Repository,
    root: &Path,
    tree: &Tree<'_>,
    relative: &Path,
    depth: usize,
    state: &mut CheckoutState,
) -> Result<()> {
    if depth > MAX_TREE_DEPTH {
        return Err(Error::Rejected("albero Git troppo profondo".into()));
    }
    for entry in tree.iter() {
        let name = entry
            .name()
            .map_err(|_| Error::Rejected("nome di file Git non UTF-8".into()))?;
        validate_component(name)?;
        let child = relative.join(name);
        let kind = entry
            .kind()
            .ok_or_else(|| Error::Rejected("entry Git senza tipo".into()))?;

        match kind {
            ObjectType::Tree => {
                let object = entry.to_object(repo)?;
                let subtree = object
                    .as_tree()
                    .ok_or_else(|| Error::Rejected("albero Git non valido".into()))?;
                walk_tree(repo, root, subtree, &child, depth + 1, state)?;
            }
            ObjectType::Blob => {
                let mode = entry.filemode();
                if mode == 0o120000 {
                    return Err(Error::Rejected(format!(
                        "il repository contiene un symlink (rifiutato): {}",
                        child.display()
                    )));
                }
                if mode == 0o160000 {
                    return Err(Error::Rejected(format!(
                        "il repository contiene un submodule (rifiutato): {}",
                        child.display()
                    )));
                }
                if mode != 0o100644 && mode != 0o100755 {
                    return Err(Error::Rejected(format!(
                        "modalità Git non supportata ({mode:o}): {}",
                        child.display()
                    )));
                }
                let object = entry.to_object(repo)?;
                let blob = object
                    .as_blob()
                    .ok_or_else(|| Error::Rejected("blob Git non valido".into()))?;
                let len = blob.content().len() as u64;
                if len > MAX_BLOB_BYTES {
                    return Err(Error::Rejected(format!(
                        "blob troppo grande ({} byte): {}",
                        len,
                        child.display()
                    )));
                }
                state.total_bytes += len;
                if state.total_bytes > MAX_CHECKOUT_BYTES {
                    return Err(Error::Rejected("checkout troppo grande".into()));
                }
                if state.files.len() >= MAX_CHECKOUT_FILES {
                    return Err(Error::Rejected("troppi file nel checkout".into()));
                }
                let out = safe_join(root, &child)?;
                if let Some(parent) = out.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                atomic_write(&out, blob.content())?;
                state.files.push(child);
            }
            _ => {
                return Err(Error::Rejected(format!(
                    "tipo di entry Git non supportato: {}",
                    child.display()
                )))
            }
        }
    }
    Ok(())
}

fn remove_stale(
    repo: &Repository,
    root: &Path,
    old_tree: &Tree<'_>,
    new_tree: &Tree<'_>,
) -> Result<()> {
    let mut old_paths = HashSet::new();
    let mut new_paths = HashSet::new();
    collect_paths(repo, old_tree, Path::new(""), 0, &mut old_paths);
    collect_paths(repo, new_tree, Path::new(""), 0, &mut new_paths);

    for relative in old_paths {
        if new_paths.contains(&relative) {
            continue;
        }
        let Ok(target) = safe_join(root, &relative) else {
            continue;
        };
        match std::fs::symlink_metadata(&target) {
            Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => {
                std::fs::remove_file(&target)?;
            }
            _ => continue,
        }
        // Rimuove le cartelle rimaste vuote, senza mai uscire dal vault.
        let mut parent = target.parent().map(Path::to_path_buf);
        while let Some(dir) = parent {
            if dir == root || !dir.starts_with(root) {
                break;
            }
            match std::fs::read_dir(&dir) {
                Ok(mut entries) => {
                    if entries.next().is_some() {
                        break;
                    }
                    if std::fs::remove_dir(&dir).is_err() {
                        break;
                    }
                    parent = dir.parent().map(Path::to_path_buf);
                }
                Err(_) => break,
            }
        }
    }
    Ok(())
}

fn collect_paths(
    repo: &Repository,
    tree: &Tree<'_>,
    relative: &Path,
    depth: usize,
    out: &mut HashSet<PathBuf>,
) {
    if depth > MAX_TREE_DEPTH {
        return;
    }
    for entry in tree.iter() {
        let Ok(name) = entry.name() else { continue };
        if validate_component(name).is_err() {
            continue;
        }
        let child = relative.join(name);
        match entry.kind() {
            Some(ObjectType::Tree) => {
                if let Ok(object) = entry.to_object(repo) {
                    if let Some(subtree) = object.as_tree() {
                        collect_paths(repo, subtree, &child, depth + 1, out);
                    }
                }
            }
            Some(ObjectType::Blob) => {
                out.insert(child);
            }
            _ => {}
        }
    }
}

fn short_oid(oid: git2::Oid) -> String {
    let full = oid.to_string();
    full[..7.min(full.len())].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::Signature;

    fn identity() -> Identity {
        Identity::new("Tizio", "tizio@example.com").unwrap()
    }

    fn signature() -> Signature<'static> {
        Signature::now("Tizio", "tizio@example.com").unwrap()
    }

    /// Crea un commit sopra `base` scrivendo solo nell'object database:
    /// il working tree resta quello che il test decide.
    fn make_commit<'a>(
        repo: &'a Repository,
        base: Option<&Commit<'_>>,
        files: &[(&str, &str)],
        remove: &[&str],
    ) -> Commit<'a> {
        let mut index = repo.index().unwrap();
        if let Some(base) = base {
            index.read_tree(&base.tree().unwrap()).unwrap();
        }
        for relative in remove {
            index.remove_path(Path::new(relative)).unwrap();
        }
        for (relative, content) in files {
            let blob = repo.blob(content.as_bytes()).unwrap();
            let entry = git2::IndexEntry {
                ctime: git2::IndexTime::new(0, 0),
                mtime: git2::IndexTime::new(0, 0),
                dev: 0,
                ino: 0,
                mode: 0o100644,
                uid: 0,
                gid: 0,
                file_size: content.len() as u32,
                id: blob,
                flags: 0,
                flags_extended: 0,
                path: relative.as_bytes().to_vec(),
            };
            index.add(&entry).unwrap();
        }
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let parents: Vec<&Commit> = base.into_iter().collect();
        let oid = repo
            .commit(None, &signature(), &signature(), "test", &tree, &parents)
            .unwrap();
        repo.find_commit(oid).unwrap()
    }

    fn set_local_remote(repo: &Repository, path: &Path) {
        repo.remote(REMOTE, path.to_str().unwrap()).unwrap();
    }

    #[test]
    fn init_commit_e_nessuna_modifica() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("vault");
        std::fs::create_dir_all(&root).unwrap();
        assert!(!is_repo(&root));

        init(&root, &identity()).unwrap();
        assert!(is_repo(&root));
        assert!(matches!(init(&root, &identity()), Err(Error::Rejected(_))));

        std::fs::create_dir_all(root.join("notebooks/nb-a/pages")).unwrap();
        std::fs::write(root.join("notebooks/nb-a/pages/pg.json"), b"{}").unwrap();
        let report = commit_all(&root, "primo commit").unwrap();
        assert_eq!(report.short.len(), 7);

        let status = status(&root).unwrap();
        assert!(status.initialized);
        assert_eq!(status.branch.as_deref(), Some("main"));
        assert!(!status.dirty);
        assert_eq!(status.files, 0);

        assert!(matches!(
            commit_all(&root, "di nuovo"),
            Err(Error::Rejected(_))
        ));
    }

    #[test]
    fn primo_pull_dal_remote_e_fast_forward() {
        let dir = tempfile::tempdir().unwrap();
        let origin = dir.path().join("origin");
        let work = dir.path().join("vault");
        std::fs::create_dir_all(&origin).unwrap();
        std::fs::create_dir_all(&work).unwrap();

        let origin_repo = Repository::init(&origin).unwrap();
        origin_repo.set_head("refs/heads/main").unwrap();
        let first = make_commit(
            &origin_repo,
            None,
            &[
                ("inkpad.json", "{\"schema\":1,\"notebooks\":[\"nb-a\"]}"),
                ("notebooks/nb-a/pages/pg-1.json", "{}"),
            ],
            &[],
        );
        origin_repo.branch("main", &first, true).unwrap();

        init(&work, &identity()).unwrap();
        let work_repo = Repository::open(&work).unwrap();
        set_local_remote(&work_repo, &origin);

        let report = pull(&work, None).unwrap();
        assert_eq!(report.outcome, "initial");
        assert_eq!(report.branch, "main");
        assert!(work.join("notebooks/nb-a/pages/pg-1.json").is_file());

        // Secondo pull: niente da fare.
        let second = pull(&work, None).unwrap();
        assert_eq!(second.outcome, "up_to_date");

        // Il remote avanza e rimuove un file: fast-forward con pulizia.
        let head = origin_repo.head().unwrap().peel_to_commit().unwrap();
        let second_commit = make_commit(
            &origin_repo,
            Some(&head),
            &[("inkpad.json", "{\"schema\":1,\"notebooks\":[]}")],
            &["notebooks/nb-a/pages/pg-1.json"],
        );
        origin_repo
            .reference("refs/heads/main", second_commit.id(), true, "test")
            .unwrap();

        let third = pull(&work, None).unwrap();
        assert_eq!(third.outcome, "fast_forward");
        assert!(
            !work.join("notebooks/nb-a/pages/pg-1.json").exists(),
            "file obsoleto non rimosso"
        );
        assert_eq!(
            std::fs::read_to_string(work.join("inkpad.json")).unwrap(),
            "{\"schema\":1,\"notebooks\":[]}"
        );
    }

    #[test]
    fn push_verso_un_bare_locale() {
        let dir = tempfile::tempdir().unwrap();
        let bare = dir.path().join("bare.git");
        let work = dir.path().join("vault");
        std::fs::create_dir_all(&work).unwrap();
        Repository::init_bare(&bare).unwrap();

        init(&work, &identity()).unwrap();
        std::fs::write(work.join("inkpad.json"), b"{\"schema\":1,\"notebooks\":[]}").unwrap();
        commit_all(&work, "base").unwrap();

        let repo = Repository::open(&work).unwrap();
        repo.remote(REMOTE, bare.to_str().unwrap()).unwrap();
        let report = push(&work, None).unwrap();
        assert_eq!(report.branch, "main");

        let bare_repo = Repository::open(&bare).unwrap();
        let reference = bare_repo.find_reference("refs/heads/main").unwrap();
        assert!(reference.target().is_some());
    }

    #[test]
    fn pull_rifiutato_con_modifiche_non_committate() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("vault");
        std::fs::create_dir_all(&root).unwrap();
        init(&root, &identity()).unwrap();
        std::fs::create_dir_all(root.join("notebooks")).unwrap();
        std::fs::write(root.join("inkpad.json"), b"{\"schema\":1,\"notebooks\":[]}").unwrap();
        commit_all(&root, "base").unwrap();
        std::fs::write(
            root.join("inkpad.json"),
            b"{\"schema\":1,\"notebooks\":[\"x\"]}",
        )
        .unwrap();

        let err = pull(&root, None).unwrap_err();
        assert!(
            matches!(err, Error::Rejected(_)),
            "atteso rifiuto, ottenuto {err:?}"
        );
    }

    #[test]
    fn pull_rifiuta_configurazione_con_credential_helper() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("vault");
        std::fs::create_dir_all(&root).unwrap();
        init(&root, &identity()).unwrap();
        let repo = Repository::open(&root).unwrap();
        repo.config()
            .unwrap()
            .set_str("credential.helper", "!touch /tmp/inkpad-pwned")
            .unwrap();
        let err = pull(&root, None).unwrap_err();
        assert!(matches!(err, Error::Rejected(_)));
    }

    #[cfg(unix)]
    #[test]
    fn rifiuta_symlink_nel_checkout() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("vault");
        std::fs::create_dir_all(&root).unwrap();
        let repo = Repository::init(&root).unwrap();
        let blob = repo.blob(b"/etc/passwd").unwrap();
        let mut builder = repo.treebuilder(None).unwrap();
        builder.insert("evil", blob, 0o120000).unwrap();
        let tree_id = builder.write().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let signature = Signature::now("Tizio", "tizio@example.com").unwrap();
        repo.commit(None, &signature, &signature, "symlink", &tree, &[])
            .unwrap();

        let err = materialize(&repo, &root, &tree).unwrap_err();
        assert!(matches!(err, Error::Rejected(_)));
        assert!(!root.join("evil").exists());
    }

    #[test]
    fn rifiuta_traversal_negli_alberi() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("vault");
        std::fs::create_dir_all(&root).unwrap();
        let repo = Repository::init(&root).unwrap();
        let blob = repo.blob(b"pwn").unwrap();
        let mut builder = repo.treebuilder(None).unwrap();
        let inserted = builder.insert("../escape", blob, 0o100644).is_ok();
        let tree_id = builder.write().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();

        if inserted {
            assert!(materialize(&repo, &root, &tree).is_err());
        }
        assert!(!dir.path().join("escape").exists());
    }

    #[test]
    fn merge_senza_conflitti_e_conflitto_senza_scritture() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("vault");
        std::fs::create_dir_all(&root).unwrap();
        let repo = Repository::init(&root).unwrap();
        repo.set_head("refs/heads/main").unwrap();

        let base = make_commit(&repo, None, &[("notebooks/a.json", "base")], &[]);
        repo.branch("main", &base, true).unwrap();
        repo.set_head("refs/heads/main").unwrap();

        // Divergenza non conflittuale: locale modifica a.json, remoto aggiunge b.json.
        let local = make_commit(&repo, Some(&base), &[("notebooks/a.json", "locale")], &[]);
        repo.reference("refs/heads/main", local.id(), true, "test")
            .unwrap();
        repo.set_head("refs/heads/main").unwrap();
        let remote = make_commit(&repo, Some(&base), &[("notebooks/b.json", "remoto")], &[]);
        std::fs::create_dir_all(root.join("notebooks")).unwrap();
        std::fs::write(root.join("notebooks/a.json"), "locale").unwrap();

        let head = merge(&repo, &root, "main", &local, &remote).unwrap();
        assert_eq!(head.len(), 7);
        assert_eq!(
            std::fs::read_to_string(root.join("notebooks/a.json")).unwrap(),
            "locale"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("notebooks/b.json")).unwrap(),
            "remoto"
        );
        let head_commit = repo.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(head_commit.parent_count(), 2);

        // Divergenza conflittuale: stesso file, contenuti diversi.
        let base2 = head_commit;
        let local2 = make_commit(&repo, Some(&base2), &[("notebooks/a.json", "uno")], &[]);
        repo.reference("refs/heads/main", local2.id(), true, "test")
            .unwrap();
        repo.set_head("refs/heads/main").unwrap();
        let remote2 = make_commit(&repo, Some(&base2), &[("notebooks/a.json", "due")], &[]);
        std::fs::write(root.join("notebooks/a.json"), "uno").unwrap();

        let err = merge(&repo, &root, "main", &local2, &remote2).unwrap_err();
        match err {
            Error::Conflict(paths) => assert!(paths.contains("notebooks/a.json")),
            other => panic!("atteso conflitto, ottenuto {other:?}"),
        }
        assert_eq!(
            std::fs::read_to_string(root.join("notebooks/a.json")).unwrap(),
            "uno",
            "il merge in conflitto non deve scrivere sul disco"
        );
        let head_after = repo.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(head_after.id(), local2.id());
    }
}
