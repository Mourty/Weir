#!/usr/bin/env node
// A live level meter for a strip, in the terminal. Ctrl+C stops it.
//
//   ./level_meter.js Music

"use strict";

const weir = require("./weir");

const FLOOR_DB = -60;
const WIDTH = 50;

async function main() {
  const name = process.argv[2];
  if (!name) {
    console.error("usage: level_meter.js STRIP");
    process.exit(2);
  }
  const mixer = await weir.connect();
  const state = await mixer.call("get_state");
  const strip = weir.find(state.mixer.strips, name);
  if (!strip) throw new Error(`no strip called '${name}'`);

  mixer.on("meters", (meters) => {
    const channels = meters.strips[strip.id] ?? [];
    const peak = Math.max(FLOOR_DB, ...channels);
    const filled = Math.round((WIDTH * (peak - FLOOR_DB)) / -FLOOR_DB);
    const bar = "#".repeat(filled) + ".".repeat(WIDTH - filled);
    process.stdout.write(`\r${strip.name.padStart(12)} [${bar}] ${peak.toFixed(1).padStart(6)} dB`);
  });
  // Ten a second is plenty for a terminal; the peaks in between are kept.
  await mixer.subscribe(["meters"], 10);
}

main().catch((e) => {
  console.error(`weir: ${e.message}`);
  process.exit(1);
});
