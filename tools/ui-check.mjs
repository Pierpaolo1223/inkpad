/**
 * Verifica end-to-end del frontend, senza Tauri.
 *
 * Avvia Chrome headless, sostituisce `window.__TAURI_INTERNALS__` con un vault
 * in memoria e guida l'interfaccia come farebbe una persona (eventi mouse veri
 * sul canvas, click sui pulsanti). Serve a coprire la logica che i test Rust
 * non vedono: autosalvataggio, cambio pagina, aggiunta ed eliminazione.
 *
 * Uso: npm run test:ui   (avvia Vite se non è già in ascolto)
 */
import { execFileSync, spawn } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const APP_URL = "http://localhost:1420/";
const DEBUG_PORT = 9333;

// --- infrastruttura -------------------------------------------------------

function findChrome() {
  if (process.env.CHROME_PATH) return process.env.CHROME_PATH;
  for (const name of ["google-chrome", "chromium", "chromium-browser"]) {
    try {
      return execFileSync("which", [name], { encoding: "utf8" }).trim();
    } catch {
      // prova il prossimo
    }
  }
  throw new Error("Chrome/Chromium non trovato: imposta CHROME_PATH");
}

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

async function reachable(url) {
  try {
    const response = await fetch(url);
    return response.status < 500;
  } catch {
    return false;
  }
}

async function ensureServer() {
  if (await reachable(APP_URL)) return null;
  const vite = spawn("npx", ["vite", "--port", "1420", "--strictPort"], {
    cwd: new URL("..", import.meta.url).pathname,
    stdio: "ignore",
  });
  for (let attempt = 0; attempt < 60; attempt += 1) {
    if (await reachable(APP_URL)) return vite;
    await sleep(250);
  }
  vite.kill("SIGTERM");
  throw new Error(`Vite non risponde su ${APP_URL}`);
}

/** Vault in memoria: risponde ai comandi IPC che l'app usa davvero. */
const MOCK_SOURCE = `
(() => {
  const clone = (value) => JSON.parse(JSON.stringify(value));
  const initial = () => ({
    notebook: { id: "nb-1", title: "Prova", pages: ["pg-1", "pg-2"], ui: null },
    pages: {
      "pg-1": { schema: 1, id: "pg-1", paper: "ruled", text: "", strokes: [], updated_at: 1 },
      "pg-2": { schema: 1, id: "pg-2", paper: "ruled", text: "", strokes: [], updated_at: 1 },
    },
    saves: [],
    vaultPath: "/mock/vault",
    nextPick: null,
    deleted: false,
  });
  // In sessionStorage: sopravvive a un Page.reload, così si può verificare la
  // memoria di strumento e colori come dopo una riapertura.
  const KEY = "inkpad-mock-state";
  let state;
  try {
    state = JSON.parse(sessionStorage.getItem(KEY) || "null") || initial();
  } catch {
    state = initial();
  }
  const persist = () => {
    try { sessionStorage.setItem(KEY, JSON.stringify(state)); } catch {}
  };
  persist();
  let pageSeq = 100;
  window.__inkpadState = state;
  window.confirm = () => true;
  window.__TAURI_INTERNALS__ = {
    invoke: async (cmd, args = {}) => {
      switch (cmd) {
        case "vault_open":
          return { path: state.vaultPath, notebooks: state.deleted ? [] : [{ id: state.notebook.id, title: state.notebook.title, pages: state.notebook.pages.length, updated_at: 1 }] };
        case "vault_use":
          if (!args.create && args.path === "/mock/vault-nuovo") {
            throw { code: "vault_missing", message: "cartella senza vault" };
          }
          state.vaultPath = args.path;
          persist();
          return { path: state.vaultPath, notebooks: state.deleted ? [] : [{ id: state.notebook.id, title: state.notebook.title, pages: state.notebook.pages.length, updated_at: 1 }] };
        case "vault_pick":
          return state.nextPick || "/mock/vault-2";
        case "notebook_load":
          return { schema: 1, id: state.notebook.id, title: state.notebook.title, pages: state.notebook.pages.slice(), updated_at: 1, ui: state.notebook.ui };
        case "notebook_set_ui":
          state.notebook.ui = {
            tool: args.tool,
            pen_color: args.penColor,
            marker_color: args.markerColor,
          };
          persist();
          return { schema: 1, id: state.notebook.id, title: state.notebook.title, pages: state.notebook.pages.slice(), updated_at: 1, ui: state.notebook.ui };
        case "page_load":
          return clone(state.pages[args.pageId]);
        case "page_save": {
          const saved = clone(args.page);
          saved.updated_at = Date.now();
          state.pages[saved.id] = clone(saved);
          state.saves.push({ pageId: saved.id, strokes: saved.strokes.length, text: saved.text });
          persist();
          return clone(saved);
        }
        case "page_create": {
          pageSeq += 1;
          const created = { schema: 1, id: "pg-" + pageSeq, paper: args.paper, text: "", strokes: [], updated_at: Date.now() };
          state.pages[created.id] = clone(created);
          state.notebook.pages.push(created.id);
          persist();
          return clone(created);
        }
        case "page_delete":
          state.notebook.pages = state.notebook.pages.filter((id) => id !== args.pageId);
          delete state.pages[args.pageId];
          persist();
          return null;
        case "notebook_delete":
          for (const id of state.notebook.pages) delete state.pages[id];
          state.notebook.pages = [];
          state.deleted = true;
          persist();
          return null;
        case "git_status":
          return { initialized: false, branch: null, remote: null, dirty: false, ahead: 0, behind: 0, files: 0, head: null };
        default:
          throw { code: "mock_missing", message: "comando non simulato: " + cmd };
      }
    },
  };
})();
`;

class Session {
  #ws;
  #seq = 0;
  #pending = new Map();

  constructor(ws) {
    this.#ws = ws;
    ws.onmessage = (event) => {
      const message = JSON.parse(event.data);
      const resolve = this.#pending.get(message.id);
      if (resolve) {
        this.#pending.delete(message.id);
        resolve(message);
      }
    };
  }

  static async open(url) {
    const ws = new WebSocket(url);
    await new Promise((resolve, reject) => {
      ws.onopen = resolve;
      ws.onerror = reject;
    });
    return new Session(ws);
  }

  send(method, params = {}) {
    return new Promise((resolve) => {
      const id = this.#seq += 1;
      this.#pending.set(id, resolve);
      this.#ws.send(JSON.stringify({ id, method, params }));
    });
  }

  async eval(expression) {
    const response = await this.send("Runtime.evaluate", {
      expression,
      awaitPromise: true,
      returnByValue: true,
    });
    if (response.result?.exceptionDetails) {
      throw new Error(`${expression}\n${response.result.exceptionDetails.text}`);
    }
    return response.result?.result?.value;
  }

  close() {
    this.#ws.close();
  }
}

async function waitFor(check, { timeout = 6000, label = "condizione" } = {}) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    if (await check()) return;
    await sleep(100);
  }
  throw new Error(`Timeout in attesa di: ${label}`);
}

// --- scenari --------------------------------------------------------------

const failures = [];

function check(name, actual, expected) {
  const ok = JSON.stringify(actual) === JSON.stringify(expected);
  console.log(`${ok ? "ok  " : "FAIL"} ${name}${ok ? "" : ` → atteso ${JSON.stringify(expected)}, trovato ${JSON.stringify(actual)}`}`);
  if (!ok) failures.push(name);
}

async function main() {
  const server = await ensureServer();
  const profile = mkdtempSync(join(tmpdir(), "inkpad-ui-"));
  const chrome = spawn(
    findChrome(),
    [
      "--headless=new",
      `--remote-debugging-port=${DEBUG_PORT}`,
      `--user-data-dir=${profile}`,
      "--no-first-run",
      "--no-default-browser-check",
      "--disable-gpu",
      "about:blank",
    ],
    { stdio: "ignore" },
  );

  let session;
  try {
    for (let attempt = 0; attempt < 80; attempt += 1) {
      if (await reachable(`http://127.0.0.1:${DEBUG_PORT}/json/version`)) break;
      await sleep(100);
    }

    const target = await (
      await fetch(`http://127.0.0.1:${DEBUG_PORT}/json/new?about:blank`, { method: "PUT" })
    ).json();
    session = await Session.open(target.webSocketDebuggerUrl);
    await session.send("Page.enable");
    await session.send("Runtime.enable");
    await session.send("Page.addScriptToEvaluateOnNewDocument", { source: MOCK_SOURCE });
    await session.send("Page.navigate", { url: APP_URL });

    const indicator = () => session.eval("document.getElementById('page-indicator').textContent");
    const diskStrokes = (id) => session.eval(`window.__inkpadState.pages["${id}"].strokes.length`);
    const saves = () => session.eval("window.__inkpadState.saves.length");
    const click = (selector) => session.eval(`document.querySelector(${JSON.stringify(selector)}).click()`);

    await waitFor(async () => (await indicator()) === "1 / 2", { label: "prima pagina caricata" });
    check("avvio sulla prima pagina", await indicator(), "1 / 2");

    // Al primo accesso si apre sul testo, con il cursore già sulla pagina.
    check(
      "all'avvio è attivo il testo",
      await session.eval("document.getElementById('tool-text').getAttribute('aria-pressed')"),
      "true",
    );
    check("il cursore è sulla pagina", await session.eval("document.activeElement.id"), "page-text");
    await click("#tool-pen");

    const rect = await session.eval(`(() => {
      const r = document.getElementById('ink').getBoundingClientRect();
      return { x: Math.round(r.left + 60), y: Math.round(r.top + 80) };
    })()`);

    const drawStroke = async (offset) => {
      const start = { x: rect.x, y: rect.y + offset };
      const end = { x: rect.x + 70, y: rect.y + offset + 30 };
      await session.send("Input.dispatchMouseEvent", { type: "mousePressed", x: start.x, y: start.y, button: "left", clickCount: 1 });
      await session.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: end.x, y: end.y, button: "left" });
      await session.send("Input.dispatchMouseEvent", { type: "mouseReleased", x: end.x, y: end.y, button: "left", clickCount: 1 });
    };

    // Tratto 1: deve finire su disco dopo l'autosalvataggio.
    await drawStroke(0);
    await waitFor(async () => (await saves()) >= 1, { label: "primo autosalvataggio" });
    check("primo tratto salvato", await diskStrokes("pg-1"), 1);

    // Tratto 2 dopo un salvataggio: qui l'oggetto pagina non deve più cambiare
    // identità, altrimenti il tratto resta su una copia orfana.
    await drawStroke(40);
    await waitFor(async () => (await saves()) >= 2, { label: "secondo autosalvataggio" });
    check("secondo tratto salvato dopo un salvataggio", await diskStrokes("pg-1"), 2);

    // Cambio pagina immediato: il flush di navigazione deve portare con sé il
    // tratto appena disegnato, senza aspettare il timer di 450 ms.
    await drawStroke(80);
    await click("#next-page");
    await waitFor(async () => (await indicator()) === "2 / 2", { label: "cambio alla seconda pagina" });
    check("cambio pagina aggiorna subito l'indicatore", await indicator(), "2 / 2");
    check("tratto salvato dal cambio pagina", await diskStrokes("pg-1"), 3);

    await click("#prev-page");
    await waitFor(async () => (await indicator()) === "1 / 2", { label: "ritorno alla prima pagina" });
    check("ritorno alla prima pagina", await indicator(), "1 / 2");
    check("i tratti restano sulla pagina", await diskStrokes("pg-1"), 3);

    // --- testo dattiloscritto ed evidenziatore --------------------------
    const highlightsOn = () =>
      session.eval("(window.__inkpadState.pages['pg-1'].highlights || []).length");

    await click("#tool-text");
    await session.send("Input.insertText", { text: "Ciao sulle righe" });
    await waitFor(
      async () => (await session.eval("document.getElementById('page-text').value")) === "Ciao sulle righe",
      { label: "testo digitato" },
    );

    // Punto al centro della parola "sulle" (offset 5..10).
    const wordPoint = await session.eval(`(() => {
      const measure = document.getElementById('page-measure');
      const node = measure.firstChild;
      const range = document.createRange();
      range.setStart(node, node.textContent.indexOf('sulle'));
      range.setEnd(node, node.textContent.indexOf('sulle') + 5);
      const box = range.getBoundingClientRect();
      return { x: Math.round(box.left + box.width / 2), y: Math.round(box.top + box.height / 2) };
    })()`);

    const tap = async (point) => {
      await session.send("Input.dispatchMouseEvent", { type: "mousePressed", x: point.x, y: point.y, button: "left", clickCount: 1 });
      await session.send("Input.dispatchMouseEvent", { type: "mouseReleased", x: point.x, y: point.y, button: "left", clickCount: 1 });
    };

    // Da Testo a Evid. o Gomma: lo strumento scelto deve attivarsi subito,
    // senza un passaggio visivo sulla penna.
    const centerOf = (selector) =>
      session.eval(`(() => {
        const box = document.querySelector(${JSON.stringify(selector)}).getBoundingClientRect();
        return { x: Math.round(box.left + box.width / 2), y: Math.round(box.top + box.height / 2) };
      })()`);
    const recordToolTrail = () =>
      session.eval(`(() => {
        const ids = ['tool-pen', 'tool-marker', 'tool-eraser', 'tool-text'];
        const state = () =>
          ids.filter((id) => document.getElementById(id).getAttribute('aria-pressed') === 'true').join('+') || 'none';
        window.__toolTrail = [state()];
        const observer = new MutationObserver(() => {
          const next = state();
          if (window.__toolTrail[window.__toolTrail.length - 1] !== next) window.__toolTrail.push(next);
        });
        for (const id of ids) {
          observer.observe(document.getElementById(id), { attributes: true, attributeFilter: ['aria-pressed'] });
        }
        return window.__toolTrail;
      })()`);

    const markerCenter = await centerOf("#tool-marker");
    const eraserCenter = await centerOf("#tool-eraser");
    const textCenter = await centerOf("#tool-text");

    await recordToolTrail();
    await tap(markerCenter);
    check("da Testo a Evid. senza passare dalla penna", await session.eval("window.__toolTrail"), [
      "tool-text",
      "tool-marker",
    ]);

    // Con il mouse il `mousedown` del pulsante non deve rubare il focus al
    // textarea: altrimenti il `blur` uscirebbe subito dal testo e Testo non
    // resterebbe mai attivo dopo aver usato un altro strumento.
    await tap(textCenter);
    check(
      "dall'Evid. si torna al testo",
      await session.eval("document.getElementById('tool-text').getAttribute('aria-pressed')"),
      "true",
    );
    check(
      "tornando al testo il cursore resta sulla pagina",
      await session.eval("document.activeElement.id"),
      "page-text",
    );

    await recordToolTrail();
    await tap(eraserCenter);
    check("da Testo a Gomma senza passare dalla penna", await session.eval("window.__toolTrail"), [
      "tool-text",
      "tool-eraser",
    ]);

    await tap(markerCenter);
    check(
      "Evid. dopo la gomma riparte dal giallo",
      await session.eval(
        "document.querySelector('.swatch[data-color=\"#f2c200\"]').hasAttribute('data-active')",
      ),
      true,
    );
    await tap(wordPoint);
    await waitFor(async () => (await highlightsOn()) === 1, { label: "evidenziazione salvata" });
    check("evidenzia solo la parola toccata", await session.eval("window.__inkpadState.pages['pg-1'].highlights"), [
      { start: 5, end: 10, color: "#f2c200" },
    ]);
    check(
      "un rettangolo colorato sulla pagina",
      await session.eval("document.querySelectorAll('#page-highlight i').length"),
      1,
    );
    check("l'evidenziatore non lascia tratti", await diskStrokes("pg-1"), 3);
    check(
      "il rettangolo copre la parola",
      await session.eval(`(() => {
        const mark = document.querySelector('#page-highlight i').getBoundingClientRect();
        const node = document.getElementById('page-measure').firstChild;
        const range = document.createRange();
        range.setStart(node, 5);
        range.setEnd(node, 10);
        const word = range.getBoundingClientRect();
        return (
          Math.abs(mark.left - word.left) <= 2 &&
          Math.abs(mark.top - word.top) <= 2 &&
          Math.abs(mark.width - word.width) <= 3
        );
      })()`),
      true,
    );

    await tap(wordPoint);
    await waitFor(async () => (await highlightsOn()) === 0, { label: "evidenziazione tolta" });
    check("secondo tocco con lo stesso colore toglie", await highlightsOn(), 0);

    await tap(wordPoint);
    await waitFor(async () => (await highlightsOn()) === 1, { label: "evidenziazione riapplicata" });
    await click("#undo");
    await waitFor(async () => (await highlightsOn()) === 0, { label: "annulla evidenziazione" });
    check("annulla toglie l'evidenziazione", await highlightsOn(), 0);
    await click("#redo");
    await waitFor(async () => (await highlightsOn()) === 1, { label: "ripeti evidenziazione" });
    check("ripeti la riapplica", await highlightsOn(), 1);

    await click("#next-page");
    await waitFor(async () => (await indicator()) === "2 / 2", { label: "pagina 2 per le evidenziazioni" });
    check(
      "sulla pagina 2 non ci sono evidenziazioni",
      await session.eval("document.querySelectorAll('#page-highlight i').length"),
      0,
    );
    await click("#prev-page");
    await waitFor(async () => (await indicator()) === "1 / 2", { label: "ritorno con l'evidenziazione" });
    check(
      "l'evidenziazione resta al ritorno",
      await session.eval("document.querySelectorAll('#page-highlight i').length"),
      1,
    );

    // --- modificare il testo sposta o toglie le evidenziazioni -----------
    const setSelection = (start, end) =>
      session.eval(`(() => {
        const area = document.getElementById('page-text');
        area.focus();
        area.setSelectionRange(${start}, ${end});
        return area.selectionStart;
      })()`);

    await click("#tool-text");
    await setSelection(0, 0);
    await session.send("Input.insertText", { text: "Oggi " });
    await waitFor(
      async () => (await session.eval("window.__inkpadState.pages['pg-1'].text")) === "Oggi Ciao sulle righe",
      { label: "testo modificato prima della parola" },
    );
    check(
      "scrivere prima sposta l'evidenziazione",
      await session.eval("window.__inkpadState.pages['pg-1'].highlights"),
      [{ start: 10, end: 15, color: "#f2c200" }],
    );

    // "sulle" ora è a 10..15: il cursore dentro la parola la rende diversa.
    await setSelection(12, 12);
    await session.send("Input.insertText", { text: "X" });
    await waitFor(async () => (await highlightsOn()) === 0, { label: "evidenziazione tolta dall'editing" });
    check(
      "modificare la parola toglie l'evidenziazione",
      await session.eval("document.querySelectorAll('#page-highlight i').length"),
      0,
    );
    check(
      "il testo resta quello digitato",
      await session.eval("window.__inkpadState.pages['pg-1'].text"),
      "Oggi Ciao suXlle righe",
    );
    await click("#tool-pen");

    // Aggiunta ed eliminazione di pagina: stato aggiornato al primo click.
    await click("#add-page");
    await waitFor(async () => (await indicator()) === "3 / 3", { label: "nuova pagina" });
    check("nuova pagina selezionata subito", await indicator(), "3 / 3");

    await click("#prev-page");
    await waitFor(async () => (await indicator()) === "2 / 3", { label: "pagina precedente" });
    check("pagina precedente dopo l'aggiunta", await indicator(), "2 / 3");

    await click("#delete-page");
    await waitFor(async () => (await indicator()) === "2 / 2", { label: "pagina eliminata" });
    check("eliminazione aggiorna subito l'indicatore", await indicator(), "2 / 2");
    check(
      "pagine rimaste",
      await session.eval("window.__inkpadState.notebook.pages.length"),
      2,
    );

    // --- memoria per quaderno -------------------------------------------
    await tap(markerCenter);
    await click('.swatch[data-color="#1d4ed8"]');
    const storedUi = async () =>
      session.eval("JSON.stringify(window.__inkpadState.notebook.ui ?? null)");
    await waitFor(async () => (await storedUi()) !== "null", {
      label: "preferenze scritte nel quaderno",
    });
    check(
      "strumento e colori salvati nel quaderno",
      JSON.parse(await storedUi()),
      { tool: "marker", pen_color: "#1f2933", marker_color: "#1d4ed8" },
    );
    check(
      "le preferenze non restano fuori dal quaderno",
      await session.eval("localStorage.getItem('inkpad:tool:nb-1')"),
      null,
    );

    // --- apertura di un altro vault -------------------------------------
    await click("#open-vault");
    await waitFor(
      async () =>
        (await session.eval("document.getElementById('vault-path').textContent")) ===
        "/mock/vault-2",
      { label: "vault scelto" },
    );
    check(
      "apertura vault aggiorna il percorso",
      await session.eval("document.getElementById('vault-path').textContent"),
      "/mock/vault-2",
    );
    check("dopo il cambio si apre il primo quaderno", await indicator(), "1 / 2");

    await session.eval("window.__inkpadState.nextPick = '/mock/vault-nuovo'");
    await click("#open-vault");
    await waitFor(
      async () =>
        (await session.eval("document.getElementById('vault-path').textContent")) ===
        "/mock/vault-nuovo",
      { label: "nuovo vault confermato" },
    );
    check(
      "la conferma crea il vault scelto",
      await session.eval("document.getElementById('vault-path').textContent"),
      "/mock/vault-nuovo",
    );

    await session.send("Page.reload");
    await waitFor(async () => (await indicator()) === "1 / 2", { label: "app ricaricata" });
    check(
      "riaprendo il quaderno riprende Evid.",
      await session.eval("document.getElementById('tool-marker').getAttribute('aria-pressed')"),
      "true",
    );
    check(
      "riaprendo riprende il colore dell'evidenziatore",
      await session.eval(
        "document.querySelector('.swatch[data-color=\"#1d4ed8\"]').hasAttribute('data-active')",
      ),
      true,
    );

    // --- eliminazione del quaderno --------------------------------------
    await click(".notebook-delete");
    await waitFor(
      async () => (await session.eval("document.querySelectorAll('.notebook-item').length")) === 0,
      { label: "quaderno eliminato dalla sidebar" },
    );
    check(
      "eliminazione quaderno rimuove la voce",
      await session.eval("document.querySelectorAll('.notebook-item').length"),
      0,
    );
    check(
      "dopo l'eliminazione compare lo stato vuoto",
      await session.eval("document.getElementById('empty-state').hidden"),
      false,
    );
  } finally {
    session?.close();
    chrome.kill("SIGTERM");
    await sleep(300);
    rmSync(profile, { recursive: true, force: true });
    if (server) server.kill("SIGTERM");
  }

  if (failures.length > 0) {
    console.error(`\n${failures.length} verifiche fallite.`);
    process.exit(1);
  }
  console.log("\nTutte le verifiche sono passate.");
}

await main();
