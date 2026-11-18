# Sicurezza di Inkpad

Data: 2026-11-17. Modello di minaccia, controlli implementati e rischi residui.
Le classi di difetto sono riferite a CWE; i requisiti mobile a OWASP MASVS.

## 1. Beni da proteggere

| Bene | Perché conta |
|---|---|
| contenuto delle pagine | appunti personali, potenzialmente sensibili |
| token di accesso Git | consente di leggere/scrivere un repository dell'utente |
| integrità del vault | un file corrotto è inchiostro perso |
| integrità del dispositivo | il codice non deve eseguire comandi da dati remoti |

Non ci sono account Inkpad, telemetria, analytics o server propri: l'unica
rete è quella aperta da un comando Git esplicito.

## 2. Confini di fiducia

1. **WebView ↔ guscio Tauri**: il frontend è codice nostro, ma è la superficie
   che un bug XSS userebbe. La CSP e l'assenza di risorse remote lo limitano;
   i comandi IPC sono l'unica API.
2. **Repository Git ↔ filesystem**: l'albero remoto è dato non fidato (un altro
   dispositivo, un collaboratore, un repository sbagliato).
3. **Rete ↔ processo**: risposta HTTPS e pack Git sono input non fidato.
4. **Sistema operativo ↔ app**: sandbox della piattaforma; l'app non chiede
   permessi di archiviazione esterna.

## 3. Controlli

| Minaccia | Controllo | Dove |
|---|---|---|
| Path traversal da nomi di entry Git (CWE-22) | ogni componente validato: niente `..`, `/`, `\`, NUL, control char, `.git`; massimo 32 livelli | `paths::validate_component`, `git::walk_tree` |
| Symlink che esce dal vault (CWE-59) | mode `120000` rifiutato; symlink già presenti su disco rifiutati su lettura, scrittura e join | `paths::safe_join`, `git::walk_tree` |
| Submodule che esegue codice al checkout | mode `160000` rifiutato | `git::walk_tree` |
| Hook o credential helper pilotati dal repository | libgit2 non esegue hook; `assert_safe_config` rifiuta `credential.*`, `include.*`, `filter.*`, `core.sshCommand`, `core.hookspath`, `diff.*.command` | `git::assert_safe_config` |
| Credenziali nell'URL del remote | schema solo `https`, `userinfo` rifiutato | `RemoteUrl::parse` |
| Token su disco o nei log | token in `Zeroizing<String>` in memoria; mai serializzato verso il frontend; nessun logging del valore | `src-tauri/src/lib.rs` |
| Uso del token su host sbagliato | il token viene passato solo alla callback di libgit2 per il remote configurato; nessun redirect automatico di credenziali | `git::fetch`, `git::push` |
| Repository enorme / decompression bomb / esaurimento risorse (CWE-400) | limiti: 16 MiB per blob, 128 MiB per checkout, 20 000 file, profondità 32, timeout fetch 120 s, pagina max 16 MiB, tratti e punti limitati | `git.rs`, `vault.rs`, `model.rs` |
| Pull che sovrascrive inchiostro non salvato | pull rifiutato con working tree sporco; il client fa `flush` prima di ogni operazione Git | `git::assert_clean_worktree`, `main.ts` |
| Conflitti che corrompono il JSON | merge calcolato in memoria con `Checkout::NONE`; in caso di conflitto nessuna scrittura e nessun marker nel file | `git::merge` |
| Scritture parziali su crash | file temporaneo + `fsync` + rename + `fsync` della cartella | `paths::atomic_write` |
| File di pagina manomesso (JSON enorme, NaN, campi extra) | limiti di dimensione e di contenuto, `deny_unknown_fields`, `schema` obbligatorio | `model::Page::validate` |
| Salvataggio di una pagina in un quaderno che non la contiene | verifica di appartenenza prima di scrivere; nessun percorso costruito da input libero | `Vault::save_page` |
| XSS / injection nella WebView | CSP `default-src 'self'`, nessun `unsafe-eval`, niente risorse remote, `withGlobalTauri: false`; il testo dell'utente è inserito con `textContent`, mai `innerHTML` | `tauri.conf.json`, `main.ts` |
| Comandi IPC non necessari | unica capability `core:default`; nessun plugin fs/shell/http/updater; comandi con allowlist di argomenti tipizzati | `capabilities/default.json`, `lib.rs` |
| Due operazioni Git concorrenti | `try_lock` sul mutex di sync, errore `busy` | `lib.rs::begin_sync` |
| URL remoti non HTTPS o locali | schema, host, assenza di query/fragment e di credenziali | `RemoteUrl::parse` |

Riferimenti: CWE-22, CWE-59, CWE-400, CWE-79; OWASP MASVS **STORAGE** e
**PLATFORM**; OWASP ASVS (validazione dell'input, gestione dei file,
configurazione); NIST SP 800-115 per il metodo di verifica offensiva (usato per
scegliere i casi dei test, non come certificazione).

## 4. Ciclo di vita del token

1. L'utente incolla un fine-grained token con permesso *Contents* sul
   repository scelto.
2. `git_set_token` valida lunghezza (≤ 512) e assenza di caratteri di
   controllo, poi sostituisce il valore in memoria; il valore precedente viene
   azzerato da `Zeroizing`.
3. Il token non è restituito da nessun comando, non è scritto nel vault, non
   finisce in `.git/config` (l'URL con credenziali è rifiutato).
4. Alla chiusura del processo la memoria viene azzerata; alla riapertura
   dell'app il token non c'è e va reinserito.
5. `git_clear_token` lo cancella senza attendere la chiusura.

## 5. Rischi residui (dichiarati, non risolti)

| Rischio | Impatto | Nota |
|---|---|---|
| Decompression bomb Git | picco di memoria prima del controllo di dimensione del blob | libgit2 decomprime il blob prima che possiamo misurarlo; il limite scatta dopo. Mitigazione futura: pre-scansione del pack o streaming |
| TOCTOU su creazione cartelle nel vault | un processo locale ostile potrebbe sostituire una cartella con un symlink fra validazione e scrittura | richiede accesso al filesystem dell'utente, che implica già compromissione; il join rivalida comunque i componenti esistenti |
| Nessuna cifratura dei dati a riposo | chi legge la sandbox legge le note | ci si affida alla cifratura del dispositivo; non implementata una cifratura propria (chiavi e recupero sarebbero un rischio maggiore) |
| Backup automatico Android | le note potrebbero finire in un backup cloud | Tauri non disabilita `allowBackup`; da valutare `android:allowBackup="false"` nel manifest generato |
| Nessuna protezione screenshot su mobile | anteprima dell'app nelle app recenti mostra la pagina | valutare `FLAG_SECURE` / copertura all'inattività |
| Token in memoria non bloccato (`mlock`) | dump di memoria del processo | fuori scope per un'app locale; `Zeroizing` copre il caso normale |
| Nessun certificate pinning | CA compromessa o proxy aziendale legge il traffico | si usa lo store del sistema, scelta consapevole per non rompere proxy legittimi |
| `style-src 'unsafe-inline'` | necessario per gli attributi `style` dei campioni colore | non consente script; può essere eliminato spostando i colori in classi CSS |
| Vault aperto da una cartella scelta | un `.git/config` non fidato può puntare il remote o un proxy altrove | il selettore valida manifest, radice e schema del percorso, non la configurazione Git; non sincronizzare vault di provenienza sconosciuta |

## 6. Fuori scope

* Sicurezza del server Git dell'utente (permessi del repository, revisione
  delle pull request).
* Difesa da un dispositivo già compromesso o da un attaccante con accesso
  fisico sbloccato.
* Riservatezza del contenuto verso terze parti che ricevono il repository.

## 7. Come verificare

```bash
cargo test -p inkpad-core
```

I test rilevanti per la sicurezza sono:

* `paths::tests::rifiuta_traversal`, `rifiuta_symlink_antenato`;
* `vault::tests::rifiuta_cartella_pagine_symlink`,
  `rifiuta_id_di_pagina_con_traversal`, `un_file_manifest_corrotto_non_viene_riscritto`;
* `git::tests::rifiuta_symlink_nel_checkout`,
  `rifiuta_traversal_negli_alberi`,
  `pull_rifiuta_configurazione_con_credential_helper`,
  `pull_rifiutato_con_modifiche_non_committate`,
  `merge_senza_conflitti_e_conflitto_senza_scritture`;
* `error::tests::rifiuta_schemi_non_https`, `rifiuta_credenziali_nell_url`.

Non sono state eseguite prove su dispositivo né un test di penetrazione
dell'app compilata: la build Tauri non è disponibile in questo ambiente (vedi
README). Le affermazioni sopra riguardano il codice e i test automatici, non
un artefatto mobile firmato.
