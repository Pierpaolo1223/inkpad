# Architettura di Inkpad

> Le affermazioni su cosa è stato misurato sono datate e riproducibili; le
> affermazioni di progetto sono decisioni, non fatti. Dove una decisione
> discende da una fonte, la fonte è citata.

Data: 2026-11-17.

## 1. Vincoli di partenza

| Vincolo | Conseguenza |
|---|---|
| "leggerissima" | nessun framework UI, nessun database, nessun modulo nativo oltre libgit2; canvas 2D e moduli TypeScript |
| penna | eventi `pointer` con pressione e coalescing; rifiuto del palmo; nessun ritardo introdotto da animazioni |
| sincronizzabile con Git | il vault *è* un repository; una pagina = un file; commit/pull/push espliciti |
| pagine "a mo' di quaderno" | quaderni e pagine ordinate, carta a righe/quadretti/punti, coordinate di pagina fisse |
| Rust + Tauri v2 | un core Rust senza dipendenze Tauri, riusato su desktop, iOS e Android |
| iOS e Android | nessuna API desktop-only, vault predefinito in `app_data_dir`, OpenSSL inclusa nel binario mobile |

## 2. Ripartizione del codice

```
frontend (TS)                  guscio Tauri                core Rust
canvas + UI  ──invoke──────▶  comandi IPC        ──────▶  Page / Vault / Git
                              stato di sessione            fs atomico, limiti
                                                           libgit2 (HTTPS)
```

* `crates/inkpad-core` non conosce Tauri: si compila e si testa con
  `cargo test`, anche in CI senza WebView.
* `src-tauri` non contiene logica di dominio: valida l'input di confine,
  traduce errori e serializza. Nessun plugin filesystem/shell/http.
* il frontend non tocca mai il disco: ogni operazione passa da un comando
  tipizzato.

**Perché**: la parte che può corrompere dati è la più piccola possibile e ha
test senza interfaccia grafica; il guscio mobile resta sostituibile.

### Apertura del vault

La sessione parte da `app_data_dir()/vault`. Il comando `vault_pick` apre il
selettore nativo (desktop) e `vault_use` cambia la radice solo dopo aver
validato il percorso: deve essere una directory assoluta, reale e non symlink;
deve contenere un `inkpad.json` valido, oppure essere vuota e richiedere una
conferma esplicita. La scelta viene scritta in `settings.json` nella
configurazione dell'app e riaperta al riavvio. Al cambio di vault il token Git
in memoria viene azzerato, perché apparteneva a un altro repository.

Su Android e iOS il plugin dialog di Tauri non offre un selettore di cartelle:
il comando restituisce `unsupported` e resta il vault in `app_data_dir`. Un
supporto SAF richiede un plugin nativo dedicato.

## 3. Modello dati

```
vault/
├── inkpad.json                 { schema, notebooks: [id] }
└── notebooks/<nb-id>/
    ├── notebook.json           { schema, id, title, pages: [id], updated_at,
                                  ui? }
    └── pages/<pg-id>.json      { schema, id, paper, text, strokes, highlights,
                                 updated_at }
```

* `Stroke.points` è un array piatto `x, y, pressione` (terzine) in unità di
  pagina `1000 × 1414`: stesso tratto su ogni schermo, nessuna dipendenza dal
  device pixel ratio.
* `serde` con `deny_unknown_fields`: un'app vecchia **rifiuta** di riscrivere
  una pagina di una versione nuova invece di cancellarne i campi.
* `Highlight` è un intervallo `[start, end)` in unità **UTF-16**, gli stessi
  indici delle stringhe JavaScript; il frontend non converte nulla e il core
  valida solo i limiti. Le evidenziazioni che toccano testo modificato vengono
  scartate invece di restare appese a parole sbagliate.
* `Notebook.ui` (`tool`, `pen_color`, `marker_color`) è l'unica parte del
  quaderno che non è contenuto: serve a ritrovare lo strumento su ogni
  dispositivo. È `Option` + `skip_serializing_if`, quindi i quaderni che non
  l'hanno producono gli stessi byte di prima, e salvarla non tocca
  `updated_at` (niente riordino della lista, nessun diff se il valore è
  identico). Scriverla da subito nel file la fa entrare nel prossimo commit,
  insieme alle pagine.
* Limiti espliciti: 5 000 tratti/pagina, 200 000 punti/pagina, 2 000
  evidenziazioni/pagina, 256 KB di testo, coordinate finite entro 10 000,
  pressione in `[0, 1]`.
* Scrittura atomica: file temporaneo nella stessa cartella, `fsync`, `rename`,
  `fsync` della cartella. Un crash non lascia JSON a metà.

## 4. Sincronizzazione

`pull`:

1. rifiuta se il working tree ha modifiche non committate (l'inchiostro non
   ancora salvato non deve essere sovrascritto);
2. verifica la configurazione locale del repository (vedi §7);
3. `fetch` con timeout di 120 s e refspec esplicito;
4. se non c'è ancora un commit locale: adotta il branch remoto (primo
   allineamento di un dispositivo nuovo);
5. altrimenti `merge_analysis`: fast-forward → checkout; merge normale →
   `merge_commits` **in memoria**; conflitti → errore con l'elenco dei file e
   nessuna scrittura; nessun conflitto → commit di merge con due padri;
6. il checkout non usa `checkout_head`: materializza l'albero con le regole di
   §7 e aggiorna ref, HEAD e index.

`push`: refspec `refs/heads/<b>:refs/heads/<b>`, callback di credenziali,
rifiuto del server riportato come errore. Nessun force.

`commit`: stage limitato a `notebooks/` e `inkpad.json`; un albero identico al
padre produce "nessuna modifica" invece di un commit vuoto.

## 5. Reparto Rust

* Edizione 2021, nessun `unsafe`, nessuna `unwrap` su input utente nel core.
* Errori tipizzati (`thiserror`) con un `code` stabile per il client; i
  messaggi non includono mai il token.
* `git2` con libgit2 compilata dal sorgente (`vendored-libgit2`): nessuna
  dipendenza dal libgit2 di sistema, che su iOS/Android non esiste.
* Test unitari dentro i moduli, con repository temporanei reali.

## 6. Reparto Design

* Metafora del quaderno: sidebar dei quaderni, una pagina centrale, barra
  pagina in basso, strumenti in alto. Il foglio ha proporzioni A4 e non si
  deforma.
* All'apertura lo strumento è Testo (scoperta del foglio), poi ogni quaderno
  ricorda l'ultimo strumento e i due colori, presi da `notebook.json`; dopo un
  pull si riprende quello arrivato dagli altri dispositivi. Le preferenze
  scritte dalle versioni precedenti in `localStorage` vengono lette una volta
  e migrate nel quaderno.
* Il testo selezionato con il mouse resta su un velo chiaro (`::selection`):
  la selezione del tema di sistema scurisce le parole e le rende illeggibili
  sul foglio.
* L'interfaccia compare solo quando il CSS è applicato (`app-ready` aggiunto
  da `main.ts`, `#app` nascosto nel `index.html`): in sviluppo Vite inietta il
  CSS dal JavaScript e senza questo si vedeva l'HTML non formattato.
* Scegliere uno strumento avviene sul `pointerdown` e la scelta fa
  `preventDefault`: altrimenti il `mousedown` porta il focus sul pulsante, il
  `blur` del textarea esce dal testo e Testo non resterebbe mai attivo.
* Bersagli ≥ 44 px su touch, `safe-area-inset-*` per notch e home indicator,
  focus visibile, `prefers-reduced-motion` rispettato.
* La penna scrive, il dito scrive finché non compare la penna; un tocco entro
  1,2 s dall'ultimo evento penna o con contatto > 28 px è trattato come palmo.
* Nessun elemento dell'interfaccia si muove durante il tratto: il canvas è
  `touch-action: none`, il contenitore ha `overscroll-behavior: contain`.
* Testo dattiloscritto opzionale scritto sul foglio: un livello trasparente
  sopra il canvas, con interlinea uguale al passo della carta (`--rule`, 3,12 %
  per righe, 2,5 % per quadretti e punti) e linee di base sulle righe. Attivo
  con lo strumento **Testo**, non intercetta i puntatori quando si scrive a
  penna; salvato nello stesso file della pagina.
* L'evidenziatore colora parole, non tratti: un livello invisibile con la
  stessa metrica del textarea (`.page-measure`) fornisce le posizioni delle
  parole, sia per il tocco sia per i rettangoli colorati (`.page-highlight`).
  Due testi con la stessa metrica sono l'unico modo per non duplicare il
  layout del testo a mano.

## 7. Reparto Security e Red team

Modello di minaccia e tabella dei controlli: [SECURITY.md](SECURITY.md).
Sintesi delle scelte non ovvie:

* Il repository è **input non fidato**. Il checkout è scritto a mano:
  solo blob regolari, nomi validati componente per componente, niente `.git`,
  niente symlink (mode `120000`), niente submodule (`160000`), limiti per blob
  (16 MiB), per checkout (128 MiB) e per numero di file (20 000).
* La configurazione locale del repository non può contenere
  `credential.*`, `include.*`, `filter.*`, `core.sshCommand`,
  `core.hookspath`, `diff.*.command`: sono chiavi che eseguono programmi o
  leggono file arbitrari.
* Il token non è mai nell'URL (rifiutato con `userinfo`), mai su disco, mai
  restituito al frontend; vive in `Zeroizing<String>` dietro un `Mutex`.
* Un solo pull/push alla volta (`try_lock`): una sincronizzazione in corso non
  si sovrappone a un save.
* Un vault scelto dall'utente è input non fidato: la radice non può essere un
  symlink, il manifesto viene validato e la creazione è consentita solo in una
  cartella vuota. La configurazione Git di un vault importato resta fuori da
  questa validazione: non sincronizzare un vault di provenienza sconosciuta.
* CSP senza `unsafe-eval`, nessuna risorsa remota, `withGlobalTauri: false`,
  capability ridotta a `core:default`.

## 8. Reparto QA

Test automatici (`cargo test -p inkpad-core`, 36 test):

| Area | Cosa dimostra |
|---|---|
| modello | roundtrip JSON bit-a-bit, campi sconosciuti rifiutati, limiti di pressione/coordinate/testo, id con traversal rifiutati |
| vault | ciclo quaderno→pagina→save→delete, eliminazione del quaderno e delle sue pagine, quaderno orfano rimosso dal manifesto, pagina di un altro quaderno rifiutata, cartella `pages` symlink rifiutata, manifesto corrotto non riscritto, apertura solo di vault esistenti e creazione solo in cartella vuota |
| path | `..`, radice assoluta, `.git`, symlink antenato rifiutati; scrittura atomica senza file temporanei residui |
| Git | init/commit, nessuna modifica → errore, primo pull, fast-forward con rimozione dei file obsoleti, push su bare locale, pull bloccato da modifiche non committate, pull bloccato da `credential.helper`, symlink nell'albero rifiutato, traversal rifiutato, merge pulito a due padri, conflitto senza scritture |
| evidenziazioni | roundtrip, intervalli vuoti/fuori dal testo, colore non esadecimale, limite, offset UTF-16, pagine senza il campo |

Frontend: `npm run test:unit` (8 test sulle funzioni pure) e `npm run test:ui`
(39 controlli), che pilota la vera interfaccia in Chrome headless con l'IPC
Tauri sostituito da un vault in memoria: autosalvataggio, cambio pagina,
aggiunta/eliminazione di pagine e quaderni, evidenziatore, annulla/ripeti,
apertura di un altro vault e conferma di creazione.
| URL | solo HTTPS, credenziali nell'URL rifiutate, schemi/local path rifiutati |

Non coperto dai test automatici, da provare su dispositivo (matrice manuale):

1. penna attiva su iPad/Apple Pencil: pressione, latenza percepita, palmo
   appoggiato, scrittura veloce;
2. S Pen / penna USI su Android: `getCoalescedEvents` disponibile, pressione;
3. ruotare il dispositivo durante un tratto (resize del canvas);
4. pull da un secondo dispositivo con pagine modificate su entrambi i lati:
   conflitto elencato, nessun file toccato;
5. pull dopo un commit locale e un commit remoto su pagine diverse: merge
   automatico e pagina remota visibile;
6. token errato/scaduto: errore mostrato, nessun crash, nessuna credenziale nei
   log;
7. ibernazione/chiusura con modifiche pendenti: il flush su `visibilitychange`
   salva.

## 9. Consultazione del daemon (SKILLS.md)

Sessione `inkpad-app`, daemon locale del repository (`./askd.sh`). Le risposte
sono state trattate come ipotesi da confrontare con le fonti primarie.

| Domanda | Route | Esito | Classificazione |
|---|---|---|---|
| Layout/IPC Tauri v2 per desktop+iOS+Android, capability e CSP | `rust + security` | `grounded: false`, 0 citazioni: il corpus non copre la meccanica Tauri | **confine intenzionale** (dettaglio di prodotto): fonti primarie Tauri v2 |
| Classi di attacco su clone/checkout di un repository non fidato e controlli ASVS/NIST | `git + security` (5 citazioni) | il corpus documenta il modello a oggetti: mode `120000` per symlink, `160000` per submodule, hook locali e saltabili; **manca** la mappatura mitigazione→controllo | **lacuna reale parziale**: principi dal corpus, controlli da CWE/OWASP |
| Requisiti di input stylus/palmo/coalescing e livelli di test per un motore di tratti | `git + security` (0 citazioni) | route sbagliata e nessuna mensola per input penna o livelli di test | **miss di routing + confine**: principi da MDN Pointer Events e linee guida piattaforma |

Decisioni classificate:

* **Adottata** dal corpus: il repository è un albero di oggetti con mode
  espliciti; symlink e submodule sono tipi distinti e vanno trattati come tali;
  gli hook non sono garanzia di sicurezza (sono locali e saltabili), quindi la
  difesa non può appoggiarsi a loro.
* **Adattata**: il principio "non fidarsi del contenuto dell'albero" è
  implementato come checkout manuale con allowlist di mode e limiti, non come
  `checkout_head` con opzioni.
* **Rifiutata**: l'idea di usare un credential helper per l'autenticazione —
  esegue comandi arbitrari letti dalla configurazione del repository, quindi è
  esclusa e sostituita dal token in memoria.

Fonti primarie usate per colmare le lacune:

* Tauri v2 — *Capabilities* e *Content Security Policy*:
  <https://v2.tauri.app/security/capabilities/>, <https://v2.tauri.app/security/csp/>
* MDN — *Pointer events* e `getCoalescedEvents`:
  <https://developer.mozilla.org/en-US/docs/Web/API/Pointer_events>,
  <https://developer.mozilla.org/en-US/docs/Web/API/PointerEvent/getCoalescedEvents>
* libgit2 — checkout e merge: <https://libgit2.org/docs/>
* CWE-22 (path traversal), CWE-59 (symlink following), CWE-400 (resource
  exhaustion): <https://cwe.mitre.org/>
* OWASP MASVS/MASTG (storage e platform interaction su mobile):
  <https://mas.owasp.org/>
* Git — `git-config` e credential helper:
  <https://git-scm.com/docs/git-config>

## 10. Prestazioni e limiti onesti

* Il ridisegno è incrementale durante il tratto (solo i segmenti nuovi);
  undo, gomma e resize ridisegnano l'intera pagina. Con migliaia di tratti
  questo è il primo punto da ottimizzare (tile cache o doppio canvas).
* Il core legge una pagina intera in memoria (cap 16 MiB): adatto a un
  quaderno, non a un PDF annotato.
* Il checkout materializza l'albero dopo il fetch: un repository molto grande
  viene rifiutato dai limiti, non gestito in streaming.
* Non esistono misure di latenza penna→pixel: non sono state eseguite, quindi
  non vengono dichiarate.
