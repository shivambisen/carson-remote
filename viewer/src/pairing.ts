/** Pairing-code helpers. Codes look like "BCDF-GHJK" (RFC 8628-style consonant alphabet). */

const ALPHABET = /[BCDFGHJKLMNPQRSTVWXZ]/;

/** Upper-cases and keeps only valid code letters. */
export function normalizeCode(input: string): string {
  return [...input.toUpperCase()].filter((c) => ALPHABET.test(c)).join("").slice(0, 8);
}

/** Formats as the user types: "bcdfg" → "BCDF-G". */
export function formatCode(input: string): string {
  const n = normalizeCode(input);
  return n.length > 4 ? `${n.slice(0, 4)}-${n.slice(4)}` : n;
}

export function isCompleteCode(input: string): boolean {
  return normalizeCode(input).length === 8;
}
