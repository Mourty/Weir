#!/usr/bin/env node
// Mute or unmute a strip, and say which it is now.
//
//   ./toggle_mute.js Mic

"use strict";

const weir = require("./weir");

async function main() {
  const name = process.argv[2];
  if (!name) {
    console.error("usage: toggle_mute.js STRIP");
    process.exit(2);
  }
  const mixer = await weir.connect();
  try {
    const strip = await mixer.call("set_strip", { id: name, mute: "toggle" });
    console.log(`${strip.name} is ${strip.mute ? "muted" : "live"}`);
  } finally {
    mixer.close();
  }
}

main().catch((e) => {
  console.error(`weir: ${e.message}`);
  process.exit(1);
});
