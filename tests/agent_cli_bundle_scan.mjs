// Search a shipped agent CLI binary for a literal and print what surrounds it.
//
// Both harnesses ship as single compiled binaries, so the only way to answer
// "does this CLI support X over its control protocol?" without guessing is to
// look for the request schema in the bundle. String literals survive the build
// in both, so a plain substring scan finds them; the surrounding bytes are
// printed because the answer is usually the dispatch arm or the schema next to
// the hit, not the hit itself.
//
// This is how the Claude Code `stop_task` control request was found after the
// SDK-facing docs were assumed, wrongly, to be the whole protocol surface.
//
// Usage:
//   node tests/agent_cli_bundle_scan.mjs <binary> <literal> [context-bytes]
//
// Binaries live at:
//   Claude Code  ~/.local/share/claude/versions/<version>
//   Codex        <npm-root>/@openai/codex/node_modules/@openai/codex-<platform>/
//                  vendor/<target>/bin/codex.exe

import { readFileSync } from "node:fs";

const [file, needle, win = "260"] = process.argv.slice(2);
if (!file || !needle) {
  console.log(
    "usage: node tests/agent_cli_bundle_scan.mjs <binary> <literal> [context-bytes]",
  );
  process.exit(2);
}

// latin1 maps every byte to one character, so offsets stay byte-accurate and no
// byte sequence is lost to invalid-UTF-8 replacement.
const buf = readFileSync(file).toString("latin1");
const w = Number(win);

let i = -1;
let hits = 0;
while ((i = buf.indexOf(needle, i + 1)) !== -1 && hits < 12) {
  hits++;
  const slice = buf
    .slice(Math.max(0, i - w), i + needle.length + w)
    // Compiled code around a literal is mostly non-printable; collapsing it
    // keeps the readable identifiers on one legible line.
    .replace(/[^\x20-\x7e]/g, "·");
  console.log(`\n--- hit ${hits} @ ${i}\n${slice}`);
}
if (!hits) console.log("no hits");
