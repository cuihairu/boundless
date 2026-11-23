/**
 * Tokenizes `.def` text with the real VS Code TextMate engine
 * (`vscode-textmate` driven by the `onig.wasm` scanner the editor itself ships)
 * and the grammar published in `syntaxes/`.
 *
 * Running the actual engine is what makes a highlight "checkable": the
 * assertions below compare the scope names the editor would apply, not a
 * re-implementation of the grammar.
 */

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { createRequire } from "node:module";

import * as oniguruma from "vscode-oniguruma";
import * as vsctm from "vscode-textmate";
import type { IRawGrammar } from "vscode-textmate/release/rawGrammar";

/** One token the grammar produced. */
export interface Token {
  /** Start offset in the line, inclusive. */
  start: number;
  /** End offset in the line, exclusive. */
  end: number;
  /** The full scope stack, outermost first. */
  scopes: string[];
}

const GRAMMAR_SCOPE = "source.entities";
const here = dirname(fileURLToPath(import.meta.url));
const GRAMMAR_PATH = join(here, "..", "syntaxes", "entities.tmLanguage.json");

const nodeRequire = createRequire(import.meta.url);
const WASM_PATH =
  dirname(nodeRequire.resolve("vscode-oniguruma/package.json")) +
  "/release/onig.wasm";

const MAX_PROGRAM_LENGTH = 100000;

let grammar: vsctm.IGrammar | null = null;

/** Loads `onig.wasm` and the grammar once per test file. */
export async function loadGrammar(): Promise<void> {
  if (grammar) return;

  const wasm = new Uint8Array(readFileSync(WASM_PATH));
  // `loadWASM` installs the scanner into the oniguruma module and resolves to
  // nothing, so the library to hand the registry is the module itself.
  const onigLib = oniguruma.loadWASM({ data: wasm }).then(() => oniguruma);

  const registry = new vsctm.Registry({
    onigLib,
    loadGrammar: async (scopeName) =>
      scopeName === GRAMMAR_SCOPE
        ? (JSON.parse(readFileSync(GRAMMAR_PATH, "utf8")) as IRawGrammar)
        : null,
  });

  const loaded = await registry.loadGrammar(GRAMMAR_SCOPE);
  if (!loaded) throw new Error(`no grammar for ${GRAMMAR_SCOPE}`);
  grammar = loaded;
}

function active(): vsctm.IGrammar {
  if (!grammar) throw new Error("loadGrammar() must be awaited first");
  return grammar;
}

/** Tokenizes one line from the top of the document. */
export function tokenizeLine(line: string): Token[] {
  return toTokens(active().tokenizeLine(line, vsctm.INITIAL, MAX_PROGRAM_LENGTH));
}

/** Tokenizes a whole document, carrying the rule stack from line to line.
 * Needed to prove that a `<Type>…</Type>` opened on one line still classifies a
 * payload seen on a later line. */
export function tokenizeDocument(text: string): Token[][] {
  const grammar = active();
  const lines = text.split("\n");
  const out: Token[][] = [];
  let state: vsctm.StateStack | null = vsctm.INITIAL;
  for (const line of lines) {
    const result = grammar.tokenizeLine(line, state, MAX_PROGRAM_LENGTH);
    out.push(toTokens(result));
    state = result.ruleStack;
  }
  return out;
}

function toTokens(result: vsctm.ITokenizeLineResult): Token[] {
  return result.tokens.map((t) => ({
    start: t.startIndex,
    end: t.endIndex,
    scopes: [...t.scopes],
  }));
}

/** The scope stack of the token covering `offset` in `line`. */
export function scopesAt(line: string, offset: number): string[] {
  const token = tokenizeLine(line).find(
    (t) => t.start <= offset && offset < t.end,
  );
  return token?.scopes ?? [];
}

/** True when some token of `line` carries `scope`. */
export function hasScope(line: string, scope: string): boolean {
  return tokenizeLine(line).some((t) => t.scopes.includes(scope));
}
