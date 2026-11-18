//! Errori del core. Il tipo è l'unica superficie che l'app Tauri converte in
//! errore IPC: il messaggio non contiene mai credenziali né URL con userinfo.

use std::fmt;

/// Errore del dominio Inkpad.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("input non valido: {0}")]
    Invalid(String),
    #[error("payload rifiutato: {0}")]
    Rejected(String),
    #[error("non trovato: {0}")]
    NotFound(String),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON non valido: {0}")]
    Json(#[from] serde_json::Error),
    #[error("git: {0}")]
    Git(#[from] git2::Error),
    #[error("il vault non è ancora inizializzato")]
    NotInitialized,
    #[error("repository Git non inizializzato")]
    NoRepo,
    #[error("conflitto di merge nei file: {0}")]
    Conflict(String),
    #[error("autenticazione richiesta: serve un token HTTPS in memoria")]
    AuthRequired,
    #[error("operazione già in corso")]
    Busy,
    #[error("sincronizzazione scaduta (timeout di rete)")]
    Timeout,
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// Codice stabile per il client: il frontend decide il messaggio, non la
    /// stringa dell'errore.
    pub fn code(&self) -> &'static str {
        match self {
            Error::Invalid(_) => "invalid_input",
            Error::Rejected(_) => "rejected",
            Error::NotFound(_) => "not_found",
            Error::Io(_) => "io",
            Error::Json(_) => "invalid_json",
            Error::Git(_) => "git",
            Error::NotInitialized => "not_initialized",
            Error::NoRepo => "no_repo",
            Error::Conflict(_) => "conflict",
            Error::AuthRequired => "auth_required",
            Error::Busy => "busy",
            Error::Timeout => "timeout",
        }
    }
}

/// Identità Git usata per i commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub name: String,
    pub email: String,
}

impl Identity {
    pub fn new(name: &str, email: &str) -> Result<Self> {
        let name = name.trim();
        let email = email.trim();
        if name.is_empty()
            || name.len() > 64
            || name.chars().any(|c| c.is_control() || c == '<' || c == '>')
        {
            return Err(Error::Invalid("nome autore non valido".into()));
        }
        if email.len() > 254
            || !email.contains('@')
            || email.starts_with('@')
            || email.ends_with('@')
            || email
                .chars()
                .any(|c| c.is_whitespace() || c.is_control() || c == '<' || c == '>')
        {
            return Err(Error::Invalid("email autore non valida".into()));
        }
        Ok(Self {
            name: name.to_string(),
            email: email.to_string(),
        })
    }
}

/// URL remoto accettato: solo HTTPS, senza userinfo (un token nell'URL
/// finirebbe in `.git/config` e nei log).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteUrl(String);

impl RemoteUrl {
    pub fn parse(raw: &str) -> Result<Self> {
        let raw = raw.trim();
        if raw.is_empty()
            || raw.len() > 2048
            || raw.chars().any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(Error::Invalid("URL del remote non valido".into()));
        }
        let parsed =
            url::Url::parse(raw).map_err(|_| Error::Invalid("URL del remote non valido".into()))?;
        if parsed.scheme() != "https" {
            return Err(Error::Invalid("sono ammessi solo remote HTTPS".into()));
        }
        if parsed.host_str().map(str::is_empty).unwrap_or(true) {
            return Err(Error::Invalid("l'URL deve avere un host".into()));
        }
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err(Error::Rejected(
                "l'URL non deve contenere credenziali: il token resta in memoria".into(),
            ));
        }
        if parsed.fragment().is_some() || parsed.query().is_some() {
            return Err(Error::Invalid(
                "l'URL non deve avere query o frammento".into(),
            ));
        }
        Ok(Self(parsed.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RemoteUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accetta_https_semplice() {
        let u = RemoteUrl::parse("https://github.com/tizio/quaderni.git").unwrap();
        assert_eq!(u.as_str(), "https://github.com/tizio/quaderni.git");
    }

    #[test]
    fn rifiuta_schemi_non_https() {
        for raw in [
            "http://github.com/a/b.git",
            "ssh://git@github.com/a/b.git",
            "git://github.com/a/b.git",
            "file:///etc/passwd",
            "/tmp/repo",
            "//host/path",
        ] {
            assert!(RemoteUrl::parse(raw).is_err(), "accettato: {raw}");
        }
    }

    #[test]
    fn rifiuta_credenziali_nell_url() {
        assert!(matches!(
            RemoteUrl::parse("https://user:token@github.com/a/b.git"),
            Err(Error::Rejected(_))
        ));
        assert!(RemoteUrl::parse("https://token@github.com/a/b.git").is_err());
    }

    #[test]
    fn identita_rifiuta_a_capo_e_parentesi() {
        assert!(Identity::new("a\nb", "a@b.it").is_err());
        assert!(Identity::new("Mario <x>", "a@b.it").is_err());
        assert!(Identity::new("Mario", "non-una-email").is_err());
    }
}
