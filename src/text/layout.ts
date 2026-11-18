import type { Highlight } from "../api";
import type { WordRange } from "./highlights";

export interface HighlightRect {
  left: number;
  top: number;
  width: number;
  height: number;
  color: string;
}

/**
 * Rettangoli (in pixel CSS, relativi al bordo del foglio) da colorare dietro
 * alle parole evidenziate. Il testo di riferimento vive in un livello
 * invisibile con la stessa metrica del textarea: una parola spezzata su due
 * righe produce due rettangoli.
 */
export function highlightRects(
  measure: HTMLElement,
  highlights: readonly Highlight[],
): HighlightRect[] {
  const node = textNodeOf(measure);
  const length = node?.textContent?.length ?? 0;
  if (!node || length === 0) return [];

  const origin = measure.getBoundingClientRect();
  const range = document.createRange();
  const rects: HighlightRect[] = [];
  for (const highlight of highlights) {
    if (highlight.start < 0 || highlight.start >= highlight.end || highlight.end > length) continue;
    range.setStart(node, highlight.start);
    range.setEnd(node, highlight.end);
    for (const rect of Array.from(range.getClientRects())) {
      if (rect.width <= 0 || rect.height <= 0) continue;
      rects.push({
        left: rect.left - origin.left,
        top: rect.top - origin.top,
        width: rect.width,
        height: rect.height,
        color: highlight.color,
      });
    }
  }
  return rects;
}

/** Parola sotto il puntatore (coordinate viewport), o `null`. */
export function wordUnderPoint(
  measure: HTMLElement,
  words: readonly WordRange[],
  clientX: number,
  clientY: number,
): WordRange | null {
  const node = textNodeOf(measure);
  if (!node) return null;
  const range = document.createRange();
  for (const word of words) {
    range.setStart(node, word.start);
    range.setEnd(node, word.end);
    for (const rect of Array.from(range.getClientRects())) {
      // Il rettangolo di un intervallo copre l'intera riga: basta centrare il
      // tocco in orizzontale, così anche le lettere strette sono facili.
      if (clientY < rect.top || clientY > rect.bottom) continue;
      if (clientX >= rect.left - 2 && clientX <= rect.right + 2) return word;
    }
  }
  return null;
}

function textNodeOf(measure: HTMLElement): Text | null {
  const node = measure.firstChild;
  return node && node.nodeType === Node.TEXT_NODE ? (node as Text) : null;
}
