/**
 * Unit test delle funzioni pure dell'evidenziatore.
 * Uso: npm run test:unit   (Node esegue direttamente il TypeScript)
 */
import assert from "node:assert/strict";
import { test } from "node:test";

import { highlightWord, shiftHighlights, wordRanges } from "../src/text/highlights.ts";


const red = "#b91c1c";
const blue = "#1d4ed8";

test("le parole sono intervalli di caratteri non bianchi", () => {
  assert.deepEqual(wordRanges("Ciao sulle righe"), [
    { start: 0, end: 4 },
    { start: 5, end: 10 },
    { start: 11, end: 16 },
  ]);
  assert.deepEqual(wordRanges("  "), []);
  assert.deepEqual(wordRanges("ciao, mondo."), [
    { start: 0, end: 5 },
    { start: 6, end: 12 },
  ]);
});

test("evidenzia una parola nuova, la sostituisce o la toglie", () => {
  const parola = { start: 5, end: 10 };

  const added = highlightWord([], parola, red);
  assert.deepEqual(added, [{ ...parola, color: red }]);

  const recolored = highlightWord(added, parola, blue);
  assert.deepEqual(recolored, [{ ...parola, color: blue }]);

  const removed = highlightWord(recolored, parola, blue);
  assert.deepEqual(removed, []);
});

test("le evidenziazioni restano ordinate per posizione", () => {
  const seconda = { start: 11, end: 16 };
  const prima = { start: 0, end: 4 };
  const result = highlightWord(highlightWord([], seconda, red), prima, red);
  assert.deepEqual(
    result.map((item) => item.start),
    [0, 11],
  );
});

test("scrivere prima di una parola evidenziata ne sposta gli offset", () => {
  const highlights = [{ start: 0, end: 4, color: red }];
  assert.deepEqual(shiftHighlights(highlights, "Ciao", "Ecco Ciao"), [
    { start: 5, end: 9, color: red },
  ]);
});

test("scrivere dopo non cambia gli offset", () => {
  const highlights = [{ start: 0, end: 4, color: red }];
  assert.deepEqual(shiftHighlights(highlights, "Ciao", "Ciao a tutti"), [
    { start: 0, end: 4, color: red },
  ]);
});

test("modificare la parola evidenziata toglie l'evidenziazione", () => {
  const highlights = [{ start: 5, end: 10, color: red }];
  assert.deepEqual(shiftHighlights(highlights, "Ciao sulle righe", "Ciao sopra righe"), []);
});

test("cancellare il testo prima di una parola avvicina gli offset", () => {
  const highlights = [{ start: 10, end: 14, color: red }];
  assert.deepEqual(shiftHighlights(highlights, "1234567890ciao", "10ciao"), [
    { start: 2, end: 6, color: red },
  ]);
});

test("un'emoji inserita prima sposta la parola di due unità UTF-16", () => {
  const highlights = [{ start: 2, end: 6, color: red }];
  // "x ciao" → "x 👍 ciao": l'emoji occupa due unità, più lo spazio.
  assert.deepEqual(shiftHighlights(highlights, "x ciao", "x 👍 ciao"), [
    { start: 5, end: 9, color: red },
  ]);
});
