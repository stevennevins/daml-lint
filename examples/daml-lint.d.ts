// Type definitions for daml-lint custom rule scripts.
//
// Write rules in TypeScript against these types, compile to JavaScript
// (e.g. `npx esbuild my-rule.ts --outfile=my-rule.js`), and pass the .js
// file to daml-lint --rules. Node shapes mirror src/ir.rs.
//
// v2: nodes now carry a real expression AST (`Expr`). The raw-text fields
// of v1 (`body_raw`, `raw_text`, statement `expr`/`condition`/`raw`) are
// kept for compatibility but deprecated; prefer the structured payloads.

/** Span of a declaration-level node (template, choice, field, ...). */
interface Span {
  file: string;
  line: number;
  column: number;
}

/** Position of an expression-level node. 1-based; the file is the
 *  enclosing module's. */
interface SrcPos {
  line: number;
  column: number;
}

/** DAML types as parsed by daml-lint.
 *
 *  Builtin scalar types serialize as bare strings: "Party", "Text",
 *  "Decimal", "Int", "Bool", "Date", "Time", "Unit" (for `()`), and
 *  "Unknown" (anything the parser could not classify, e.g. tuples).
 *
 *  Parameterized types are single-key objects whose value is the inner
 *  type: { List: "Text" }, { Optional: "Party" }, { TextMap: "Int" },
 *  { ContractId: { Named: "Iou" } }.
 *
 *  User-defined / unrecognized capitalized types are { Named: "..." }
 *  where the payload is the raw type text (a string, NOT a DamlType) —
 *  e.g. { Named: "Iou" } or { Named: "Map.Map Party Decimal" }. */
type DamlType =
  | "Party"
  | "Text"
  | "Decimal"
  | "Int"
  | "Bool"
  | "Date"
  | "Time"
  | "Unit"
  | "Unknown"
  | { ContractId: DamlType }
  | { List: DamlType }
  | { Optional: DamlType }
  | { TextMap: DamlType }
  | { Named: string };

/** Expression AST. Tagged unions: use the key as discriminant, e.g.
 *  `if ("BinOp" in e && e.BinOp.op === "/") { ... }`.
 *
 *  - Var: variable reference; qualifier is the module alias for qualified
 *    names (`Map.lookup` → { name: "lookup", qualifier: "Map" }).
 *  - Con: constructor / type name in expression position (`Some`, `Iou`).
 *  - Lit: kind is "Int" | "Decimal" | "Text" | "Char"; value is source text.
 *  - App: application, flattened (`f a b` has two args).
 *  - BinOp: op is source-level text (`+`, `/`, `&&`, "`div`" for backtick
 *    application, ".." for ranges).
 *  - DoBlock: nested do, lowered to statements like a choice body.
 *  - Record: construction (`Iou with amount = 1.0`) when base is Con,
 *    update (`this with owner = p`) otherwise. Punned fields and `..`
 *    spreads have value: null.
 *  - Unknown: no structured encoding (operator sections, comprehension
 *    qualifiers, recovered parse errors); raw preserves source text. */
type Expr =
  | { Var: { name: string; qualifier: string | null; span: SrcPos } }
  | { Con: { name: string; qualifier: string | null; span: SrcPos } }
  | { Lit: { kind: "Int" | "Decimal" | "Text" | "Char"; value: string; span: SrcPos } }
  | { App: { func: Expr; args: Expr[]; span: SrcPos } }
  | { BinOp: { op: string; lhs: Expr; rhs: Expr; span: SrcPos } }
  | { Neg: { expr: Expr; span: SrcPos } }
  | { Lambda: { params: string[]; body: Expr; span: SrcPos } }
  | { If: { cond: Expr; then_branch: Expr; else_branch: Expr; span: SrcPos } }
  | { Case: { scrutinee: Expr; alts: CaseAlt[]; span: SrcPos } }
  | { DoBlock: { statements: Statement[]; span: SrcPos } }
  | { LetIn: { bindings: LetBinding[]; body: Expr; span: SrcPos } }
  | { Record: { base: Expr; fields: RecordField[]; span: SrcPos } }
  | { Tuple: { items: Expr[]; span: SrcPos } }
  | { List: { items: Expr[]; span: SrcPos } }
  | { Unknown: { raw: string; span: SrcPos } };

interface CaseAlt {
  /** Pattern rendered to source text: "Some x", "[]", "_". */
  pattern: string;
  body: Expr;
}

interface LetBinding {
  /** Bound name; for function bindings includes parameters ("go x"). */
  name: string;
  value: Expr;
}

interface RecordField {
  name: string;
  /** null for punned fields (`Iou with owner`) and `..` spreads. */
  value: Expr | null;
}

interface Field {
  name: string;
  type_: DamlType;
  span: Span;
}

interface EnsureClause {
  /** @deprecated prefer `expr`. "ensure " + rendered condition. */
  raw_text: string;
  /** Structured ensure condition. */
  expr: Expr;
  span: Span;
}

/** Statements are single-key objects tagged by kind. Use the tag as a
 *  discriminant: `if ("Create" in stmt) { stmt.Create.template_name ... }`.
 *
 *  Raw-text payloads (`expr`, `condition`, `raw`) are deprecated; the
 *  structured fields (`value`, `condition_expr`, `argument`, `cid`) carry
 *  the parse tree. `binder` is the pattern text bound by `x <- ...`, when
 *  present. Ledger actions nested under if/case/`$`/lambdas are surfaced
 *  as their own statements, in source order. */
type Statement =
  | { Let: { name: string; expr: string; value: Expr; span: SrcPos } }
  | { Assert: { condition: string; condition_expr: Expr; span: SrcPos } }
  | { Fetch: { cid_expr: string; cid: Expr; binder: string | null; span: SrcPos } }
  | { Archive: { cid_expr: string; cid: Expr; span: SrcPos } }
  | {
      Create: {
        template_name: string;
        raw: string;
        /** The created payload, usually a Record expression. */
        argument: Expr;
        binder: string | null;
        span: SrcPos;
      };
    }
  | {
      Exercise: {
        cid_expr: string;
        choice_name: string;
        raw: string;
        cid: Expr;
        /** The choice argument (usually a Record expression), if present. */
        argument: Expr | null;
        binder: string | null;
        span: SrcPos;
      };
    }
  | { TryCatch: { try_body: Statement[]; catch_body: Statement[]; span: SrcPos } }
  | { Other: { raw: string; expr: Expr; binder: string | null; span: SrcPos } };

interface Choice {
  name: string;
  consuming: boolean;
  /** Controller expressions rendered to text (list literals flattened). */
  controllers: string[];
  /** Structured controller expressions. */
  controller_exprs: Expr[];
  /** Choice observers, if declared. */
  observer_exprs: Expr[];
  parameters: Field[];
  return_type: DamlType;
  body: Statement[];
  /** @deprecated original source lines of the choice body; prefer `body`. */
  body_raw: string;
  span: Span;
}

interface InterfaceInstance {
  interface_name: string;
  /** Implemented method names, in declaration order. */
  methods: string[];
  span: Span;
}

interface Template {
  name: string;
  fields: Field[];
  /** Signatory expressions rendered to text (list literals flattened). */
  signatories: string[];
  observers: string[];
  /** Structured signatory/observer expressions. */
  signatory_exprs: Expr[];
  observer_exprs: Expr[];
  ensure_clause: EnsureClause | null;
  /** `key <expr> : <Type>`, if declared. */
  key_expr: Expr | null;
  key_type: string | null;
  maintainer_exprs: Expr[];
  choices: Choice[];
  /** Interfaces this template implements. */
  interface_instances: InterfaceInstance[];
  span: Span;
}

interface InterfaceMethod {
  name: string;
  type_text: string;
  span: Span;
}

/** A DAML interface declaration. Visited via on_interface. */
interface DamlInterface {
  name: string;
  /** Interfaces this interface requires (`requires Lockable.I`). */
  requires: string[];
  viewtype: string | null;
  methods: InterfaceMethod[];
  choices: Choice[];
  span: Span;
}

interface DamlFunction {
  name: string;
  /** Declared type signature text, if present. */
  type_signature: string | null;
  body: Statement[];
  /** @deprecated original source lines of the function; prefer `body`. */
  body_raw: string;
  span: Span;
}

interface Import {
  module_name: string;
  qualified: boolean;
  alias: string | null;
  span: Span;
}

interface DamlModule {
  name: string;
  file: string;
  imports: Import[];
  templates: Template[];
  interfaces: DamlInterface[];
  functions: DamlFunction[];
  source: string;
}

/** Report a finding at a node's span, or at an explicit 1-based line number. */
declare function report(
  node: { span: Span } | { span: SrcPos } | number,
  message: string
): void;
