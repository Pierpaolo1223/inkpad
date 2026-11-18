import "./style.css";
import { api, ApiError } from "./api";
import type {
  GitStatus,
  Notebook,
  NotebookSummary,
  NotebookUi,
  Page,
  Paper,
  UiTool,
} from "./api";
import { InkSurface } from "./ink/canvas";
import { PAGE_HEIGHT, PAGE_WIDTH } from "./ink/model";
import { highlightWord, shiftHighlights, wordRanges } from "./text/highlights";
import { highlightRects, wordUnderPoint } from "./text/layout";

function $<T extends HTMLElement>(id: string): T {
  const element = document.getElementById(id);
  if (!element) throw new Error(`Elemento #${id} mancante`);
  return element as T;
}

const canvas = $<HTMLCanvasElement>("ink");
const frame = $("page-frame");
const pageScroll = $("page-scroll");
const pageText = $<HTMLTextAreaElement>("page-text");
const pageMeasure = $<HTMLDivElement>("page-measure");
const pageHighlight = $<HTMLDivElement>("page-highlight");
const notebookTitle = $("notebook-title");
const notebookList = $<HTMLUListElement>("notebook-list");
const newNotebookForm = $<HTMLFormElement>("new-notebook-form");
const newNotebookTitle = $<HTMLInputElement>("new-notebook-title");
const saveState = $("save-state");
const pageIndicator = $("page-indicator");
const paperSelect = $<HTMLSelectElement>("paper-select");
const emptyState = $("empty-state");
const emptyMessage = $("empty-message");
const emptyAction = $<HTMLButtonElement>("empty-action");
const toastEl = $("toast");
const vaultPath = $("vault-path");
const gitDialog = $<HTMLDialogElement>("git-dialog");
const gitStatusEl = $("git-status");
const gitInitFields = $("git-init-fields");
const gitSyncFields = $("git-sync-fields");
const gitName = $<HTMLInputElement>("git-name");
const gitEmail = $<HTMLInputElement>("git-email");
const gitRemote = $<HTMLInputElement>("git-remote");
const gitToken = $<HTMLInputElement>("git-token");
const gitMessage = $<HTMLInputElement>("git-message");
const gitLog = $<HTMLPreElement>("git-log");

let notebooks: NotebookSummary[] = [];
let notebook: Notebook | null = null;
let page: Page | null = null;
let pageIndex = 0;
let dirty = false;
let editVersion = 0;
let flushChain: Promise<void> = Promise.resolve();
let saveTimer: number | null = null;
let toastTimer: number | null = null;
/**
 * Un colore per strumento: la penna parte dall'inchiostro, l'evidenziatore dal
 * giallo. Cambiare colore con Evid. attivo non altera quello della penna.
 */
let penColor = "#1f2933";
let markerColor = "#f2c200";
/** Ultimo strumento di disegno, per uscire dal testo senza passare dalla penna. */
let lastDrawingTool = "tool-pen";

type ToolId = "tool-pen" | "tool-marker" | "tool-eraser" | "tool-text";
const TOOL_IDS: ToolId[] = ["tool-pen", "tool-marker", "tool-eraser", "tool-text"];
/** Preferenze scritte dalle versioni che le tenevano solo su questa macchina. */
const LEGACY_PREFS_PREFIX = "inkpad:tool:";

const ink = new InkSurface(canvas, () => {
  scheduleSave();
  renderHighlights();
});

// --- stato visivo -------------------------------------------------------

function setSaveState(text: string, kind: "idle" | "saving" | "error" = "idle"): void {
  saveState.textContent = text;
  saveState.dataset.kind = kind;
}

function toast(message: string): void {
  toastEl.textContent = message;
  toastEl.hidden = false;
  if (toastTimer !== null) window.clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => {
    toastEl.hidden = true;
  }, 4000);
}

function describeError(error: unknown): string {
  if (error instanceof ApiError) {
    switch (error.code) {
      case "auth_required":
        return "Serve un token di accesso per il remote HTTPS.";
      case "conflict":
        return "Conflitto: nessun file è stato modificato. Fai commit e riprova.";
      case "busy":
        return "Una sincronizzazione è già in corso.";
      case "no_repo":
        return "Configura prima il repository Git.";
      default:
        return error.message;
    }
  }
  return error instanceof Error ? error.message : String(error);
}

function frameClass(paper: Paper): string {
  return `page-frame paper-${paper}`;
}

/**
 * Passo verticale della carta in frazione dell'altezza della pagina: deve
 * coincidere con `background-size` di `.page-frame.paper-*` in style.css,
 * altrimenti il testo non cade sulle righe.
 */
const PAPER_RHYTHM: Record<Paper, number> = {
  ruled: 0.0312,
  squared: 0.025,
  dotted: 0.025,
  plain: 0.0312,
};

/** Allinea il livello di testo alla riga o alla griglia della carta corrente. */
function syncTextLayout(): void {
  const paper = page?.paper ?? "ruled";
  const pageHeight = frame.clientWidth * (PAGE_HEIGHT / PAGE_WIDTH);
  const rule = pageHeight * PAPER_RHYTHM[paper];
  frame.style.setProperty("--rule", `${rule.toFixed(2)}px`);
  renderHighlights();
}

/**
 * Colora lo sfondo delle parole evidenziate. Le posizioni vengono dal livello
 * di misura (stessa metrica del textarea); se il testo è più lungo della
 * pagina, il livello accompagna lo scorrimento dell'editor.
 */
function renderHighlights(): void {
  pageMeasure.textContent = page?.text ?? "";
  const rects = page ? highlightRects(pageMeasure, page.highlights ?? []) : [];
  pageHighlight.replaceChildren(
    ...rects.map((rect) => {
      const mark = document.createElement("i");
      mark.style.left = `${rect.left}px`;
      mark.style.top = `${rect.top}px`;
      mark.style.width = `${rect.width}px`;
      mark.style.height = `${rect.height}px`;
      mark.style.backgroundColor = rect.color;
      return mark;
    }),
  );
  pageHighlight.style.transform = `translateY(${-pageText.scrollTop}px)`;
}

function setTextMode(enabled: boolean, focus = true): void {
  pageText.classList.toggle("editing", enabled);
  if (!enabled) {
    pageText.blur();
  } else if (focus) {
    pageText.focus({ preventScroll: true });
  }
}

function updateEmptyState(): void {
  if (notebooks.length === 0) {
    emptyState.hidden = false;
    emptyMessage.textContent = "Nessun quaderno. Creane uno per iniziare a scrivere.";
    emptyAction.hidden = false;
    emptyAction.textContent = "Crea quaderno";
    emptyAction.dataset.action = "notebook";
    return;
  }
  if (notebook && notebook.pages.length === 0) {
    emptyState.hidden = false;
    emptyMessage.textContent = "Questo quaderno è ancora vuoto.";
    emptyAction.hidden = false;
    emptyAction.textContent = "Nuova pagina";
    emptyAction.dataset.action = "page";
    return;
  }
  emptyState.hidden = true;
}

function updatePageChrome(): void {
  const count = notebook?.pages.length ?? 0;
  pageIndicator.textContent = count > 0 ? `${pageIndex + 1} / ${count}` : "— / —";
  notebookTitle.textContent = notebook?.title ?? "Nessun quaderno";
  document.title = notebook ? `${notebook.title} — Inkpad` : "Inkpad";
  paperSelect.value = page?.paper ?? "ruled";
  frame.className = frameClass(page?.paper ?? "ruled");
  syncTextLayout();
  $<HTMLButtonElement>("prev-page").disabled = pageIndex <= 0;
  $<HTMLButtonElement>("next-page").disabled = pageIndex >= count - 1;
  $<HTMLButtonElement>("delete-page").disabled = page === null;
  $<HTMLButtonElement>("undo").disabled = !ink.canUndo();
  $<HTMLButtonElement>("redo").disabled = !ink.canRedo();
  updateEmptyState();
}

/**
 * Strumento e colori dell'ultimo uso sono preferenze del quaderno: stanno in
 * `notebook.json`, quindi seguono il quaderno (e i suoi commit) su ogni
 * dispositivo. Restano in memoria finché il file non è scritto; se la
 * scrittura fallisce, `uiPending` le rimanda al prossimo salvataggio.
 */
let uiPending = false;

function validHex(value: unknown): string | null {
  return typeof value === "string" && /^#[0-9a-f]{6}$/i.test(value) ? value : null;
}

function activeTool(): ToolId | null {
  return TOOL_IDS.find((id) => $(id).getAttribute("aria-pressed") === "true") ?? null;
}

function currentUi(): NotebookUi {
  return {
    tool: (activeTool() ?? "tool-pen").slice("tool-".length) as UiTool,
    pen_color: penColor,
    marker_color: markerColor,
  };
}

function sameUi(a: NotebookUi | null | undefined, b: NotebookUi): boolean {
  return (
    a != null &&
    a.tool === b.tool &&
    a.pen_color === b.pen_color &&
    a.marker_color === b.marker_color
  );
}

function saveToolPrefs(): void {
  if (!notebook) return;
  const next = currentUi();
  if (sameUi(notebook.ui, next)) return;
  notebook.ui = next;
  uiPending = true;
  void pushToolPrefs();
}

async function pushToolPrefs(): Promise<void> {
  if (!uiPending || !notebook) return;
  const target = notebook;
  const ui = target.ui;
  if (!ui) return;
  try {
    await api.notebookSetUi(target.id, ui.tool, ui.pen_color, ui.marker_color);
    if (notebook === target && sameUi(notebook.ui, ui)) uiPending = false;
  } catch {
    // Resta `uiPending`: si riprova a ogni salvataggio o sincronizzazione,
    // così un problema del vault non fa perdere la scelta dello strumento.
  }
}

/**
 * Preferenze delle versioni precedenti, tenute solo in `localStorage`: si
 * leggono una volta e vengono salvate nel quaderno, senza perdere la scelta
 * già fatta (e senza restare un secondo posto dove guardare).
 */
function takeLegacyPrefs(): { tool: ToolId; penColor: string; markerColor: string } | null {
  const id = notebook?.id;
  if (!id) return null;
  const key = `${LEGACY_PREFS_PREFIX}${id}`;
  try {
    const raw = localStorage.getItem(key);
    if (!raw) return null;
    localStorage.removeItem(key);
    const parsed = JSON.parse(raw) as Record<string, unknown>;
    return {
      tool: TOOL_IDS.find((candidate) => candidate === parsed.tool) ?? "tool-text",
      penColor: validHex(parsed.penColor) ?? "#1f2933",
      markerColor: validHex(parsed.markerColor) ?? "#f2c200",
    };
  } catch {
    return null;
  }
}

function storedToolPrefs(): { tool: ToolId; penColor: string; markerColor: string } | null {
  const ui = notebook?.ui;
  if (ui) {
    return {
      tool: `tool-${ui.tool}` as ToolId,
      penColor: validHex(ui.pen_color) ?? "#1f2933",
      markerColor: validHex(ui.marker_color) ?? "#f2c200",
    };
  }
  return takeLegacyPrefs();
}

/**
 * Ripristina lo strumento e i colori dell'ultima volta in questo quaderno. Al
 * primo accesso lo strumento è Testo: chi apre l'app deve vedere subito che si
 * può scrivere sulle righe.
 */
function applyToolPrefs(options: { focus?: boolean; announce?: boolean } = {}): void {
  if (!page) return;
  const prefs = storedToolPrefs();
  if (prefs) {
    penColor = prefs.penColor;
    markerColor = prefs.markerColor;
  }
  // `selectTool` salva le preferenze: se venivano da `localStorage` finiscono
  // qui nel quaderno, altrimenti non cambia nulla.
  selectTool($(prefs?.tool ?? "tool-text"), options);
}

function markSwatch(color: string): void {
  for (const swatch of Array.from(document.querySelectorAll<HTMLButtonElement>(".swatch"))) {
    swatch.toggleAttribute("data-active", swatch.dataset.color === color);
  }
}

function selectTool(
  button: HTMLElement,
  options: { focus?: boolean; announce?: boolean } = {},
): void {
  for (const id of TOOL_IDS) {
    $(id).removeAttribute("data-active");
    $(id).setAttribute("aria-pressed", "false");
  }
  button.setAttribute("data-active", "");
  button.setAttribute("aria-pressed", "true");

  if (button.id === "tool-text") {
    setTextMode(true, options.focus ?? true);
    if (options.announce ?? true) {
      toast("Scrivi sulle righe. Esc per uscire dal testo.");
    }
    saveToolPrefs();
    return;
  }

  lastDrawingTool = button.id;
  setTextMode(false);
  if (button.id === "tool-eraser") {
    ink.setEraser(true);
  } else {
    ink.setEraser(false);
    ink.setTool(button.id === "tool-marker" ? "marker" : "pen");
    const color = button.id === "tool-marker" ? markerColor : penColor;
    ink.setColor(color);
    markSwatch(color);
  }
  saveToolPrefs();
}

/**
 * Chi sceglie penna, gomma o colore vuole disegnare: il testo resta visibile e
 * si torna all'ultimo strumento di disegno, non necessariamente alla penna.
 */
function exitTextMode(): void {
  if ($("tool-text").getAttribute("aria-pressed") === "true") {
    selectTool($(lastDrawingTool));
  }
}

/**
 * La scelta avviene sul `pointerdown`, cioè prima che il `blur` del textarea
 * possa far uscire dalla modalità testo: così Evid. e Gomma si attivano
 * direttamente, senza un passaggio visivo sulla penna.
 */
function bindTool(id: string): void {
  const button = $(id);
  button.addEventListener("pointerdown", (event) => {
    if (event.pointerType === "mouse" && event.button !== 0) return;
    // Senza questo, il mousedown sposta il focus sul pulsante e il `blur` del
    // textarea uscirebbe subito dal testo: scegliere Testo con il mouse non
    // funzionerebbe dopo aver usato un altro strumento. La scelta è già qui.
    event.preventDefault();
    chooseTool(button);
  });
  button.addEventListener("click", (event) => {
    // Un click del mouse (detail > 0) è già stato gestito dal pointerdown;
    // qui restano solo Enter e Spazio da tastiera.
    if (event.detail > 0) return;
    chooseTool(button);
  });
}

function chooseTool(button: HTMLElement): void {
  if (button.id === "tool-text" && button.getAttribute("aria-pressed") === "true") {
    exitTextMode();
    return;
  }
  if (button.id === "tool-text" && !page) {
    toast("Crea prima una pagina: poi potrai scriverci sopra.");
    return;
  }
  selectTool(button);
}

// --- salvataggio --------------------------------------------------------

function scheduleSave(): void {
  if (!page) return;
  dirty = true;
  editVersion += 1;
  setSaveState("Modifiche non salvate");
  if (saveTimer !== null) window.clearTimeout(saveTimer);
  saveTimer = window.setTimeout(() => {
    saveTimer = null;
    void flush();
  }, 450);
}

async function doFlush(): Promise<void> {
  if (!dirty || !page || !notebook) return;
  const version = editVersion;
  const currentNotebook = notebook;
  page.text = pageText.value;
  setSaveState("Salvataggio…", "saving");

  try {
    const saved = await api.pageSave(currentNotebook.id, page);
    // L'oggetto pagina non viene mai sostituito con quello restituito dal
    // comando: InkSurface conserva il proprio riferimento e altrimenti
    // continuerebbe a disegnare su una copia orfana, che il salvataggio
    // successivo non include. Si riporta indietro solo `updated_at`.
    if (page && page.id === saved.id) page.updated_at = saved.updated_at;
    if (editVersion === version) {
      dirty = false;
      setSaveState("Salvato");
    }
  } catch (error) {
    setSaveState("Non salvato", "error");
    toast(describeError(error));
  }
  updatePageChrome();
}

function flush(): Promise<void> {
  if (saveTimer !== null) {
    window.clearTimeout(saveTimer);
    saveTimer = null;
  }
  const run = async () => {
    await doFlush();
    await pushToolPrefs();
  };
  flushChain = flushChain.then(run, run);
  return flushChain;
}

// --- navigazione --------------------------------------------------------

function renderNotebookList(): void {
  notebookList.replaceChildren();
  for (const summary of notebooks) {
    const item = document.createElement("li");
    const button = document.createElement("button");
    button.type = "button";
    button.className = "notebook-item";
    if (notebook?.id === summary.id) button.setAttribute("aria-current", "page");

    const title = document.createElement("span");
    title.className = "notebook-name";
    title.textContent = summary.title;

    const meta = document.createElement("span");
    meta.className = "notebook-meta";
    meta.textContent = summary.pages === 1 ? "1 pagina" : `${summary.pages} pagine`;

    button.append(title, meta);
    button.addEventListener("click", () => {
      void openNotebook(summary.id);
    });

    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "notebook-delete";
    remove.title = "Elimina quaderno";
    remove.setAttribute("aria-label", `Elimina il quaderno «${summary.title}»`);
    remove.textContent = "×";
    remove.addEventListener("click", () => {
      void deleteNotebook(summary);
    });

    item.append(button, remove);
    notebookList.append(item);
  }
}

async function refreshNotebooks(): Promise<void> {
  const view = await api.vaultOpen();
  notebooks = view.notebooks;
  vaultPath.textContent = view.path;
  vaultPath.title = view.path;
  renderNotebookList();
  updateEmptyState();
}

async function openNotebook(id: string): Promise<void> {
  await flush();
  notebook = await api.notebookLoad(id);
  pageIndex = 0;
  renderNotebookList();
  if (notebook.pages.length > 0) {
    await loadPage(0);
  } else {
    page = null;
    ink.setPage(null);
    pageText.value = "";
    updatePageChrome();
  }
  applyToolPrefs({ focus: false, announce: false });
}

async function loadPage(index: number): Promise<void> {
  if (!notebook || notebook.pages.length === 0) return;
  await flush();
  const pageId = notebook.pages[index];
  if (!pageId) return;
  const loaded = await api.pageLoad(notebook.id, pageId);
  page = loaded;
  pageIndex = index;
  ink.setPage(loaded);
  pageText.value = loaded.text ?? "";
  setSaveState("Salvato");
  pageScroll.scrollTop = 0;
  dirty = false;
  updatePageChrome();
}

async function reloadCurrentNotebook(): Promise<void> {
  if (!notebook) return;
  const currentPageId = page?.id ?? null;
  const list = await api.notebookLoad(notebook.id);
  notebook = list;
  const index = currentPageId ? list.pages.indexOf(currentPageId) : 0;
  pageIndex = index >= 0 ? index : 0;
  if (list.pages.length > 0) {
    await loadPage(pageIndex);
  } else {
    page = null;
    ink.setPage(null);
    pageText.value = "";
    updatePageChrome();
  }
  // Dopo un pull può essere arrivato lo strumento scelto su un altro
  // dispositivo: si riprende quello, senza rubare il focus o ripetere l'avviso.
  applyToolPrefs({ focus: false, announce: false });
}

async function createPage(): Promise<void> {
  if (!notebook) {
    newNotebookTitle.focus();
    return;
  }
  await flush();
  const paper = (paperSelect.value as Paper) || "ruled";
  const created = await api.pageCreate(notebook.id, paper);
  notebook.pages.push(created.id);
  pageIndex = notebook.pages.length - 1;
  await loadPage(pageIndex);
  applyToolPrefs({ focus: false, announce: false });
}

async function deleteCurrentPage(): Promise<void> {
  if (!notebook || !page) return;
  if (!window.confirm("Eliminare questa pagina? L'operazione sarà inclusa nel prossimo commit.")) {
    return;
  }
  const removedIndex = pageIndex;
  await api.pageDelete(notebook.id, page.id);
  notebook.pages.splice(removedIndex, 1);
  page = null;
  ink.setPage(null);
  pageText.value = "";
  if (notebook.pages.length > 0) {
    await loadPage(Math.max(0, Math.min(removedIndex, notebook.pages.length - 1)));
  } else {
    updatePageChrome();
  }
  void refreshNotebooks();
}

/**
 * Elimina il quaderno intero (cartella, pagine e voce nel manifesto). Se era
 * quello aperto, si passa al primo rimasto; se non ne restano, si mostra lo
 * stato vuoto.
 */
async function deleteNotebook(summary: NotebookSummary): Promise<void> {
  const message = `Eliminare il quaderno «${summary.title}» e tutte le sue pagine? L'operazione sarà inclusa nel prossimo commit.`;
  if (!window.confirm(message)) return;
  try {
    await flush();
    await api.notebookDelete(summary.id);
    if (notebook?.id === summary.id) {
      notebook = null;
      page = null;
      dirty = false;
      ink.setPage(null);
      pageText.value = "";
      setSaveState("");
    }
    await refreshNotebooks();
    if (!notebook) {
      const first = notebooks[0];
      if (first) {
        await openNotebook(first.id);
      } else {
        updatePageChrome();
      }
    }
    toast(`Quaderno «${summary.title}» eliminato.`);
  } catch (error) {
    toast(describeError(error));
  }
}

// --- Git ----------------------------------------------------------------

function appendGitLog(line: string): void {
  const time = new Date().toLocaleTimeString("it-IT");
  gitLog.textContent = `${gitLog.textContent ?? ""}[${time}] ${line}\n`;
  gitLog.scrollTop = gitLog.scrollHeight;
}

function describeGitStatus(status: GitStatus): string {
  if (!status.initialized) return "Repository non inizializzato: i file sono salvati solo su questo dispositivo.";
  const parts = [`Branch ${status.branch ?? "?"}`];
  parts.push(status.remote ? `remote ${status.remote}` : "nessun remote");
  if (status.head) parts.push(`HEAD ${status.head}`);
  parts.push(status.dirty ? `${status.files} modifiche non committate` : "working tree pulito");
  if (status.ahead > 0) parts.push(`${status.ahead} da pushare`);
  if (status.behind > 0) parts.push(`${status.behind} da pullare`);
  return parts.join(" · ");
}

async function refreshGit(): Promise<void> {
  try {
    const status = await api.gitStatus();
    gitStatusEl.textContent = describeGitStatus(status);
    gitInitFields.hidden = status.initialized;
    gitSyncFields.hidden = !status.initialized;
    if (status.remote && gitRemote.value === "") gitRemote.value = status.remote;
  } catch (error) {
    gitStatusEl.textContent = describeError(error);
  }
}

async function openGitDialog(): Promise<void> {
  await flush();
  gitLog.textContent = "";
  gitDialog.showModal();
  await refreshGit();
}

// --- avvio --------------------------------------------------------------

async function start(): Promise<void> {
  try {
    await refreshNotebooks();
    const first = notebooks[0];
    if (first) {
      await openNotebook(first.id);
      // Si apre sul testo al primo accesso, poi sull'ultimo strumento usato in
      // questo quaderno: chi entra vede subito che si può scrivere sulle righe.
      if (page) {
        applyToolPrefs({ focus: true, announce: true });
      } else {
        selectTool($("tool-pen"), { announce: false });
      }
    } else {
      selectTool($("tool-pen"), { announce: false });
      updatePageChrome();
    }
  } catch (error) {
    setTimeout(() => toast(describeError(error)), 0);
  }
}

newNotebookForm.addEventListener("submit", (event) => {
  event.preventDefault();
  const title = newNotebookTitle.value.trim();
  if (!title) return;
  void (async () => {
    try {
      const created = await api.notebookCreate(title);
      newNotebookTitle.value = "";
      await refreshNotebooks();
      await openNotebook(created.id);
      await createPage();
    } catch (error) {
      toast(describeError(error));
    }
  })();
});

emptyAction.addEventListener("click", () => {
  const action = emptyAction.dataset.action;
  if (action === "page") void createPage();
  else newNotebookTitle.focus();
});

$("add-page").addEventListener("click", () => void createPage());
$("delete-page").addEventListener("click", () => void deleteCurrentPage());
$("prev-page").addEventListener("click", () => void loadPage(Math.max(0, pageIndex - 1)));
$("next-page").addEventListener("click", () => {
  if (notebook) void loadPage(Math.min(notebook.pages.length - 1, pageIndex + 1));
});

for (const id of ["tool-pen", "tool-marker", "tool-eraser", "tool-text"]) bindTool(id);

for (const swatch of Array.from(document.querySelectorAll<HTMLButtonElement>(".swatch"))) {
  swatch.addEventListener("click", () => {
    const color = swatch.dataset.color;
    if (!color) return;
    exitTextMode();
    if (ink.getTool() === "marker" && !ink.isErasing()) {
      markerColor = color;
      ink.setColor(color);
      markSwatch(color);
      saveToolPrefs();
      return;
    }
    penColor = color;
    ink.setColor(color);
    selectTool($("tool-pen"));
  });
}

for (const widthButton of Array.from(document.querySelectorAll<HTMLButtonElement>(".width-btn"))) {
  widthButton.addEventListener("click", () => {
    const width = Number(widthButton.dataset.width);
    if (!Number.isFinite(width) || width <= 0) return;
    exitTextMode();
    ink.setWidth(width);
    for (const other of Array.from(document.querySelectorAll<HTMLButtonElement>(".width-btn"))) {
      other.toggleAttribute("data-active", other === widthButton);
    }
  });
}

paperSelect.addEventListener("change", () => {
  if (!page) return;
  const paper = paperSelect.value as Paper;
  page.paper = paper;
  ink.setPaper(paper);
  frame.className = frameClass(paper);
  syncTextLayout();
  scheduleSave();
});

pageText.addEventListener("input", () => {
  if (!page) return;
  const next = pageText.value;
  page.highlights = shiftHighlights(page.highlights ?? [], page.text ?? "", next);
  page.text = next;
  renderHighlights();
  scheduleSave();
});

pageText.addEventListener("scroll", renderHighlights);

pageText.addEventListener("keydown", (event) => {
  if (event.key === "Escape") {
    event.preventDefault();
    exitTextMode();
  }
});

pageText.addEventListener("blur", () => exitTextMode());

/**
 * L'evidenziatore non disegna: colora la parola sotto il puntatore, come su un
 * quaderno. Il listener sta sul contenitore in fase di cattura per arrivare
 * prima del canvas, che altrimenti inizierebbe un tratto.
 */
frame.addEventListener(
  "pointerdown",
  (event) => {
    if (event.target !== canvas) return;
    if (ink.getTool() !== "marker" || ink.isErasing()) return;
    if (event.pointerType === "mouse" && event.button !== 0) return;
    if (event.pointerType === "touch" && !event.isPrimary) return;
    event.preventDefault();
    event.stopPropagation();
    if (!page) return;
    const word = wordUnderPoint(
      pageMeasure,
      wordRanges(page.text ?? ""),
      event.clientX,
      event.clientY,
    );
    if (!word) return;
    if (ink.updateHighlights((highlights) => highlightWord(highlights, word, markerColor))) {
      updatePageChrome();
    }
  },
  true,
);

new ResizeObserver(() => syncTextLayout()).observe(frame);

$("undo").addEventListener("click", () => {
  ink.undo();
  updatePageChrome();
});
$("redo").addEventListener("click", () => {
  ink.redo();
  updatePageChrome();
});

document.addEventListener("keydown", (event) => {
  const target = event.target as HTMLElement | null;
  if (target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.isContentEditable)) {
    return;
  }
  const modifier = event.ctrlKey || event.metaKey;
  if (!modifier) return;
  if (event.key.toLowerCase() === "z" && !event.shiftKey) {
    event.preventDefault();
    ink.undo();
    updatePageChrome();
  } else if ((event.key.toLowerCase() === "z" && event.shiftKey) || event.key.toLowerCase() === "y") {
    event.preventDefault();
    ink.redo();
    updatePageChrome();
  }
});

$("sidebar-open").addEventListener("click", () => $("sidebar").classList.add("open"));
$("sidebar-close").addEventListener("click", () => $("sidebar").classList.remove("open"));

$("open-git").addEventListener("click", () => void openGitDialog());
$("git-init").addEventListener("click", () => {
  void (async () => {
    try {
      await api.gitInit(gitName.value.trim(), gitEmail.value.trim());
      appendGitLog("Repository inizializzato su branch main.");
      await refreshGit();
    } catch (error) {
      appendGitLog(describeError(error));
    }
  })();
});

$("git-set-remote").addEventListener("click", () => {
  void (async () => {
    try {
      await api.gitSetRemote(gitRemote.value.trim());
      appendGitLog("Remote HTTPS impostato.");
      await refreshGit();
    } catch (error) {
      appendGitLog(describeError(error));
    }
  })();
});

gitToken.addEventListener("change", () => {
  const token = gitToken.value;
  if (!token) return;
  void (async () => {
    try {
      await api.gitSetToken(token);
      gitToken.value = "";
      appendGitLog("Token in memoria per questa sessione (non scritto su disco).");
    } catch (error) {
      appendGitLog(describeError(error));
    }
  })();
});

$("git-commit").addEventListener("click", () => {
  void (async () => {
    try {
      await flush();
      const report = await api.gitCommit(gitMessage.value.trim() || "appunti");
      appendGitLog(`Commit ${report.short}.`);
      await refreshGit();
    } catch (error) {
      appendGitLog(describeError(error));
      await refreshGit();
    }
  })();
});

$("git-pull").addEventListener("click", () => {
  void (async () => {
    try {
      await flush();
      const report = await api.gitPull();
      const label: Record<string, string> = {
        up_to_date: "Già aggiornato",
        fast_forward: "Fast-forward",
        merged: "Merge completato",
        initial: "Primo allineamento",
      };
      appendGitLog(`${label[report.outcome] ?? report.outcome} (${report.branch} @ ${report.head}).`);
      await refreshGit();
      await refreshNotebooks();
      if (notebook && notebooks.some((item) => item.id === notebook?.id)) {
        await reloadCurrentNotebook();
      }
    } catch (error) {
      appendGitLog(describeError(error));
      await refreshGit();
    }
  })();
});

$("git-push").addEventListener("click", () => {
  void (async () => {
    try {
      await flush();
      const report = await api.gitPush();
      appendGitLog(`Push completato (${report.branch}).`);
      await refreshGit();
    } catch (error) {
      appendGitLog(describeError(error));
      await refreshGit();
    }
  })();
});

document.addEventListener("visibilitychange", () => {
  if (document.visibilityState === "hidden") void flush();
});
window.addEventListener("beforeunload", () => {
  void flush();
});

// Il CSS è già nel documento quando questo modulo viene eseguito (in sviluppo
// lo inietta Vite, in produzione è un `<link>` che blocca il paint): solo ora
// l'interfaccia può comparire, senza il lampo di HTML non formattato.
document.documentElement.classList.add("app-ready");

updatePageChrome();
void start();
