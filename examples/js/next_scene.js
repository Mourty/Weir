#!/usr/bin/env node
// Load the next saved scene, in alphabetical order, wrapping around: one
// button that steps through your scenes.
//
//   ./next_scene.js

"use strict";

const weir = require("./weir");

async function main() {
  const mixer = await weir.connect();
  try {
    const { library } = await mixer.call("get_state");
    if (library.scenes.length === 0) {
      console.log("No scenes saved yet. Save one from the Scenes menu first.");
      return;
    }
    const scenes = [...library.scenes].sort();
    const next = scenes[(scenes.indexOf(library.scene) + 1) % scenes.length];
    await mixer.call("load_scene", { name: next });
    console.log(`Loaded ${next}`);
  } finally {
    mixer.close();
  }
}

main().catch((e) => {
  console.error(`weir: ${e.message}`);
  process.exit(1);
});
