//! Guscio Tauri: sola traduzione fra IPC e `inkpad-core`.
//!
//! Regole di questo strato:
//! * nessun plugin filesystem/shell/http: l'unico I/O è nei comandi qui sotto;
//! * il token Git vive in memoria (`Zeroizing<String>`), non viene mai scritto
//!   su disco né restituito al frontend;
//! * una sola sincronizzazione alla volta (`sync: Mutex<()>`), così un pull non
//!   corre mentre il frontend salva una pagina.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use inkpad_core::git::{self, CommitReport, GitStatus, PullReport, PushReport, RemoteInfo};
use inkpad_core::{
    Error as CoreError, Identity, Notebook, NotebookSummary, NotebookUi, Page, Paper, RemoteUrl,
    UiTool, Vault,
};
use serde::Serialize;
use tauri::{Manager, State};
use zeroize::Zeroizing;

struct Session {
    root: PathBuf,
    token: Mutex<Zeroizing<String>>,
    sync: Mutex<()>,
}

#[derive(Debug, Serialize)]
struct AppError {
    code: &'static str,
    message: String,
}

impl From<CoreError> for AppError {
    fn from(error: CoreError) -> Self {
        Self {
            code: error.code(),
            message: error.to_string(),
        }
    }
}

impl From<std::io::Error> for AppError {
    fn from(error: std::io::Error) -> Self {
        Self {
            code: "io",
            message: error.to_string(),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VaultView {
    path: String,
    notebooks: Vec<NotebookSummary>,
}

fn session_token(session: &Session) -> MutexGuard<'_, Zeroizing<String>> {
    session
        .token
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn begin_sync(session: &Session) -> Result<MutexGuard<'_, ()>, AppError> {
    session
        .sync
        .try_lock()
        .map_err(|_| AppError::from(CoreError::Busy))
}

// --- vault e pagine -------------------------------------------------------

#[tauri::command]
fn vault_open(session: State<'_, Session>) -> Result<VaultView, AppError> {
    let vault = Vault::open(session.root.clone())?;
    Ok(VaultView {
        path: session.root.to_string_lossy().to_string(),
        notebooks: vault.list_notebooks()?,
    })
}

#[tauri::command]
fn notebook_create(session: State<'_, Session>, title: String) -> Result<Notebook, AppError> {
    Ok(Vault::open(session.root.clone())?.create_notebook(&title)?)
}

#[tauri::command]
fn notebook_load(session: State<'_, Session>, notebook_id: String) -> Result<Notebook, AppError> {
    Ok(Vault::open(session.root.clone())?.load_notebook(&notebook_id)?)
}

/// Strumento e colori sono preferenze del quaderno: viaggiano con il suo file
/// (e quindi con i commit Git) invece di restare sulla singola macchina.
#[tauri::command]
fn notebook_set_ui(
    session: State<'_, Session>,
    notebook_id: String,
    tool: UiTool,
    pen_color: String,
    marker_color: String,
) -> Result<Notebook, AppError> {
    let ui = NotebookUi {
        tool,
        pen_color,
        marker_color,
    };
    Ok(Vault::open(session.root.clone())?.save_notebook_ui(&notebook_id, ui)?)
}

#[tauri::command]
fn page_create(
    session: State<'_, Session>,
    notebook_id: String,
    paper: Paper,
) -> Result<Page, AppError> {
    Ok(Vault::open(session.root.clone())?.create_page(&notebook_id, paper)?)
}

#[tauri::command]
fn page_load(
    session: State<'_, Session>,
    notebook_id: String,
    page_id: String,
) -> Result<Page, AppError> {
    Ok(Vault::open(session.root.clone())?.load_page(&notebook_id, &page_id)?)
}

#[tauri::command]
fn page_save(
    session: State<'_, Session>,
    notebook_id: String,
    page: Page,
) -> Result<Page, AppError> {
    Ok(Vault::open(session.root.clone())?.save_page(&notebook_id, page)?)
}

#[tauri::command]
fn page_delete(
    session: State<'_, Session>,
    notebook_id: String,
    page_id: String,
) -> Result<(), AppError> {
    Vault::open(session.root.clone())?.delete_page(&notebook_id, &page_id)?;
    Ok(())
}

// --- git ----------------------------------------------------------------

#[tauri::command]
fn git_status(session: State<'_, Session>) -> Result<GitStatus, AppError> {
    Ok(git::status(&session.root)?)
}

#[tauri::command]
fn git_remote_info(session: State<'_, Session>) -> Result<RemoteInfo, AppError> {
    Ok(git::remote_info(&session.root)?)
}

#[tauri::command]
fn git_init(session: State<'_, Session>, name: String, email: String) -> Result<(), AppError> {
    let identity = Identity::new(&name, &email)?;
    git::init(&session.root, &identity)?;
    Ok(())
}

#[tauri::command]
fn git_set_remote(session: State<'_, Session>, url: String) -> Result<(), AppError> {
    let url = RemoteUrl::parse(&url)?;
    git::set_remote(&session.root, &url)?;
    Ok(())
}

#[tauri::command]
fn git_commit(session: State<'_, Session>, message: String) -> Result<CommitReport, AppError> {
    let _guard = begin_sync(&session)?;
    Ok(git::commit_all(&session.root, &message)?)
}

#[tauri::command]
fn git_pull(session: State<'_, Session>) -> Result<PullReport, AppError> {
    let _guard = begin_sync(&session)?;
    let token = session_token(&session);
    let token = if token.is_empty() {
        None
    } else {
        Some(token.as_str())
    };
    Ok(git::pull(&session.root, token)?)
}

#[tauri::command]
fn git_push(session: State<'_, Session>) -> Result<PushReport, AppError> {
    let _guard = begin_sync(&session)?;
    let token = session_token(&session);
    let token = if token.is_empty() {
        None
    } else {
        Some(token.as_str())
    };
    Ok(git::push(&session.root, token)?)
}

/// Il token resta solo in memoria, sostituito in modo sicuro quando cambia.
#[tauri::command]
fn git_set_token(session: State<'_, Session>, token: String) -> Result<(), AppError> {
    if token.len() > 512 || token.chars().any(char::is_control) {
        return Err(AppError::from(CoreError::Invalid(
            "token non valido".into(),
        )));
    }
    *session_token(&session) = Zeroizing::new(token);
    Ok(())
}

#[tauri::command]
fn git_clear_token(session: State<'_, Session>) -> Result<(), AppError> {
    *session_token(&session) = Zeroizing::new(String::new());
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            app.manage(Session {
                root: data_dir.join("vault"),
                token: Mutex::new(Zeroizing::new(String::new())),
                sync: Mutex::new(()),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            vault_open,
            notebook_create,
            notebook_load,
            notebook_set_ui,
            page_create,
            page_load,
            page_save,
            page_delete,
            git_status,
            git_remote_info,
            git_init,
            git_set_remote,
            git_commit,
            git_pull,
            git_push,
            git_set_token,
            git_clear_token
        ])
        .run(tauri::generate_context!())
        .expect("errore nell'avvio di Inkpad");
}
