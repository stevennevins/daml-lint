// Nonconsuming choices that create contracts can be exercised repeatedly on
// the same contract, fanning out unbounded copies. Walks choice body
// statements, recursing into try/catch blocks.
// Compile: npx esbuild no-create-in-nonconsuming.ts --outfile=no-create-in-nonconsuming.js

const NAME = "no-create-in-nonconsuming";
const SEVERITY = "medium";
const DESCRIPTION = "Nonconsuming choices should not create contracts";

function creates(stmts: Statement[]): boolean {
  for (const stmt of stmts) {
    if ("Create" in stmt) {
      return true;
    }
    if ("TryCatch" in stmt) {
      const tc = (stmt as { TryCatch: { try_body: Statement[]; catch_body: Statement[] } }).TryCatch;
      if (creates(tc.try_body) || creates(tc.catch_body)) {
        return true;
      }
    }
  }
  return false;
}

function on_choice(choice: Choice, template: Template): void {
  if (!choice.consuming && creates(choice.body)) {
    report(
      choice,
      `Nonconsuming choice '${choice.name}' on template '${template.name}' creates contracts — repeated exercise fans out unbounded copies`
    );
  }
}
