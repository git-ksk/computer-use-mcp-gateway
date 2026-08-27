#!/usr/bin/env node
// Compatibility/regression wrapper. The first-class CUMG runtime is v2_handoff_runtime.mjs.
export * from "./v2_handoff_runtime.mjs";
import { pathToFileURL } from "node:url";
import { runCli } from "./v2_handoff_runtime.mjs";

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  runCli().catch((error) => {
    process.stderr.write(`operator handoff bridge refused: ${error.message}\n`);
    process.exitCode = 2;
  });
}
