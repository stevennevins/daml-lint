// Compiled from function-ledger-actions.ts — this is the file you pass to --rules.

const NAME = "function-ledger-actions";
const SEVERITY = "info";
const DESCRIPTION = "Top-level functions performing archive/exercise — verify authorization is audited";

function ledgerActions(stmts) {
  const found = [];
  for (const stmt of stmts) {
    if ("Archive" in stmt) {
      found.push("archive");
    }
    if ("Exercise" in stmt) {
      found.push("exercise");
    }
    if ("TryCatch" in stmt) {
      found.push(...ledgerActions(stmt.TryCatch.try_body), ...ledgerActions(stmt.TryCatch.catch_body));
    }
  }
  return found;
}

function on_function(fn) {
  const actions = ledgerActions(fn.body);
  if (actions.length > 0) {
    report(fn, `Function '${fn.name}' performs ledger actions (${actions.join(", ")}) outside a choice`);
  }
}
