import type { Highlight } from "../api";

export interface WordRange {
  start: number;
  end: number;
}

/**
 * Intervalli `[start, end)` delle parole: sequenze di caratteri non bianchi.
 * La punteggiatura attaccata alla parola resta dentro l'intervallo, come su
 * carta quando si passa l'evidenziatore su "parola.".
 */
export function wordRanges(text: string): WordRange[] {
  const ranges: WordRange[] = [];
  const matcher = /\S+/gu;
  for (let match = matcher.exec(text); match !== null; match = matcher.exec(text)) {
    ranges.push({ start: match.index, end: match.index + match[0].length });
  }
  return ranges;
}

/**
 * Aggiunge, sostituisce o toglie (se il colore è lo stesso) l'evidenziazione
 * di una parola. L'elenco resta ordinato per posizione: il JSON su disco
 * cambia solo nelle righe interessate.
 */
export function highlightWord(
  highlights: readonly Highlight[],
  range: WordRange,
  color: string,
): Highlight[] {
  const index = highlights.findIndex(
    (item) => item.start === range.start && item.end === range.end,
  );
  const existing = index >= 0 ? highlights[index] : undefined;
  if (!existing) {
    const next = [...highlights, { start: range.start, end: range.end, color }];
    next.sort((a, b) => a.start - b.start || a.end - b.end);
    return next;
  }
  if (existing.color === color) {
    return highlights.filter((_, position) => position !== index);
  }
  return highlights.map((item, position) => (position === index ? { ...item, color } : item));
}

/**
 * Sposta le evidenziazioni quando il testo cambia.
 *
 * Confronta vecchio e nuovo testo (prefisso e suffisso comuni) e applica il
 * delta alle evidenziazioni che stanno dopo la modifica; quelle che toccano la
 * parte modificata vengono scartate, invece di finire su parole sbagliate o
 * fuori dal testo (cosa che il core rifiuterebbe al salvataggio).
 */
export function shiftHighlights(
  highlights: readonly Highlight[],
  before: string,
  after: string,
): Highlight[] {
  if (before === after) return highlights.map((item) => ({ ...item }));
  let prefix = 0;
  const shortest = Math.min(before.length, after.length);
  while (prefix < shortest && before[prefix] === after[prefix]) prefix += 1;
  let suffix = 0;
  while (
    suffix < shortest - prefix &&
    before[before.length - 1 - suffix] === after[after.length - 1 - suffix]
  ) {
    suffix += 1;
  }
  // Non tagliare in mezzo a una coppia surrogata: gli offset restano validi.
  if (prefix > 0 && isHighSurrogate(before.codePointAt(prefix - 1) ?? 0)) prefix -= 1;
  if (suffix > 0 && isHighSurrogate(before.codePointAt(before.length - suffix - 1) ?? 0)) {
    suffix -= 1;
  }

  const delta = after.length - before.length;
  const changedFrom = before.length - suffix;
  const result: Highlight[] = [];
  for (const item of highlights) {
    if (item.end <= prefix) {
      result.push({ ...item });
    } else if (item.start >= changedFrom) {
      result.push({ ...item, start: item.start + delta, end: item.end + delta });
    }
  }
  return result.filter((item) => item.start >= 0 && item.start < item.end && item.end <= after.length);
}

function isHighSurrogate(code: number): boolean {
  return code >= 0xd800 && code <= 0xdbff;
}
