//! Accesso al filesystem con due invarianti:
//!
//! 1. ogni percorso è relativo alla radice del vault e viene validato
//!    componente per componente: niente `..`, niente percorsi assoluti,
//!    niente symlink (né come foglia né come antenato);
//! 2. ogni scrittura è atomica (file temporaneo + rename + fsync), così un
//!    crash a metà non lascia una pagina JSON troncata.

use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::{Error, Result};

pub const MAX_DEPTH: usize = 32;
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Nome di componente ammesso: nessun separatore, nessun `.`/`..`, nessun
/// carattere di controllo, niente `.git` (che confonderebbe il repository).
pub fn validate_component(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 128 {
        return Err(Error::Invalid("nome di percorso non valido".into()));
    }
    if name == "."
        || name == ".."
        || name.contains('\0')
        || name.contains('/')
        || name.contains('\\')
    {
        return Err(Error::Rejected("componente di percorso non ammessa".into()));
    }
    if name.chars().any(char::is_control) {
        return Err(Error::Rejected(
            "carattere di controllo nel percorso".into(),
        ));
    }
    if name.eq_ignore_ascii_case(".git") {
        return Err(Error::Rejected("il percorso non può contenere .git".into()));
    }
    Ok(())
}

/// Unisce `rel` alla radice rifiutando traversal e symlink già presenti.
pub fn safe_join(root: &Path, rel: &Path) -> Result<PathBuf> {
    if rel.as_os_str().is_empty() || rel.is_absolute() {
        return Err(Error::Invalid("percorso relativo richiesto".into()));
    }
    let mut out = root.to_path_buf();
    let mut depth = 0usize;
    for component in rel.components() {
        let Component::Normal(os) = component else {
            return Err(Error::Rejected("percorso con traversal o radice".into()));
        };
        let name = os
            .to_str()
            .ok_or_else(|| Error::Invalid("percorso non UTF-8".into()))?;
        validate_component(name)?;
        depth += 1;
        if depth > MAX_DEPTH {
            return Err(Error::Rejected("percorso troppo profondo".into()));
        }
        out.push(os);
        if let Ok(meta) = std::fs::symlink_metadata(&out) {
            if meta.file_type().is_symlink() {
                return Err(Error::Rejected(format!(
                    "symlink rifiutato nel vault: {}",
                    out.display()
                )));
            }
        }
    }
    Ok(out)
}

/// Verifica che la radice esista, sia una directory reale e non un symlink.
pub fn ensure_root_dir(root: &Path) -> Result<()> {
    if let Ok(meta) = std::fs::symlink_metadata(root) {
        if meta.file_type().is_symlink() {
            return Err(Error::Rejected("la cartella del vault è un symlink".into()));
        }
        if !meta.is_dir() {
            return Err(Error::Rejected(
                "il percorso del vault non è una cartella".into(),
            ));
        }
    } else {
        std::fs::create_dir_all(root)?;
    }
    Ok(())
}

fn sync_dir(dir: &Path) {
    // Durabilità del rename: best effort, non tutti i filesystem lo supportano.
    if let Ok(handle) = std::fs::File::open(dir) {
        let _ = handle.sync_all();
    }
}

/// Scrittura atomica: `path` non è mai osservabile a metà.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::Invalid("percorso senza cartella".into()))?;
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| Error::Invalid("nome di file non valido".into()))?;
    validate_component(file_name)?;

    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if meta.file_type().is_symlink() {
            return Err(Error::Rejected("scrittura su symlink rifiutata".into()));
        }
        if meta.is_dir() {
            return Err(Error::Invalid("il percorso è una cartella".into()));
        }
    }

    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = parent.join(format!(".{file_name}.tmp-{}-{n}", std::process::id()));

    let result = (|| -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        #[cfg(windows)]
        {
            // Su Windows rename non sostituisce un file esistente. La finestra
            // fra remove e rename è accettata (un solo processo scrive).
            if path.exists() {
                let _ = std::fs::remove_file(path);
            }
        }
        std::fs::rename(&tmp, path)?;
        sync_dir(parent);
        Ok(())
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Lettura con limite di dimensione, senza seguire symlink.
pub fn read_limited(path: &Path, max_bytes: u64) -> Result<Vec<u8>> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        return Err(Error::Rejected("lettura di un symlink rifiutata".into()));
    }
    if !meta.is_file() {
        return Err(Error::Rejected("il percorso non è un file".into()));
    }
    if meta.len() > max_bytes {
        return Err(Error::Rejected(format!(
            "file troppo grande ({} byte, limite {max_bytes})",
            meta.len()
        )));
    }
    Ok(std::fs::read(path)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rifiuta_traversal() {
        let root = Path::new("/tmp/vault");
        for rel in [
            "../x",
            "a/../../b",
            "/etc/passwd",
            ".git/config",
            "ok/../no",
        ] {
            assert!(safe_join(root, Path::new(rel)).is_err(), "accettato: {rel}");
        }
    }

    #[test]
    fn accetta_percorso_normale() {
        let root = Path::new("/tmp/vault");
        assert_eq!(
            safe_join(root, Path::new("notebooks/nb-1/pages/pg-1.json")).unwrap(),
            PathBuf::from("/tmp/vault/notebooks/nb-1/pages/pg-1.json")
        );
    }

    #[test]
    fn rifiuta_symlink_antenato() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("vault");
        std::fs::create_dir_all(&root).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.path(), root.join("notebooks")).unwrap();
            assert!(safe_join(&root, Path::new("notebooks/x.json")).is_err());
        }
        #[cfg(not(unix))]
        {
            let _ = root;
        }
    }

    #[test]
    fn scrittura_atomica_non_lascia_temporanei() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("page.json");
        atomic_write(&path, b"{\"a\":1}").unwrap();
        atomic_write(&path, b"{\"a\":2}").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"{\"a\":2}");
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty());
    }
}
