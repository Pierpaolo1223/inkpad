//! Vault: la cartella che contiene quaderni e pagine. È anche la radice del
//! repository Git, quindi ogni percorso qui è relativo e validato.

use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

use crate::error::{Error, Result};
use crate::ids::new_id;
use crate::model::{
    now_ms, to_json_bytes, validate_title, Manifest, Notebook, NotebookSummary, NotebookUi, Page,
    Paper,
};
use crate::paths::{atomic_write, ensure_root_dir, read_limited, safe_join};

pub const MANIFEST_FILE: &str = "inkpad.json";
pub const NOTEBOOKS_DIR: &str = "notebooks";
const MAX_META_BYTES: u64 = 512 * 1024;
const MAX_PAGE_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Vault {
    root: PathBuf,
}

impl Vault {
    /// Apre il vault, creandolo se non esiste. La radice non può essere un
    /// symlink: il vault non deve poter puntare fuori da sé stesso.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        ensure_root_dir(&root)?;
        let vault = Self { root };
        let manifest_path = vault.manifest_path()?;
        if !manifest_path.exists() {
            vault.ensure_layout()?;
            vault.write_manifest(&Manifest::empty())?;
        } else {
            let manifest: Manifest = read_json(&manifest_path, MAX_META_BYTES)?;
            manifest.validate()?;
            vault.ensure_layout()?;
        }
        Ok(vault)
    }

    /// Apre solo un vault già esistente. Non crea la cartella né il manifesto:
    /// serve al selettore, che deve poter distinguere una cartella qualsiasi da
    /// un vault Inkpad prima di cambiare la sessione.
    pub fn open_existing(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        if !root.join(MANIFEST_FILE).exists() {
            return Err(Error::VaultMissing);
        }
        Self::open(root)
    }

    /// Crea un vault nuovo solo in una cartella vuota. Su una cartella con un
    /// manifesto valido si comporta come `open_existing`; su una cartella con
    /// altri file rifiuta, per non spargere `notebooks/` in una directory
    /// scelta per errore.
    pub fn create_in_empty(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        if !root.is_dir() {
            return Err(Error::Invalid("la cartella del vault non esiste".into()));
        }
        ensure_root_dir(&root)?;
        if root.join(MANIFEST_FILE).exists() {
            return Self::open(root);
        }
        let mut entries = std::fs::read_dir(&root)?;
        if entries.next().is_some() {
            return Err(Error::Rejected("la cartella non è vuota".into()));
        }
        Self::open(root)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn list_notebooks(&self) -> Result<Vec<NotebookSummary>> {
        let manifest = self.read_manifest()?;
        let mut out = Vec::with_capacity(manifest.notebooks.len());
        for id in &manifest.notebooks {
            // Un quaderno citato e assente non deve impedire di aprire gli
            // altri: la sincronizzazione può aver portato uno stato parziale.
            if let Ok(nb) = self.load_notebook(id) {
                out.push(NotebookSummary {
                    id: nb.id,
                    title: nb.title,
                    pages: nb.pages.len(),
                    updated_at: nb.updated_at,
                });
            }
        }
        out.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| a.title.cmp(&b.title))
        });
        Ok(out)
    }

    pub fn create_notebook(&self, title: &str) -> Result<Notebook> {
        validate_title(title)?;
        let mut manifest = self.read_manifest()?;
        if manifest.notebooks.len() >= crate::model::MAX_NOTEBOOKS {
            return Err(Error::Rejected("troppi quaderni".into()));
        }
        let id = new_id("nb");
        let notebook = Notebook::new(id, title.trim().to_string());
        self.ensure_notebook_dirs(&notebook.id)?;
        self.write_notebook(&notebook)?;
        manifest.notebooks.push(notebook.id.clone());
        self.write_manifest(&manifest)?;
        Ok(notebook)
    }

    pub fn load_notebook(&self, id: &str) -> Result<Notebook> {
        let path = self.notebook_path(id)?;
        let nb: Notebook = read_json(&path, MAX_META_BYTES)?;
        nb.validate()?;
        if nb.id != id {
            return Err(Error::Rejected(
                "identificativo del quaderno incoerente".into(),
            ));
        }
        Ok(nb)
    }

    pub fn create_page(&self, notebook_id: &str, paper: Paper) -> Result<Page> {
        let mut notebook = self.load_notebook(notebook_id)?;
        if notebook.pages.len() >= crate::model::MAX_PAGES_PER_NOTEBOOK {
            return Err(Error::Rejected("troppe pagine nel quaderno".into()));
        }
        let page = Page::new(new_id("pg"), paper);
        self.ensure_pages_dir(notebook_id)?;
        self.write_page(notebook_id, &page)?;
        notebook.pages.push(page.id.clone());
        notebook.updated_at = now_ms();
        self.write_notebook(&notebook)?;
        Ok(page)
    }

    pub fn load_page(&self, notebook_id: &str, page_id: &str) -> Result<Page> {
        let notebook = self.load_notebook(notebook_id)?;
        if !notebook.pages.iter().any(|id| id == page_id) {
            return Err(Error::NotFound(format!(
                "la pagina {page_id} non è in questo quaderno"
            )));
        }
        let path = self.page_path(notebook_id, page_id)?;
        let page: Page = read_json(&path, MAX_PAGE_BYTES)?;
        page.validate()?;
        if page.id != page_id {
            return Err(Error::Rejected(
                "identificativo della pagina incoerente".into(),
            ));
        }
        Ok(page)
    }

    /// Salva una pagina già appartenente al quaderno: `updated_at` lo decide
    /// il core, non il client.
    pub fn save_page(&self, notebook_id: &str, mut page: Page) -> Result<Page> {
        page.validate()?;
        let mut notebook = self.load_notebook(notebook_id)?;
        if !notebook.pages.iter().any(|id| id == &page.id) {
            return Err(Error::NotFound(format!(
                "la pagina {} non è in questo quaderno",
                page.id
            )));
        }
        page.updated_at = now_ms();
        self.write_page(notebook_id, &page)?;
        notebook.updated_at = page.updated_at;
        self.write_notebook(&notebook)?;
        Ok(page)
    }

    /// Salva strumento e colori dell'ultimo uso.
    ///
    /// Non tocca `updated_at`: non è una modifica del contenuto, quindi
    /// scegliere un colore non riordina la lista dei quaderni. Il file cambia
    /// comunque, così la preferenza entra nel prossimo commit e arriva sugli
    /// altri dispositivi insieme al quaderno.
    pub fn save_notebook_ui(&self, notebook_id: &str, ui: NotebookUi) -> Result<Notebook> {
        ui.validate()?;
        let mut notebook = self.load_notebook(notebook_id)?;
        if notebook.ui.as_ref() == Some(&ui) {
            return Ok(notebook);
        }
        notebook.ui = Some(ui);
        self.write_notebook(&notebook)?;
        Ok(notebook)
    }

    pub fn delete_page(&self, notebook_id: &str, page_id: &str) -> Result<()> {
        let mut notebook = self.load_notebook(notebook_id)?;
        let position = notebook
            .pages
            .iter()
            .position(|id| id == page_id)
            .ok_or_else(|| {
                Error::NotFound(format!("la pagina {page_id} non è in questo quaderno"))
            })?;
        let path = self.page_path(notebook_id, page_id)?;
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }
        notebook.pages.remove(position);
        notebook.updated_at = now_ms();
        self.write_notebook(&notebook)?;
        Ok(())
    }

    /// Elimina un quaderno, le sue pagine e la voce nel manifesto.
    ///
    /// Il percorso passa da `safe_join`: l'id non può uscire dal vault e la
    /// cartella non può essere un symlink. Se i file sono già spariti, la voce
    /// nel manifesto viene comunque rimossa, così un quaderno orfano non resta
    /// per sempre nella lista.
    pub fn delete_notebook(&self, notebook_id: &str) -> Result<()> {
        let mut manifest = self.read_manifest()?;
        let position = manifest
            .notebooks
            .iter()
            .position(|id| id == notebook_id)
            .ok_or_else(|| Error::NotFound(format!("il quaderno {notebook_id} non esiste")))?;
        let dir = safe_join(&self.root, &Path::new(NOTEBOOKS_DIR).join(notebook_id))?;
        match std::fs::symlink_metadata(&dir) {
            Ok(meta) => {
                if !meta.is_dir() || meta.file_type().is_symlink() {
                    return Err(Error::Rejected("il quaderno non è una cartella".into()));
                }
                std::fs::remove_dir_all(&dir)?;
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }
        manifest.notebooks.remove(position);
        self.write_manifest(&manifest)?;
        Ok(())
    }

    // --- percorsi -------------------------------------------------------

    fn manifest_path(&self) -> Result<PathBuf> {
        safe_join(&self.root, Path::new(MANIFEST_FILE))
    }

    fn notebook_path(&self, notebook_id: &str) -> Result<PathBuf> {
        safe_join(
            &self.root,
            &Path::new(NOTEBOOKS_DIR)
                .join(notebook_id)
                .join("notebook.json"),
        )
    }

    fn pages_dir(&self, notebook_id: &str) -> Result<PathBuf> {
        safe_join(
            &self.root,
            &Path::new(NOTEBOOKS_DIR).join(notebook_id).join("pages"),
        )
    }

    fn page_path(&self, notebook_id: &str, page_id: &str) -> Result<PathBuf> {
        Ok(self.pages_dir(notebook_id)?.join(format!("{page_id}.json")))
    }

    fn ensure_layout(&self) -> Result<()> {
        let notebooks = safe_join(&self.root, Path::new(NOTEBOOKS_DIR))?;
        std::fs::create_dir_all(&notebooks)?;
        Ok(())
    }

    fn ensure_notebook_dirs(&self, notebook_id: &str) -> Result<()> {
        let pages = self.pages_dir(notebook_id)?;
        std::fs::create_dir_all(&pages)?;
        Ok(())
    }

    fn ensure_pages_dir(&self, notebook_id: &str) -> Result<()> {
        let pages = self.pages_dir(notebook_id)?;
        std::fs::create_dir_all(&pages)?;
        Ok(())
    }

    // --- I/O ------------------------------------------------------------

    fn read_manifest(&self) -> Result<Manifest> {
        let manifest: Manifest = read_json(&self.manifest_path()?, MAX_META_BYTES)?;
        manifest.validate()?;
        Ok(manifest)
    }

    fn write_manifest(&self, manifest: &Manifest) -> Result<()> {
        manifest.validate()?;
        atomic_write(&self.manifest_path()?, &to_json_bytes(manifest)?)
    }

    fn write_notebook(&self, notebook: &Notebook) -> Result<()> {
        notebook.validate()?;
        atomic_write(
            &self.notebook_path(&notebook.id)?,
            &to_json_bytes(notebook)?,
        )
    }

    fn write_page(&self, notebook_id: &str, page: &Page) -> Result<()> {
        page.validate()?;
        let path = self.page_path(notebook_id, &page.id)?;
        atomic_write(&path, &to_json_bytes(page)?)
    }
}

fn read_json<T: DeserializeOwned>(path: &Path, max_bytes: u64) -> Result<T> {
    let bytes = read_limited(path, max_bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::UiTool;

    fn vault() -> (tempfile::TempDir, Vault) {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::open(dir.path().join("vault")).unwrap();
        (dir, vault)
    }

    #[test]
    fn apre_solo_un_vault_esistente() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("vault");
        std::fs::create_dir_all(&root).unwrap();
        assert!(matches!(
            Vault::open_existing(&root),
            Err(Error::VaultMissing)
        ));

        Vault::open(&root).unwrap();
        let reopened = Vault::open_existing(&root).unwrap();
        assert!(reopened.list_notebooks().unwrap().is_empty());
    }

    #[test]
    fn crea_un_vault_solo_in_una_cartella_vuota() {
        let dir = tempfile::tempdir().unwrap();
        let occupata = dir.path().join("occupata");
        std::fs::create_dir_all(&occupata).unwrap();
        std::fs::write(occupata.join("altro.txt"), b"x").unwrap();
        assert!(matches!(
            Vault::create_in_empty(&occupata),
            Err(Error::Rejected(_))
        ));

        let vuota = dir.path().join("vuota");
        std::fs::create_dir_all(&vuota).unwrap();
        Vault::create_in_empty(&vuota).unwrap();
        assert!(vuota.join(MANIFEST_FILE).is_file());
    }

    #[test]
    fn ciclo_completo_quaderno_pagina() {
        let (_dir, vault) = vault();
        let nb = vault.create_notebook("Appunti").unwrap();
        assert_eq!(vault.list_notebooks().unwrap().len(), 1);

        let mut page = vault.create_page(&nb.id, Paper::Ruled).unwrap();
        page.text = "prima riga".into();
        let saved = vault.save_page(&nb.id, page).unwrap();
        assert!(saved.updated_at > 0);

        let loaded = vault.load_page(&nb.id, &saved.id).unwrap();
        assert_eq!(loaded.text, "prima riga");
        assert_eq!(vault.load_notebook(&nb.id).unwrap().pages.len(), 1);

        vault.delete_page(&nb.id, &saved.id).unwrap();
        assert!(vault.load_page(&nb.id, &saved.id).is_err());
        assert!(vault.load_notebook(&nb.id).unwrap().pages.is_empty());
    }

    #[test]
    fn preferenze_del_quaderno_restano_nel_suo_file() {
        let (_dir, vault) = vault();
        let nb = vault.create_notebook("A").unwrap();
        assert!(vault.load_notebook(&nb.id).unwrap().ui.is_none());

        let ui = NotebookUi {
            tool: UiTool::Marker,
            pen_color: "#1f2933".into(),
            marker_color: "#1d4ed8".into(),
        };
        let saved = vault.save_notebook_ui(&nb.id, ui.clone()).unwrap();
        assert_eq!(saved.updated_at, nb.updated_at, "la preferenza non è contenuto");
        assert_eq!(vault.load_notebook(&nb.id).unwrap().ui, Some(ui.clone()));

        // Riscrivere la stessa preferenza non tocca il file: nessun diff inutile.
        let path = vault.notebook_path(&nb.id).unwrap();
        let before = std::fs::read(&path).unwrap();
        vault.save_notebook_ui(&nb.id, ui.clone()).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), before);

        let bad = NotebookUi {
            tool: UiTool::Pen,
            pen_color: "red".into(),
            marker_color: "#f2c200".into(),
        };
        assert!(vault.save_notebook_ui(&nb.id, bad).is_err());
        assert_eq!(vault.load_notebook(&nb.id).unwrap().ui, Some(ui));
    }

    #[test]
    fn non_salva_una_pagina_di_un_altro_quaderno() {
        let (_dir, vault) = vault();
        let a = vault.create_notebook("A").unwrap();
        let b = vault.create_notebook("B").unwrap();
        let page = vault.create_page(&a.id, Paper::Plain).unwrap();
        assert!(vault.save_page(&b.id, page).is_err());
    }

    #[test]
    fn rifiuta_id_di_pagina_con_traversal() {
        let (_dir, vault) = vault();
        let nb = vault.create_notebook("A").unwrap();
        assert!(vault.load_page(&nb.id, "../../inkpad").is_err());
        assert!(vault.load_notebook("../../../etc").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rifiuta_cartella_pagine_symlink() {
        use std::os::unix::fs::symlink;
        let (dir, vault) = vault();
        let nb = vault.create_notebook("A").unwrap();
        let page = vault.create_page(&nb.id, Paper::Plain).unwrap();
        let pages = vault.pages_dir(&nb.id).unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::remove_dir_all(&pages).unwrap();
        symlink(&outside, &pages).unwrap();

        let err = vault.save_page(&nb.id, page).unwrap_err();
        assert!(
            matches!(err, Error::Rejected(_)),
            "atteso rifiuto, ottenuto {err:?}"
        );
    }

    #[test]
    fn un_file_manifest_corrotto_non_viene_riscritto() {
        let (dir, _vault) = vault();
        let manifest = dir.path().join("vault").join(MANIFEST_FILE);
        std::fs::write(&manifest, b"{ not json").unwrap();
        let reopened = Vault::open(dir.path().join("vault"));
        assert!(reopened.is_err());
        assert_eq!(std::fs::read(&manifest).unwrap(), b"{ not json");
    }

    #[test]
    fn elimina_quaderno_e_le_sue_pagine() {
        let (_dir, vault) = vault();
        let keep = vault.create_notebook("Da tenere").unwrap();
        let gone = vault.create_notebook("Da eliminare").unwrap();
        vault.create_page(&gone.id, Paper::Ruled).unwrap();
        let gone_dir = vault.root().join("notebooks").join(&gone.id);
        assert!(gone_dir.is_dir());

        vault.delete_notebook(&gone.id).unwrap();

        assert!(!gone_dir.exists());
        let ids: Vec<_> = vault
            .list_notebooks()
            .unwrap()
            .into_iter()
            .map(|notebook| notebook.id)
            .collect();
        assert_eq!(ids, vec![keep.id.clone()]);
        assert!(vault.load_notebook(&gone.id).is_err());
        assert!(vault.delete_notebook(&gone.id).is_err());
    }

    #[test]
    fn elimina_quaderno_orfano_pulisce_il_manifesto() {
        let (_dir, vault) = vault();
        let orphan = vault.create_notebook("Orfano").unwrap();
        std::fs::remove_dir_all(vault.root().join("notebooks").join(&orphan.id)).unwrap();

        vault.delete_notebook(&orphan.id).unwrap();

        assert!(vault.list_notebooks().unwrap().is_empty());
    }
}
