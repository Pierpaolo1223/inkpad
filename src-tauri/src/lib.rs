//! Guscio Tauri: sola traduzione fra IPC e `inkpad-core`.
//!
//! Regole di questo strato:
//! * nessun plugin filesystem/shell/http: l'unico I/O è nei comandi qui sotto;
//! * il token Git vive in memoria (`Zeroizing<String>`), non viene mai scritto
//!   su disco né restituito al frontend;
//! * una sola sincronizzazione alla volta (`sync: Mutex<()>`), così un pull non
//!   corre mentre il frontend salva una pagina.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use inkpad_core::git::{self, CommitReport, GitStatus, PullReport, PushReport, RemoteInfo};
use inkpad_core::{
    Error as CoreError, Identity, Notebook, NotebookSummary, NotebookUi, Page, Paper, RemoteUrl,
    UiTool, Vault,
};
use serde::{Deserialize, Serialize};
use tauri::{Manager, State};
#[cfg(desktop)]
use tauri_plugin_dialog::DialogExt;
use zeroize::Zeroizing;

const SETTINGS_FILE: &str = "settings.json";

struct Session {
    root: Mutex<PathBuf>,
    token: Mutex<Zeroizing<String>>,
    sync: Mutex<()>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Settings {
    #[serde(default)]
    vault_path: Option<String>,
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

fn session_root(session: &Session) -> PathBuf {
    session
        .root
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

fn load_settings(app: &tauri::AppHandle) -> Option<Settings> {
    let path = app.path().app_config_dir().ok()?.join(SETTINGS_FILE);
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn save_settings(app: &tauri::AppHandle, root: &Path) -> Result<(), AppError> {
    let dir = app.path().app_config_dir().map_err(|error| AppError {
        code: "io",
        message: error.to_string(),
    })?;
    std::fs::create_dir_all(&dir)?;
    let settings = Settings {
        vault_path: Some(root.to_string_lossy().to_string()),
    };
    let bytes = serde_json::to_vec_pretty(&settings).map_err(CoreError::Json)?;
    std::fs::write(dir.join(SETTINGS_FILE), bytes)?;
    Ok(())
}

// --- vault e pagine -------------------------------------------------------

#[tauri::command]
fn vault_open(session: State<'_, Session>) -> Result<VaultView, AppError> {
    let root = session_root(&session);
    let vault = Vault::open(root.clone())?;
    Ok(VaultView {
        path: root.to_string_lossy().to_string(),
        notebooks: vault.list_notebooks()?,
    })
}

/// Sceglie una cartella con il selettore nativo. Il percorso non viene aperto
/// qui: il frontend deve poter chiedere conferma prima di creare un vault in una
/// cartella vuota.
#[tauri::command]
async fn vault_pick(app: tauri::AppHandle) -> Result<Option<String>, AppError> {
    #[cfg(desktop)]
    {
        let picked = app
            .dialog()
            .file()
            .set_title("Apri un vault Inkpad")
            .blocking_pick_folder();
        let Some(path) = picked else {
            return Ok(None);
        };
        let path = path.into_path().map_err(|_| AppError {
            code: "unsupported_path",
            message: "Il selettore ha restituito un percorso non gestibile.".into(),
        })?;
        Ok(Some(path.to_string_lossy().to_string()))
    }

    #[cfg(mobile)]
    {
        let _ = app;
        Err(AppError {
            code: "unsupported",
            message: "La scelta della cartella non è supportata su questa piattaforma; il vault resta nella cartella dati dell'app.".into(),
        })
    }
}

/// Cambia la radice della sessione. Un vault esistente viene aperto e validato;
/// una cartella vuota diventa un vault solo se `create` è esplicito. Il token
/// della sessione precedente viene azzerato: apparteneva a un altro repository.
#[tauri::command]
fn vault_use(
    app: tauri::AppHandle,
    session: State<'_, Session>,
    path: String,
    create: bool,
) -> Result<VaultView, AppError> {
    let _guard = begin_sync(&session)?;
    let raw = PathBuf::from(&path);
    if raw.as_os_str().is_empty() || !raw.is_absolute() {
        return Err(AppError::from(CoreError::Invalid(
            "serve un percorso assoluto della cartella".into(),
        )));
    }
    let meta = std::fs::symlink_metadata(&raw)
        .map_err(|_| AppError::from(CoreError::VaultMissing))?;
    if meta.file_type().is_symlink() {
        return Err(AppError::from(CoreError::Rejected(
            "la cartella del vault è un symlink".into(),
        )));
    }
    if !meta.is_dir() {
        return Err(AppError::from(CoreError::Invalid(
            "il percorso scelto non è una cartella".into(),
        )));
    }
    let root = std::fs::canonicalize(&raw)?;
    let vault = if create {
        Vault::create_in_empty(&root)?
    } else {
        Vault::open_existing(&root)?
    };
    let view = VaultView {
        path: root.to_string_lossy().to_string(),
        notebooks: vault.list_notebooks()?,
    };
    save_settings(&app, &root)?;
    *session
        .root
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = root;
    *session_token(&session) = Zeroizing::new(String::new());
    Ok(view)
}

#[tauri::command]
fn notebook_create(session: State<'_, Session>, title: String) -> Result<Notebook, AppError> {
    Ok(Vault::open(session_root(&session))?.create_notebook(&title)?)
}

#[tauri::command]
fn notebook_load(session: State<'_, Session>, notebook_id: String) -> Result<Notebook, AppError> {
    Ok(Vault::open(session_root(&session))?.load_notebook(&notebook_id)?)
}

#[tauri::command]
fn notebook_delete(session: State<'_, Session>, notebook_id: String) -> Result<(), AppError> {
    Vault::open(session.root.clone())?.delete_notebook(&notebook_id)?;
    Ok(())
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
    Ok(Vault::open(session_root(&session))?.save_notebook_ui(&notebook_id, ui)?)
}

#[tauri::command]
fn page_create(
    session: State<'_, Session>,
    notebook_id: String,
    paper: Paper,
) -> Result<Page, AppError> {
    Ok(Vault::open(session_root(&session))?.create_page(&notebook_id, paper)?)
}

#[tauri::command]
fn page_load(
    session: State<'_, Session>,
    notebook_id: String,
    page_id: String,
) -> Result<Page, AppError> {
    Ok(Vault::open(session_root(&session))?.load_page(&notebook_id, &page_id)?)
}

#[tauri::command]
fn page_save(
    session: State<'_, Session>,
    notebook_id: String,
    page: Page,
) -> Result<Page, AppError> {
    Ok(Vault::open(session_root(&session))?.save_page(&notebook_id, page)?)
}

#[tauri::command]
fn page_delete(
    session: State<'_, Session>,
    notebook_id: String,
    page_id: String,
) -> Result<(), AppError> {
    Vault::open(session_root(&session))?.delete_page(&notebook_id, &page_id)?;
    Ok(())
}

// --- git ----------------------------------------------------------------

#[tauri::command]
fn git_status(session: State<'_, Session>) -> Result<GitStatus, AppError> {
    Ok(git::status(&session_root(&session))?)
}

#[tauri::command]
fn git_remote_info(session: State<'_, Session>) -> Result<RemoteInfo, AppError> {
    Ok(git::remote_info(&session_root(&session))?)
}

#[tauri::command]
fn git_init(session: State<'_, Session>, name: String, email: String) -> Result<(), AppError> {
    let identity = Identity::new(&name, &email)?;
    git::init(&session_root(&session), &identity)?;
    Ok(())
}

#[tauri::command]
fn git_set_remote(session: State<'_, Session>, url: String) -> Result<(), AppError> {
    let url = RemoteUrl::parse(&url)?;
    git::set_remote(&session_root(&session), &url)?;
    Ok(())
}

#[tauri::command]
fn git_commit(session: State<'_, Session>, message: String) -> Result<CommitReport, AppError> {
    let _guard = begin_sync(&session)?;
    Ok(git::commit_all(&session_root(&session), &message)?)
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
    Ok(git::pull(&session_root(&session), token)?)
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
    Ok(git::push(&session_root(&session), token)?)
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
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let configured = load_settings(app.handle())
                .and_then(|settings| settings.vault_path)
                .map(PathBuf::from)
                .filter(|path| {
                    std::fs::symlink_metadata(path)
                        .map(|meta| meta.is_dir() && !meta.file_type().is_symlink())
                        .unwrap_or(false)
                        && path.join("inkpad.json").is_file()
                });
            app.manage(Session {
                root: Mutex::new(configured.unwrap_or_else(|| data_dir.join("vault"))),
                token: Mutex::new(Zeroizing::new(String::new())),
                sync: Mutex::new(()),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            vault_open,
            vault_pick,
            vault_use,
            notebook_create,
            notebook_load,
            notebook_delete,
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
