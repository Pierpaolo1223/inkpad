import type { Highlight, Page, Stroke, ToolKind } from "../api";
import {
  PAGE_HEIGHT,
  PAGE_WIDTH,
  clampToPage,
  distanceToStroke,
  pointAt,
  pointCount,
  strokeWidthAt,
} from "./model";

const MAX_UNDO = 60;

interface PageState {
  strokes: Stroke[];
  highlights: Highlight[];
}

function sameHighlights(a: readonly Highlight[], b: readonly Highlight[]): boolean {
  return (
    a.length === b.length &&
    a.every(
      (item, index) =>
        item.start === b[index]?.start &&
        item.end === b[index]?.end &&
        item.color === b[index]?.color,
    )
  );
}
const PALM_WINDOW_MS = 1200;
const PALM_CONTACT_PX = 28;

/**
 * Superficie a inchiostro.
 *
 * Geometria: le coordinate salvate sono in unità di pagina (1000 × 1414),
 * indipendenti dal device pixel ratio e dalla dimensione dello schermo. Il
 * canvas è ridisegnato a ogni resize e scalato con una sola trasformazione.
 *
 * Input: eventi pointer con coalescing (i WebView moderni consegnano più
 * campioni per frame), pressione quando disponibile, rifiuto del palmo
 * (touch subito dopo la penna o con contatto largo) e gomma a tratto intero.
 */
export class InkSurface {
  private readonly canvas: HTMLCanvasElement;
  private readonly ctx: CanvasRenderingContext2D;
  private readonly onChange: () => void;
  private readonly observer: ResizeObserver;

  private page: Page | null = null;
  private scale = 1;
  private dpr = 1;

  private active: Stroke | null = null;
  private activePointer: number | null = null;
  private gestureErasing = false;
  private drawn = 0;
  private lastPenAt = 0;

  private tool: ToolKind = "pen";
  private color = "#1f2933";
  private width = 3;
  private erasing = false;

  private undoStack: PageState[] = [];
  private redoStack: PageState[] = [];

  constructor(canvas: HTMLCanvasElement, onChange: () => void) {
    this.canvas = canvas;
    const ctx = canvas.getContext("2d");
    if (!ctx) throw new Error("Canvas 2D non disponibile");
    this.ctx = ctx;
    this.onChange = onChange;

    canvas.addEventListener("pointerdown", this.handlePointerDown);
    canvas.addEventListener("pointermove", this.handlePointerMove);
    canvas.addEventListener("pointerup", this.handlePointerUp);
    canvas.addEventListener("pointercancel", this.handlePointerUp);
    canvas.addEventListener("lostpointercapture", this.handlePointerUp);
    canvas.addEventListener("contextmenu", (event) => event.preventDefault());

    this.observer = new ResizeObserver(() => this.resize());
    this.observer.observe(canvas);
    this.resize();
  }

  destroy(): void {
    this.observer.disconnect();
    this.canvas.removeEventListener("pointerdown", this.handlePointerDown);
    this.canvas.removeEventListener("pointermove", this.handlePointerMove);
    this.canvas.removeEventListener("pointerup", this.handlePointerUp);
    this.canvas.removeEventListener("pointercancel", this.handlePointerUp);
    this.canvas.removeEventListener("lostpointercapture", this.handlePointerUp);
  }

  // --- stato -------------------------------------------------------------

  setPage(page: Page | null): void {
    this.active = null;
    this.activePointer = null;
    this.undoStack = [];
    this.redoStack = [];
    this.page = page;
    this.resize();
  }

  getPage(): Page | null {
    return this.page;
  }

  setTool(tool: ToolKind): void {
    this.tool = tool;
    this.erasing = false;
  }

  setColor(color: string): void {
    this.color = color;
    this.erasing = false;
  }

  setWidth(width: number): void {
    this.width = width;
  }

  setEraser(enabled: boolean): void {
    this.erasing = enabled;
  }

  getTool(): ToolKind {
    return this.tool;
  }

  isErasing(): boolean {
    return this.erasing;
  }

  /**
   * Cambia le evidenziazioni con la stessa cronologia dei tratti: annulla e
   * ripeti coprono inchiostro e parole evidenziate insieme.
   */
  updateHighlights(mutate: (highlights: Highlight[]) => Highlight[]): boolean {
    if (!this.page) return false;
    const current = this.page.highlights ?? [];
    const next = mutate(current.slice());
    if (sameHighlights(current, next)) return false;
    this.snapshot();
    this.page.highlights = next;
    this.onChange();
    return true;
  }

  setPaper(paper: Page["paper"]): void {
    if (!this.page) return;
    this.page.paper = paper;
    this.redraw();
  }

  canUndo(): boolean {
    return this.undoStack.length > 0;
  }

  canRedo(): boolean {
    return this.redoStack.length > 0;
  }

  undo(): boolean {
    if (!this.page) return false;
    const previous = this.undoStack.pop();
    if (!previous) return false;
    this.redoStack.push(this.stateOf(this.page));
    this.applyState(previous);
    this.redraw();
    this.onChange();
    return true;
  }

  redo(): boolean {
    if (!this.page) return false;
    const next = this.redoStack.pop();
    if (!next) return false;
    this.undoStack.push(this.stateOf(this.page));
    this.applyState(next);
    this.redraw();
    this.onChange();
    return true;
  }

  clear(): boolean {
    if (!this.page || this.page.strokes.length === 0) return false;
    this.snapshot();
    this.page.strokes = [];
    this.redraw();
    this.onChange();
    return true;
  }

  // --- rendering ---------------------------------------------------------

  private resize(): void {
    const cssWidth = this.canvas.clientWidth || 600;
    this.scale = cssWidth / PAGE_WIDTH;
    this.dpr = Math.min(window.devicePixelRatio || 1, 3);

    const pixelWidth = Math.max(1, Math.round(cssWidth * this.dpr));
    const pixelHeight = Math.max(1, Math.round(PAGE_HEIGHT * this.scale * this.dpr));
    if (this.canvas.width !== pixelWidth) this.canvas.width = pixelWidth;
    if (this.canvas.height !== pixelHeight) this.canvas.height = pixelHeight;
    this.canvas.style.height = `${Math.round(PAGE_HEIGHT * this.scale)}px`;

    this.ctx.setTransform(this.dpr * this.scale, 0, 0, this.dpr * this.scale, 0, 0);
    this.redraw();
  }

  private redraw(): void {
    const { ctx, canvas } = this;
    ctx.save();
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.clearRect(0, 0, canvas.width, canvas.height);
    ctx.restore();
    for (const stroke of this.page?.strokes ?? []) this.drawStroke(stroke);
  }

  private drawStroke(stroke: Stroke): void {
    const count = pointCount(stroke);
    if (count === 0) return;
    if (count === 1) {
      this.drawDot(stroke);
      return;
    }
    for (let i = 1; i < count; i += 1) this.drawSegment(stroke, i);
  }

  private drawDot(stroke: Stroke): void {
    const point = pointAt(stroke, 0);
    const ctx = this.ctx;
    ctx.save();
    ctx.globalAlpha = stroke.tool === "marker" ? 0.32 : 1;
    ctx.fillStyle = stroke.color;
    ctx.beginPath();
    ctx.arc(point.x, point.y, strokeWidthAt(stroke, 0) / 2, 0, Math.PI * 2);
    ctx.fill();
    ctx.restore();
  }

  private drawSegment(stroke: Stroke, index: number): void {
    const a = pointAt(stroke, index - 1);
    const b = pointAt(stroke, index);
    const width = (strokeWidthAt(stroke, index - 1) + strokeWidthAt(stroke, index)) / 2;
    const ctx = this.ctx;
    ctx.save();
    ctx.globalAlpha = stroke.tool === "marker" ? 0.32 : 1;
    ctx.strokeStyle = stroke.color;
    ctx.lineWidth = width;
    ctx.lineCap = "round";
    ctx.lineJoin = "round";
    ctx.beginPath();
    ctx.moveTo(a.x, a.y);
    ctx.lineTo(b.x, b.y);
    ctx.stroke();
    ctx.restore();
  }

  // --- input -------------------------------------------------------------

  private toPage(event: PointerEvent | MouseEvent): { x: number; y: number } {
    const rect = this.canvas.getBoundingClientRect();
    const x = ((event.clientX - rect.left) / Math.max(1, rect.width)) * PAGE_WIDTH;
    const y = ((event.clientY - rect.top) / Math.max(1, rect.height)) * PAGE_HEIGHT;
    return clampToPage(x, y);
  }

  private pressureOf(event: PointerEvent): number {
    if (event.pointerType === "mouse") return 0.5;
    if (!Number.isFinite(event.pressure) || event.pressure <= 0) return 0.5;
    return Math.min(1, event.pressure);
  }

  /** Tocchi che non devono disegnare: palmo o dito subito dopo la penna. */
  private isPalm(event: PointerEvent): boolean {
    const now = performance.now();
    if (event.pointerType === "pen") {
      this.lastPenAt = now;
      return false;
    }
    if (event.pointerType !== "touch") return false;
    if (now - this.lastPenAt < PALM_WINDOW_MS) return true;
    return event.width > PALM_CONTACT_PX || event.height > PALM_CONTACT_PX;
  }

  private stateOf(page: Page): PageState {
    return {
      strokes: page.strokes.slice(),
      highlights: (page.highlights ?? []).slice(),
    };
  }

  private applyState(state: PageState): void {
    if (!this.page) return;
    this.page.strokes = state.strokes.slice();
    this.page.highlights = state.highlights.slice();
  }

  private snapshot(): void {
    if (!this.page) return;
    this.undoStack.push(this.stateOf(this.page));
    if (this.undoStack.length > MAX_UNDO) this.undoStack.shift();
    this.redoStack = [];
  }

  private handlePointerDown = (event: PointerEvent): void => {
    if (!this.page || this.activePointer !== null) return;
    if (event.pointerType === "mouse" && event.button !== 0) return;
    if (event.pointerType === "touch" && !event.isPrimary) return;
    if (this.isPalm(event)) return;
    event.preventDefault();

    this.activePointer = event.pointerId;
    try {
      this.canvas.setPointerCapture(event.pointerId);
    } catch {
      // Alcuni WebView rifiutano la cattura dopo `preventDefault`: il tratto
      // funziona lo stesso finché il puntatore resta sopra il canvas.
    }
    this.gestureErasing = this.erasing || (event.buttons & 32) === 32;

    const point = this.toPage(event);
    if (this.gestureErasing) {
      this.snapshot();
      this.eraseAt(point.x, point.y);
      return;
    }

    this.snapshot();
    this.active = {
      tool: this.tool,
      color: this.color,
      width: this.width,
      points: [point.x, point.y, this.pressureOf(event)],
    };
    this.drawn = 0;
    this.drawDot(this.active);
  };

  private handlePointerMove = (event: PointerEvent): void => {
    if (!this.page || event.pointerId !== this.activePointer) return;
    event.preventDefault();

    if (this.gestureErasing) {
      const point = this.toPage(event);
      this.eraseAt(point.x, point.y);
      return;
    }

    const stroke = this.active;
    if (!stroke) return;

    const samples = this.samplesOf(event);
    for (const sample of samples) {
      const point = this.toPage(sample);
      stroke.points.push(point.x, point.y, this.pressureOf(sample));
    }

    const count = pointCount(stroke);
    for (let i = Math.max(1, this.drawn); i < count; i += 1) {
      this.drawSegment(stroke, i);
    }
    this.drawn = count;
  };

  private handlePointerUp = (event: PointerEvent): void => {
    if (event.pointerId !== this.activePointer) return;
    this.activePointer = null;
    this.gestureErasing = false;

    const stroke = this.active;
    this.active = null;
    this.drawn = 0;
    if (!stroke || !this.page) return;
    this.page.strokes.push(stroke);
    this.onChange();
  };

  /**
   * I WebView che supportano `getCoalescedEvents` consegnano tutti i campioni
   * della penna accumulati fra due frame: usarli è la differenza fra una curva
   * e una spezzata.
   */
  private samplesOf(event: PointerEvent): PointerEvent[] {
    if (typeof event.getCoalescedEvents !== "function") return [event];
    const samples = event.getCoalescedEvents();
    return samples.length > 0 ? samples : [event];
  }

  private eraseAt(x: number, y: number): void {
    if (!this.page) return;
    const radius = Math.max(8, this.width * 4);
    const before = this.page.strokes.length;
    this.page.strokes = this.page.strokes.filter(
      (stroke) => distanceToStroke(stroke, x, y) > radius,
    );
    if (this.page.strokes.length !== before) {
      this.redraw();
      this.onChange();
    }
  }
}
