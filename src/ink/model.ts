import type { Stroke } from "../api";

export const PAGE_WIDTH = 1000;
export const PAGE_HEIGHT = 1414;

export interface Point {
  x: number;
  y: number;
  pressure: number;
}

export function smoothPressure(stroke: Stroke, index: number): number {
  const raw = stroke.points[index * 3 + 2] ?? 0.5;
  return Math.min(1, Math.max(0, raw));
}

export function strokeWidthAt(stroke: Stroke, index: number): number {
  // La pressione modula lo spessore ma non lo azzera: una penna che riporta
  // 0 per un tratto veloce deve restare visibile.
  const pressure = smoothPressure(stroke, index);
  return stroke.width * (0.45 + pressure * 0.95);
}

export function pointAt(stroke: Stroke, index: number): Point {
  return {
    x: stroke.points[index * 3] ?? 0,
    y: stroke.points[index * 3 + 1] ?? 0,
    pressure: smoothPressure(stroke, index),
  };
}

export function pointCount(stroke: Stroke): number {
  return Math.floor(stroke.points.length / 3);
}

/** Distanza punto-segmento in unità di pagina, per la gomma. */
export function distanceToStroke(stroke: Stroke, x: number, y: number): number {
  const count = pointCount(stroke);
  if (count === 0) return Number.POSITIVE_INFINITY;
  if (count === 1) {
    const p = pointAt(stroke, 0);
    return Math.hypot(p.x - x, p.y - y);
  }
  let best = Number.POSITIVE_INFINITY;
  for (let i = 1; i < count; i += 1) {
    const a = pointAt(stroke, i - 1);
    const b = pointAt(stroke, i);
    best = Math.min(best, distancePointSegment(x, y, a.x, a.y, b.x, b.y));
  }
  return best;
}

function distancePointSegment(
  px: number,
  py: number,
  ax: number,
  ay: number,
  bx: number,
  by: number,
): number {
  const dx = bx - ax;
  const dy = by - ay;
  const lengthSq = dx * dx + dy * dy;
  if (lengthSq === 0) return Math.hypot(px - ax, py - ay);
  const t = Math.min(1, Math.max(0, ((px - ax) * dx + (py - ay) * dy) / lengthSq));
  return Math.hypot(px - (ax + t * dx), py - (ay + t * dy));
}

export function clampToPage(x: number, y: number): { x: number; y: number } {
  return {
    x: Math.min(PAGE_WIDTH, Math.max(0, x)),
    y: Math.min(PAGE_HEIGHT, Math.max(0, y)),
  };
}
