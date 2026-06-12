// Top-level functions that archive or exercise contracts hide ledger
// mutations outside any choice, making authorization harder to audit.
// Exercises on_function and the Archive/Exercise statement variants.
// Compile: npx esbuild function-ledger-actions.ts --outfile=function-ledger-actions.js

const NAME = "function-ledger-actions";
const SEVERITY = "info";
const DESCRIPTION = "Top-level functions performing archive/exercise — verify authorization is audited";

function ledgerActions(stmts: Statement[]): string[] {
  const found: string[] = [];
  for (const stmt of stmts) {
    if ("Archive" in stmt) {
      found.push("archive");
    }
    if ("Exercise" in stmt) {
      found.push("exercise");
    }
    if ("TryCatch" in stmt) {
      const tc = (stmt as { TryCatch: { try_body: Statement[]; catch_body: Statement[] } }).TryCatch;
      found.push(...ledgerActions(tc.try_body), ...ledgerActions(tc.catch_body));
    }
  }
  return found;
}

function on_function(fn: DamlFunction): void {
  const actions = ledgerActions(fn.body);
  if (actions.length > 0) {
    report(fn, `Function '${fn.name}' performs ledger actions (${actions.join(", ")}) outside a choice`);
  }
}
