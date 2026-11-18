import { invoke } from "@tauri-apps/api/core";

export type Paper = "plain" | "ruled" | "squared" | "dotted";
export type ToolKind = "pen" | "marker";

/** Punti piatti `x, y, pressione` in unità di pagina (1000 × 1414). */
export interface Stroke {
  tool: ToolKind;
  color: string;
  width: number;
  points: number[];
}

/** Evidenziazione di una parola del testo: offset UTF-16, come in JavaScript. */
export interface Highlight {
  start: number;
  end: number;
  color: string;
}

export interface Page {
  schema: number;
  id: string;
  paper: Paper;
  text?: string;
  strokes: Stroke[];
  /** Assente nei file scritti prima dell'evidenziatore. */
  highlights?: Highlight[];
  updated_at: number;
}

export type UiTool = "pen" | "marker" | "eraser" | "text";

/** Strumento e colori dell'ultimo uso: salvati nel quaderno, viaggiano con lui. */
export interface NotebookUi {
  tool: UiTool;
  pen_color: string;
  marker_color: string;
}

export interface Notebook {
  schema: number;
  id: string;
  title: string;
  pages: string[];
  updated_at: number;
  /** Assente nei quaderni mai aperti da una versione che lo salva. */
  ui?: NotebookUi;
}

export interface NotebookSummary {
  id: string;
  title: string;
  pages: number;
  updated_at: number;
}

export interface VaultView {
  path: string;
  notebooks: NotebookSummary[];
}

export interface GitStatus {
  initialized: boolean;
  branch: string | null;
  remote: string | null;
  dirty: boolean;
  ahead: number;
  behind: number;
  files: number;
  head: string | null;
}

export interface CommitReport {
  oid: string;
  short: string;
}

export interface PullReport {
  outcome: "up_to_date" | "fast_forward" | "merged" | "initial";
  branch: string;
  head: string;
}

export interface PushReport {
  branch: string;
}

export class ApiError extends Error {
  readonly code: string;

  constructor(code: string, message: string) {
    super(message);
    this.name = "ApiError";
    this.code = code;
  }
}

function normalizeError(error: unknown): ApiError {
  if (typeof error === "object" && error !== null && "message" in error) {
    const candidate = error as { message?: unknown; code?: unknown };
    return new ApiError(
      typeof candidate.code === "string" ? candidate.code : "unknown",
      typeof candidate.message === "string" ? candidate.message : "Errore sconosciuto",
    );
  }
  return new ApiError("unknown", String(error));
}

async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(command, args);
  } catch (error) {
    throw normalizeError(error);
  }
}

export const api = {
  vaultOpen: () => call<VaultView>("vault_open"),
  vaultPick: () => call<string | null>("vault_pick"),
  vaultUse: (path: string, create: boolean) =>
    call<VaultView>("vault_use", { path, create }),
  notebookCreate: (title: string) => call<Notebook>("notebook_create", { title }),
  notebookLoad: (notebookId: string) => call<Notebook>("notebook_load", { notebookId }),
  notebookDelete: (notebookId: string) => call<void>("notebook_delete", { notebookId }),
  notebookSetUi: (notebookId: string, tool: UiTool, penColor: string, markerColor: string) =>
    call<Notebook>("notebook_set_ui", { notebookId, tool, penColor, markerColor }),
  pageCreate: (notebookId: string, paper: Paper) =>
    call<Page>("page_create", { notebookId, paper }),
  pageLoad: (notebookId: string, pageId: string) =>
    call<Page>("page_load", { notebookId, pageId }),
  pageSave: (notebookId: string, page: Page) => call<Page>("page_save", { notebookId, page }),
  pageDelete: (notebookId: string, pageId: string) =>
    call<void>("page_delete", { notebookId, pageId }),

  gitStatus: () => call<GitStatus>("git_status"),
  gitInit: (name: string, email: string) => call<void>("git_init", { name, email }),
  gitSetRemote: (url: string) => call<void>("git_set_remote", { url }),
  gitCommit: (message: string) => call<CommitReport>("git_commit", { message }),
  gitPull: () => call<PullReport>("git_pull"),
  gitPush: () => call<PushReport>("git_push"),
  gitSetToken: (token: string) => call<void>("git_set_token", { token }),
  gitClearToken: () => call<void>("git_clear_token"),
};
