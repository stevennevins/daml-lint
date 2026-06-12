// Compiled from no-trace.ts — this is the file you pass to --rules.

const NAME = "no-trace";
const SEVERITY = "low";
const DESCRIPTION = "Debug trace left in code";

function check(m) {
  m.source.split("\n").forEach((line, idx) => {
    if (line.includes("trace")) {
      report(idx + 1, "Remove debug trace calls before deploying");
    }
  });
}
