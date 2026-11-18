# Inkpad — quaderno a inchiostro, locale prima di tutto

Appunti scritti a penna su pagine di quaderno. Funziona senza rete, salva sul
dispositivo e, quando lo chiedi, sincronizza la cartella con un repository Git
via HTTPS. Stesso codice Rust per desktop, iOS e Android (Tauri v2).

* **Leggerissima**: bundle web ~23 KB di JS e ~9 KB di CSS (misurato, vedi
  [Verifiche](#verifiche)); nessun framework UI, nessun database, nessun
  servizio esterno.
* **Penna**: pressione, eventi pointer coalescenti, rifiuto del palmo, gomma a
  tratto intero, annulla/ripeti.
* **Evidenziatore**: un tocco su una parola del testo dattiloscritto la
  evidenzia (colori della tavolozza, giallo evidenziatore incluso); secondo
  tocco con lo stesso colore la toglie.
* **Si apre sul testo**: al primo accesso lo strumento attivo è Testo, per far
  vedere subito che si può scrivere sulle righe; poi ogni quaderno ricorda
  l'ultimo strumento e i due colori, salvati nel suo `notebook.json`: seguono
  il quaderno e i suoi commit, quindi li ritrovi anche su un altro dispositivo.
* **Quaderno**: quaderni → pagine ordinate, carta  rigata/quadrettata/puntinata/bianca, note dattiloscritte opzionali scritte
  sulle stesse righe dell'inchiostro; un quaderno si elimina dalla sidebar
  insieme alle sue pagine.
* **Git, esplicito**: commit, pull (fast-forward o merge), push. Solo HTTPS,
  token tenuto in memoria, nessuna sincronizzazione automatica in background.

## Struttura

```
inkpad/
├── crates/inkpad-core/   modello, vault su disco, Git (senza Tauri, testabile)
├── src-tauri/            guscio Tauri v2: comandi IPC + stato di sessione
├── src/                  frontend TypeScript (canvas, quaderni, pannello Git)
├── index.html
└── ARCHITECTURE.md / SECURITY.md
```

## Avvio (desktop)

Prerequisiti: Rust stabile, Node 20+, e su Linux le librerie Tauri v2
(`libwebkit2gtk-4.1-dev`, `libsoup-3.0-dev`, `libgtk-3-dev`, `librsvg2-dev`,
`libayatana-appindicator3-dev`, `libxdo-dev`, `cmake`, `perl` — `cmake` serve a
libgit2 compilata dal sorgente).

```bash
npm install
npm run tauri dev      # sviluppo
npm run tauri build    # pacchetto desktop
```

I dati stanno nella cartella dati dell'app (`app_data_dir()/vault`), mostrata in
fondo alla barra laterale.

## iOS e Android

Il codice applicativo è già portabile (nessuna API desktop-only, canvas
pointer events, coordinate di pagina indipendenti dallo schermo). Restano i
passi di piattaforma, che richiedono i toolchain ufficiali:

```bash
# Android: Android Studio con SDK/NDK, ANDROID_HOME, NDK_HOME, JAVA_HOME
rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android
npm run tauri android init
npm run tauri android dev      # emulatore/dispositivo
npm run tauri android build

# iOS: macOS con Xcode e CocoaPods
rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios
npm run tauri ios init
npm run tauri ios dev
npm run tauri ios build
```

Su mobile il crate `git2` va compilato con OpenSSL inclusa, perché iOS e
Android non hanno un OpenSSL di sistema:

```bash
npm run tauri android build -- --features vendored-openssl
npm run tauri ios build -- --features vendored-openssl
```

Servono `cmake` e `perl` nel toolchain di cross-compilazione. Le stesse
librerie compilano senza modifiche al codice Rust.

## Formato dei dati

```
vault/                          # radice del repository Git
├── inkpad.json                 # manifesto: schema + elenco quaderni
└── notebooks/<nb-id>/
    ├── notebook.json           # titolo, ordine delle pagine,
    │                           # ultimo strumento e colori (campo `ui`)
    └── pages/<pg-id>.json      # tratti + testo + evidenziazioni
```

Una pagina è JSON leggibile e deterministico (una modifica di un tratto cambia
una riga), così i diff Git restano piccoli e un merge non distrugge il file.
I punti sono terzine piatte `x, y, pressione` in unità di pagina 1000 × 1414:
lo stesso tratto ha la stessa geometria su telefono, tablet e desktop.

Lo strumento e i colori dell'ultimo uso stanno nel campo `ui` di
`notebook.json`: viaggiano con il quaderno, ma non toccano `updated_at`, così
scegliere un colore non lo fa salire in cima alla lista. Un quaderno che non li
ha resta identico al formato precedente (il campo è assente, non `null`).

## Sincronizzazione

1. Il remote deve essere **HTTPS**, senza credenziali nell'URL.
2. Il token (GitHub: fine-grained token con permesso *Contents*) si inserisce
   nel pannello Git e resta in memoria per la sessione.
3. **Commit** mette nel repository solo `notebooks/` e `inkpad.json`.
4. **Pull** rifiuta di partire se ci sono modifiche non committate; fa
   fast-forward oppure merge. Se ci sono conflitti non scrive nulla e li elenca.
5. **Push** invia il branch corrente.

Il repository è trattato come input non fidato: symlink, submodule, percorsi
con traversal, blob enormi e configurazioni che eseguono comandi vengono
rifiutati. Dettagli in [SECURITY.md](SECURITY.md).

## Verifiche

Eseguiti in questo repository:

```bash
cargo test -p inkpad-core          # 34 test: modello, vault, Git
cargo clippy -p inkpad-core -p inkpad --all-targets
npm run build                      # tsc --noEmit + vite build
npm run test:unit                  # 8 test: parole, evidenziazioni, offset
npm run test:ui                    # 36 controlli in Chrome headless con IPC
                                   # simulato: autosave, cambio pagina,
                                   # strumenti, evidenziatore, preferenze
```

Misure reali del bundle di produzione:

| file | dimensione | gzip |
|---|---|---|
| `dist/assets/index-*.js` | 23.52 kB | 8.34 kB |
| `dist/assets/index-*.css` | 9.01 kB | 2.69 kB |
| `dist/index.html` | 8.04 kB | 2.36 kB |

`cargo clippy -p inkpad` e `npm run tauri dev` girano su questa macchina (le
dipendenze di sistema `libsoup-3.0` e `webkit2gtk-4.1` sono installate).
Non eseguiti qui, e quindi **non verificati**: build iOS/Android, prove su
dispositivo con penna, misure di latenza penna→pixel. Vedi
[ARCHITECTURE.md](ARCHITECTURE.md#qa) per la matrice di test manuale.

## Limiti noti (v1)

* Nessun conflitto assistito: un conflitto di merge viene rifiutato, non
  risolto nell'app.
* Nessuna cronologia versioni nell'interfaccia: si usa Git.
* Nessun riconoscimento di scrittura, OCR o ricerca full-text (per scelta:
  niente modelli, niente peso).
* Una pagina alla volta; nessun layout a doppia pagina o infinito.
* Il campo `ui` di `notebook.json` è protetto da `deny_unknown_fields`: una
  build **precedente** rifiuta di aprire un quaderno che lo contiene (stessa
  scelta già fatta per `highlights`). Si risolve aggiornando l'app.
* Nessun rinomina quaderno / riordino pagine nell'interfaccia (i file sono
  modificabili a mano e il prossimo commit li porta con sé).

## Licenza

MIT — vedi [LICENSE](LICENSE).
