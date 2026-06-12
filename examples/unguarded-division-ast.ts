// The acceptance test for the expression AST: reimplements the
// unguarded-division builtin as a custom rule using ONLY typed nodes —
// no body_raw, no raw, no string matching on source text.
//
// Finds division expressions in choice bodies and reports them unless a
// preceding statement asserts the denominator non-zero / positive.
// Compile: npx esbuild unguarded-division-ast.ts --outfile=unguarded-division-ast.js

const NAME = "unguarded-division-ast";
const SEVERITY = "high";
const DESCRIPTION = "Division whose denominator has no prior non-zero assertion (AST rule)";

/** Render a denominator expression to a comparable key: variable paths
 *  like `x`, `transfer.amount` compare structurally. */
function exprKey(e: Expr): string | null {
  if ("Var" in e) {
    return e.Var.qualifier ? `${e.Var.qualifier}.${e.Var.name}` : e.Var.name;
  }
  if ("BinOp" in e && e.BinOp.op === ".") {
    const l = exprKey(e.BinOp.lhs);
    const r = exprKey(e.BinOp.rhs);
    return l !== null && r !== null ? `${l}.${r}` : null;
  }
  return null;
}

/** Collect all division denominators in an expression tree. */
function divisions(e: Expr, out: { denom: Expr; span: SrcPos }[]): void {
  if ("BinOp" in e) {
    const b = e.BinOp;
    if (b.op === "/" || b.op === "`div`" || b.op === "`divD`") {
      out.push({ denom: b.rhs, span: b.span });
    }
    divisions(b.lhs, out);
    divisions(b.rhs, out);
  } else if ("App" in e) {
    divisions(e.App.func, out);
    for (const a of e.App.args) divisions(a, out);
  } else if ("Neg" in e) {
    divisions(e.Neg.expr, out);
  } else if ("Lambda" in e) {
    divisions(e.Lambda.body, out);
  } else if ("If" in e) {
    divisions(e.If.cond, out);
    divisions(e.If.then_branch, out);
    divisions(e.If.else_branch, out);
  } else if ("Case" in e) {
    divisions(e.Case.scrutinee, out);
    for (const alt of e.Case.alts) divisions(alt.body, out);
  } else if ("LetIn" in e) {
    for (const b of e.LetIn.bindings) divisions(b.value, out);
    divisions(e.LetIn.body, out);
  } else if ("Record" in e) {
    for (const f of e.Record.fields) {
      if (f.value !== null) divisions(f.value, out);
    }
  } else if ("Tuple" in e) {
    for (const i of e.Tuple.items) divisions(i, out);
  } else if ("List" in e) {
    for (const i of e.List.items) divisions(i, out);
  } else if ("DoBlock" in e) {
    // handled at statement level by the caller
  }
}

/** Does this assertion condition bound `key` away from zero?
 *  Recognizes `key > 0`, `key >= 1`, `key /= 0`, `0 < key` and the same
 *  under && / || conjunctions — structurally. */
function guardsKey(cond: Expr, key: string): boolean {
  if (!("BinOp" in cond)) {
    return false;
  }
  const b = cond.BinOp;
  if (b.op === "&&" || b.op === "||") {
    return guardsKey(b.lhs, key) || guardsKey(b.rhs, key);
  }
  const comparisons = [">", ">=", "/=", "!=", "<", "<="];
  if (!comparisons.includes(b.op)) {
    return false;
  }
  const lk = exprKey(b.lhs);
  const rk = exprKey(b.rhs);
  const lhsIsKey = lk === key;
  const rhsIsKey = rk === key;
  const isZeroish = (e: Expr): boolean =>
    "Lit" in e && (e.Lit.kind === "Int" || e.Lit.kind === "Decimal");
  return (lhsIsKey && isZeroish(b.rhs)) || (rhsIsKey && isZeroish(b.lhs));
}

/** Expressions reachable from a statement, for division scanning. */
function stmtExprs(stmt: Statement): Expr[] {
  if ("Let" in stmt) return [stmt.Let.value];
  if ("Assert" in stmt) return [stmt.Assert.condition_expr];
  if ("Other" in stmt) return [stmt.Other.expr];
  if ("Create" in stmt) return [stmt.Create.argument];
  if ("Exercise" in stmt) {
    return stmt.Exercise.argument !== null ? [stmt.Exercise.argument] : [];
  }
  return [];
}

function checkStatements(stmts: Statement[], choiceName: string): void {
  const guarded = new Set<string>();
  for (const stmt of stmts) {
    if ("Assert" in stmt) {
      // Record every key this assertion guards.
      const conds: Expr[] = [stmt.Assert.condition_expr];
      while (conds.length > 0) {
        const c = conds.pop()!;
        if ("App" in c) {
          conds.push(...c.App.args);
        } else if ("BinOp" in c) {
          conds.push(c.BinOp.lhs, c.BinOp.rhs);
        }
        // guardsKey() is checked per division below; here we collect
        // candidate keys from comparison operands.
        if ("BinOp" in c) {
          const lk = exprKey(c.BinOp.lhs);
          const rk = exprKey(c.BinOp.rhs);
          for (const k of [lk, rk]) {
            if (k !== null && guardsKey(c, k)) {
              guarded.add(k);
            }
          }
        }
      }
      continue;
    }
    if ("TryCatch" in stmt) {
      checkStatements(stmt.TryCatch.try_body, choiceName);
      checkStatements(stmt.TryCatch.catch_body, choiceName);
      continue;
    }
    const found: { denom: Expr; span: SrcPos }[] = [];
    for (const e of stmtExprs(stmt)) {
      divisions(e, found);
    }
    for (const d of found) {
      const key = exprKey(d.denom);
      // Literal denominators are safe; unresolvable denominators are
      // reported (cannot prove a guard).
      if ("Lit" in d.denom) {
        continue;
      }
      if (key !== null && guarded.has(key)) {
        continue;
      }
      report(
        { span: d.span },
        `Division by '${key ?? "<complex expression>"}' in choice '${choiceName}' has no prior non-zero assertion`
      );
    }
  }
}

function on_choice(choice: Choice, _template: Template): void {
  checkStatements(choice.body, choice.name);
}
