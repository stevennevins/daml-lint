// Compiled from no-create-in-nonconsuming.ts — this is the file you pass to --rules.

const NAME = "no-create-in-nonconsuming";
const SEVERITY = "medium";
const DESCRIPTION = "Nonconsuming choices should not create contracts";

function creates(stmts) {
  for (const stmt of stmts) {
    if ("Create" in stmt) {
      return true;
    }
    if ("TryCatch" in stmt) {
      if (creates(stmt.TryCatch.try_body) || creates(stmt.TryCatch.catch_body)) {
        return true;
      }
    }
  }
  return false;
}

function on_choice(choice, template) {
  if (!choice.consuming && creates(choice.body)) {
    report(
      choice,
      `Nonconsuming choice '${choice.name}' on template '${template.name}' creates contracts — repeated exercise fans out unbounded copies`
    );
  }
}
