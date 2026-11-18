//! Inkpad core: modello, vault e sincronizzazione Git, senza dipendenze da
//! Tauri. L'app desktop/mobile è un guscio sottile sopra queste funzioni, così
//! lo stesso codice gira su Linux, macOS, Windows, iOS e Android ed è
//! verificabile con `cargo test` senza compilare una WebView.

pub mod error;
pub mod git;
pub mod ids;
pub mod model;
pub mod paths;
pub mod vault;

pub use error::{Error, Identity, RemoteUrl, Result};
pub use model::{
    Manifest, Notebook, NotebookSummary, NotebookUi, Page, Paper, Stroke, Tool, UiTool, MAX_NOTEBOOKS,
    MAX_PAGES_PER_NOTEBOOK, SCHEMA_VERSION,
};
pub use vault::Vault;
