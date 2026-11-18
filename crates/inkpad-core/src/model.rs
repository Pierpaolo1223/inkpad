//! Modello persistito. È il contratto fra frontend, disco e Git:
//! cambiarlo significa cambiare `schema` e prevedere una migrazione.
//!
//! Scelte difensive:
//! * `deny_unknown_fields`: un'app vecchia rifiuta di riscrivere una pagina
//!   scritta da una versione nuova invece di cancellarne i campi;
//! * limiti espliciti su conteggi, coordinate e testo: un file ostile non può
//!   far allocare al renderer milioni di punti;
//! * i punti sono terzine piatte `x, y, pressione` in unità di pagina
//!   (larghezza 1000, altezza 1414): lo stesso tratto ha la stessa geometria
//!   su telefono, tablet e desktop.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::paths::validate_component;

pub const SCHEMA_VERSION: u32 = 1;
pub const PAGE_WIDTH: f32 = 1000.0;
pub const PAGE_HEIGHT: f32 = 1414.0;

pub const MAX_STROKES: usize = 5_000;
pub const MAX_POINTS_PER_STROKE: usize = 20_000;
pub const MAX_POINTS_PER_PAGE: usize = 200_000;
pub const MAX_TEXT_BYTES: usize = 256 * 1024;
pub const MAX_HIGHLIGHTS: usize = 2_000;
pub const MAX_COORD: f32 = 10_000.0;
pub const MAX_PAGES_PER_NOTEBOOK: usize = 2_000;
pub const MAX_NOTEBOOKS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Paper {
    Plain,
    Ruled,
    Squared,
    Dotted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tool {
    Pen,
    Marker,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stroke {
    pub tool: Tool,
    /// `#rrggbb`
    pub color: String,
    /// Larghezza in unità di pagina.
    pub width: f32,
    /// Terzine piatte: `x, y, pressione`, pressione in `[0, 1]`.
    pub points: Vec<f32>,
}

impl Stroke {
    pub fn validate(&self) -> Result<()> {
        if !self.width.is_finite() || !(0.25..=64.0).contains(&self.width) {
            return Err(Error::Invalid("spessore del tratto fuori scala".into()));
        }
        if !valid_color(&self.color) {
            return Err(Error::Invalid("colore del tratto non valido".into()));
        }
        if self.points.len() < 3 || self.points.len() % 3 != 0 {
            return Err(Error::Invalid("punti del tratto non validi".into()));
        }
        if self.points.len() / 3 > MAX_POINTS_PER_STROKE {
            return Err(Error::Rejected("troppi punti in un tratto".into()));
        }
        for triple in self.points.chunks_exact(3) {
            let (x, y, pressure) = (triple[0], triple[1], triple[2]);
            if !x.is_finite() || !y.is_finite() || !pressure.is_finite() {
                return Err(Error::Rejected("coordinate non finite".into()));
            }
            if x.abs() > MAX_COORD || y.abs() > MAX_COORD {
                return Err(Error::Rejected("coordinate fuori pagina".into()));
            }
            if !(0.0..=1.0).contains(&pressure) {
                return Err(Error::Rejected("pressione fuori intervallo".into()));
            }
        }
        Ok(())
    }
}

/// Evidenziazione di una parola del testo dattiloscritto.
///
/// Gli offset sono in unità UTF-16, cioè gli stessi indici che usa
/// JavaScript su `String.prototype.slice` e sul `textarea`: il frontend non
/// deve convertire nulla.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Highlight {
    pub start: usize,
    pub end: usize,
    /// `#rrggbb`
    pub color: String,
}

impl Highlight {
    pub fn validate(&self, text_len: usize) -> Result<()> {
        if self.start >= self.end || self.end > text_len {
            return Err(Error::Invalid(
                "intervallo di evidenziazione non valido".into(),
            ));
        }
        if !valid_color(&self.color) {
            return Err(Error::Invalid(
                "colore dell'evidenziazione non valido".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Page {
    pub schema: u32,
    pub id: String,
    pub paper: Paper,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    #[serde(default)]
    pub strokes: Vec<Stroke>,
    #[serde(default)]
    pub highlights: Vec<Highlight>,
    /// Millisecondi Unix dell'ultima modifica.
    #[serde(default)]
    pub updated_at: u64,
}

impl Page {
    pub fn new(id: String, paper: Paper) -> Self {
        Self {
            schema: SCHEMA_VERSION,
            id,
            paper,
            text: String::new(),
            strokes: Vec::new(),
            highlights: Vec::new(),
            updated_at: now_ms(),
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema != SCHEMA_VERSION {
            return Err(Error::Rejected(format!(
                "schema di pagina non supportato: {}",
                self.schema
            )));
        }
        validate_component(&self.id)?;
        if self.text.len() > MAX_TEXT_BYTES {
            return Err(Error::Rejected("testo della pagina troppo lungo".into()));
        }
        if self.strokes.len() > MAX_STROKES {
            return Err(Error::Rejected("troppi tratti nella pagina".into()));
        }
        let mut points = 0usize;
        for stroke in &self.strokes {
            stroke.validate()?;
            points += stroke.points.len() / 3;
            if points > MAX_POINTS_PER_PAGE {
                return Err(Error::Rejected("troppi punti nella pagina".into()));
            }
        }
        if self.highlights.len() > MAX_HIGHLIGHTS {
            return Err(Error::Rejected(
                "troppe evidenziazioni nella pagina".into(),
            ));
        }
        let text_len = self.text.encode_utf16().count();
        for highlight in &self.highlights {
            highlight.validate(text_len)?;
        }
        Ok(())
    }
}

/// Strumento attivo nel quaderno. È una preferenza, non contenuto del foglio,
/// ma vive nel quaderno per poter viaggiare con lui su altri dispositivi.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UiTool {
    Pen,
    Marker,
    Eraser,
    Text,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotebookUi {
    pub tool: UiTool,
    /// `#rrggbb`
    pub pen_color: String,
    /// `#rrggbb`
    pub marker_color: String,
}

impl NotebookUi {
    pub fn validate(&self) -> Result<()> {
        if !valid_color(&self.pen_color) {
            return Err(Error::Invalid("colore della penna non valido".into()));
        }
        if !valid_color(&self.marker_color) {
            return Err(Error::Invalid(
                "colore dell'evidenziatore non valido".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Notebook {
    pub schema: u32,
    pub id: String,
    pub title: String,
    pub pages: Vec<String>,
    #[serde(default)]
    pub updated_at: u64,
    /// Strumento e colori dell'ultimo uso. Assente nei quaderni mai toccati da
    /// una versione che li salva: in quel caso il file resta identico a prima.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui: Option<NotebookUi>,
}

impl Notebook {
    pub fn new(id: String, title: String) -> Self {
        Self {
            schema: SCHEMA_VERSION,
            id,
            title,
            pages: Vec::new(),
            updated_at: now_ms(),
            ui: None,
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema != SCHEMA_VERSION {
            return Err(Error::Rejected(format!(
                "schema di quaderno non supportato: {}",
                self.schema
            )));
        }
        validate_component(&self.id)?;
        validate_title(&self.title)?;
        if self.pages.len() > MAX_PAGES_PER_NOTEBOOK {
            return Err(Error::Rejected("troppe pagine nel quaderno".into()));
        }
        let mut seen = std::collections::HashSet::new();
        for page in &self.pages {
            validate_component(page)?;
            if !seen.insert(page.as_str()) {
                return Err(Error::Rejected("pagina duplicata nel quaderno".into()));
            }
        }
        if let Some(ui) = &self.ui {
            ui.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotebookSummary {
    pub id: String,
    pub title: String,
    pub pages: usize,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: u32,
    pub notebooks: Vec<String>,
}

impl Manifest {
    pub fn empty() -> Self {
        Self {
            schema: SCHEMA_VERSION,
            notebooks: Vec::new(),
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema != SCHEMA_VERSION {
            return Err(Error::Rejected(format!(
                "schema del vault non supportato: {}",
                self.schema
            )));
        }
        if self.notebooks.len() > MAX_NOTEBOOKS {
            return Err(Error::Rejected("troppi quaderni nel vault".into()));
        }
        let mut seen = std::collections::HashSet::new();
        for id in &self.notebooks {
            validate_component(id)?;
            if !seen.insert(id.as_str()) {
                return Err(Error::Rejected("quaderno duplicato nel vault".into()));
            }
        }
        Ok(())
    }
}

pub fn validate_title(title: &str) -> Result<()> {
    let title = title.trim();
    if title.is_empty() || title.chars().count() > 80 {
        return Err(Error::Invalid("titolo del quaderno non valido".into()));
    }
    if title.chars().any(char::is_control) {
        return Err(Error::Invalid(
            "il titolo contiene caratteri di controllo".into(),
        ));
    }
    Ok(())
}

fn valid_color(color: &str) -> bool {
    let bytes = color.as_bytes();
    bytes.len() == 7 && bytes[0] == b'#' && bytes[1..].iter().all(|b| b.is_ascii_hexdigit())
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// JSON stabile e leggibile: la stessa pagina produce gli stessi byte, così i
/// diff Git restano piccoli (una pressione in più cambia una riga).
pub fn to_json_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stroke(points: Vec<f32>) -> Stroke {
        Stroke {
            tool: Tool::Pen,
            color: "#1f2933".into(),
            width: 3.0,
            points,
        }
    }

    #[test]
    fn roundtrip_esatto_dei_float() {
        let mut page = Page::new("pg-abc".into(), Paper::Ruled);
        page.strokes.push(stroke(vec![
            10.123456, 20.654321, 0.512345, 11.1, 21.2, 0.9,
        ]));
        let first = to_json_bytes(&page).unwrap();
        let decoded: Page = serde_json::from_slice(&first).unwrap();
        let second = to_json_bytes(&decoded).unwrap();
        assert_eq!(first, second);
        assert_eq!(decoded.strokes[0].points[0], 10.123456f32);
    }

    #[test]
    fn rifiuta_campi_sconosciuti() {
        let json = r#"{"schema":1,"id":"pg-a","paper":"plain","evil":true}"#;
        assert!(serde_json::from_str::<Page>(json).is_err());
    }

    #[test]
    fn valida_limiti_della_pagina() {
        let mut page = Page::new("pg-a".into(), Paper::Plain);
        page.strokes.push(stroke(vec![1.0, 2.0, 1.5]));
        assert!(page.validate().is_err(), "pressione > 1 deve fallire");

        page.strokes[0] = stroke(vec![f32::INFINITY, 2.0, 0.5]);
        assert!(page.validate().is_err());

        page.strokes[0] = stroke(vec![1.0, 2.0]);
        assert!(page.validate().is_err(), "terzina incompleta");

        page.strokes[0].color = "red".into();
        assert!(page.validate().is_err());

        page.strokes[0] = stroke(vec![1.0, 2.0, 0.5]);
        page.text = "x".repeat(MAX_TEXT_BYTES + 1);
        assert!(page.validate().is_err());
    }

    fn highlight(start: usize, end: usize) -> Highlight {
        Highlight {
            start,
            end,
            color: "#b91c1c".into(),
        }
    }

    #[test]
    fn evidenziazioni_roundtrip_e_limiti() {
        let mut page = Page::new("pg-a".into(), Paper::Ruled);
        page.text = "Ciao sulle righe".into();
        page.highlights.push(highlight(0, 4));

        let bytes = to_json_bytes(&page).unwrap();
        let decoded: Page = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(to_json_bytes(&decoded).unwrap(), bytes);
        assert!(decoded.validate().is_ok());

        let mut bad = decoded.clone();
        bad.highlights[0] = highlight(5, 5);
        assert!(bad.validate().is_err(), "intervallo vuoto");

        let mut bad = decoded.clone();
        bad.highlights[0] = highlight(0, 400);
        assert!(bad.validate().is_err(), "fine oltre il testo");

        let mut bad = decoded.clone();
        bad.highlights[0].color = "yellow".into();
        assert!(bad.validate().is_err(), "colore non esadecimale");

        let mut bad = decoded.clone();
        bad.highlights = (0..MAX_HIGHLIGHTS + 1).map(|_| highlight(0, 4)).collect();
        assert!(bad.validate().is_err(), "troppe evidenziazioni");
    }

    #[test]
    fn offset_delle_evidenziazioni_in_utf16() {
        let mut page = Page::new("pg-b".into(), Paper::Ruled);
        page.text = "👍 ciao".into();
        // Le emoji occupano due unità UTF-16: "ciao" va da 3 a 7, mentre in
        // caratteri Unicode il testo è lungo 6. Gli offset sono quelli di JS.
        page.highlights.push(highlight(3, 7));
        assert!(page.validate().is_ok());

        page.highlights[0] = highlight(3, 8);
        assert!(page.validate().is_err(), "oltre la lunghezza UTF-16");
    }

    #[test]
    fn pagina_senza_evidenziazioni_resta_compatibile() {
        let json = r#"{"schema":1,"id":"pg-a","paper":"plain","strokes":[]}"#;
        let page: Page = serde_json::from_str(json).unwrap();
        assert!(page.highlights.is_empty());
        assert!(page.validate().is_ok());
    }

    #[test]
    fn rifiuta_id_pericolosi() {
        let mut page = Page::new("pg-a".into(), Paper::Plain);
        page.id = "../../etc/passwd".into();
        assert!(page.validate().is_err());
    }

    #[test]
    fn quaderno_rifiuta_pagine_duplicate() {
        let mut nb = Notebook::new("nb-a".into(), "Appunti".into());
        nb.pages = vec!["pg-a".into(), "pg-a".into()];
        assert!(nb.validate().is_err());
    }

    #[test]
    fn preferenze_del_quaderno_roundtrip_e_limiti() {
        // Un quaderno che non le ha resta identico al formato precedente:
        // nessun campo nuovo nei file già committati.
        let mut nb = Notebook::new("nb-a".into(), "Appunti".into());
        assert!(!String::from_utf8_lossy(&to_json_bytes(&nb).unwrap()).contains("\"ui\""));

        nb.ui = Some(NotebookUi {
            tool: UiTool::Marker,
            pen_color: "#1f2933".into(),
            marker_color: "#f2c200".into(),
        });
        let bytes = to_json_bytes(&nb).unwrap();
        let decoded: Notebook = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(decoded.ui, nb.ui);
        assert!(decoded.validate().is_ok());

        let mut bad = nb.clone();
        bad.ui.as_mut().unwrap().marker_color = "giallo".into();
        assert!(bad.validate().is_err(), "colore non esadecimale");

        let json = r##"{"schema":1,"id":"nb-a","title":"A","pages":[],"ui":{"tool":"pen","pen_color":"#000000","marker_color":"#ffffff","evil":1}}"##;
        assert!(serde_json::from_str::<Notebook>(json).is_err(), "campo sconosciuto");

        let json = r##"{"schema":1,"id":"nb-a","title":"A","pages":[],"ui":{"tool":"pennarello","pen_color":"#000000","marker_color":"#ffffff"}}"##;
        assert!(serde_json::from_str::<Notebook>(json).is_err(), "strumento sconosciuto");
    }
}
