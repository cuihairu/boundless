/**
 * Publishing EntityDef checks as editor diagnostics.
 *
 * The rules live in `def/checker.ts`, which mirrors the reference
 * `process_defs` output (`data_description.cpp`, `entity_description.cpp`,
 * `method_description.cpp`). This module only translates absolute offsets in
 * the source text into line/column ranges and lets the adapter feed real
 * files in.
 */

import { XmlParseError } from "../datasection/xmlSection";
import { EntityDef, parseEntityDef } from "../def/defFile";
import { Severity, checkEntityDef } from "../def/checker";
import { AliasRegistry } from "../def/registry";

/** A half-open line/column range, the shape editors want. */
export interface TextRange {
  startLine: number;
  startCharacter: number;
  endLine: number;
  endCharacter: number;
}

/** A checker diagnostic anchored in the document text. */
export interface TextDiagnostic {
  severity: Severity;
  message: string;
  rule: string;
  range: TextRange;
}

/** Which role scripts exist, resolved the way the reference decides
 * component availability (`HasScriptOrTagDistributionDecider`). */
export interface ScriptProbe {
  hasScript(role: "base" | "cell" | "client" | "service"): boolean;
}

/** Start offset of every line in `text`. */
export function lineStarts(text: string): number[] {
  const starts = [0];
  for (let i = 0; i < text.length; i++) {
    if (text[i] === "\n") starts.push(i + 1);
  }
  return starts;
}

/** The line/column range covering `offset` .. `offset + length`. */
export function rangeAt(text: string, offset: number, length: number): TextRange {
  const starts = lineStarts(text);
  const lineOf = (at: number): { line: number; character: number } => {
    // Binary search for the last line start at or before `at`.
    let lo = 0;
    let hi = starts.length - 1;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if (starts[mid] <= at) lo = mid;
      else hi = mid - 1;
    }
    return { line: lo, character: at - starts[lo] };
  };
  const start = lineOf(Math.max(0, Math.min(offset, text.length)));
  const endAt = Math.max(offset, offset + length);
  const end = lineOf(Math.min(endAt, text.length));
  return {
    startLine: start.line,
    startCharacter: start.character,
    endLine: end.line,
    endCharacter: end.character,
  };
}

/** The whole document, for errors that cannot be anchored anywhere else. */
function wholeFile(text: string): TextRange {
  const lastLine = text.split("\n").length - 1;
  return {
    startLine: 0,
    startCharacter: 0,
    endLine: lastLine,
    endCharacter: 0,
  };
}

/** Checks one `.def` document: parse errors plus every checker rule. */
export function checkDefText(
  aliases: AliasRegistry,
  text: string,
  defName: string,
  isService = false,
  scripts?: ScriptProbe,
): TextDiagnostic[] {
  try {
    const def: EntityDef = parseEntityDef(text, defName, isService);
    return checkEntityDef(def, {
      aliases,
      ...(scripts ? { hasScript: (role) => scripts.hasScript(role) } : {}),
    }).map((d) => ({
      severity: d.severity,
      message: d.message,
      rule: d.rule,
      range: rangeAt(text, d.offset, d.length),
    }));
  } catch (e) {
    if (e instanceof XmlParseError) {
      return [
        {
          severity: "error",
          message: `XML parse error: ${e.message}`,
          rule: "xml-syntax",
          range: wholeFile(text),
        },
      ];
    }
    throw e;
  }
}
