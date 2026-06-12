// Flag leftover debug trace calls — a simple banned-token rule written
// as a whole-module check over the raw source.
// Compile: npx esbuild no-trace.ts --outfile=no-trace.js

const NAME = "no-trace";
const SEVERITY = "low";
const DESCRIPTION = "Debug trace left in code";

function check(m: DamlModule): void {
  m.source.split("\n").forEach((line, idx) => {
    if (line.includes("trace")) {
      report(idx + 1, "Remove debug trace calls before deploying");
    }
  });
}
