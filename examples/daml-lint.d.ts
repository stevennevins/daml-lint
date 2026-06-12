// Type definitions for daml-lint custom rule scripts.
//
// Write rules in TypeScript against these types, compile to JavaScript
// (e.g. `npx esbuild my-rule.ts --outfile=my-rule.js`), and pass the .js
// file to daml-lint --rules. Node shapes mirror src/ir.rs.

interface Span {
  file: string;
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

interface Field {
  name: string;
  type_: DamlType;
  span: Span;
}

interface EnsureClause {
  raw_text: string;
  span: Span;
}

/** Statements are single-key objects tagged by kind. Use the tag as a
 *  discriminant: `if ("Create" in stmt) { stmt.Create.template_name ... }`.
 *  Expression payloads (`expr`, `condition`, `raw`, ...) are raw source
 *  text; `template_name`, `cid_expr`, and `choice_name` may be "" when
 *  the parser cannot extract them. */
type Statement =
  | { Let: { name: string; expr: string } }
  | { Assert: { condition: string } }
  | { Fetch: { cid_expr: string } }
  | { Archive: { cid_expr: string } }
  | { Create: { template_name: string; raw: string } }
  | { Exercise: { cid_expr: string; choice_name: string; raw: string } }
  | { TryCatch: { try_body: Statement[]; catch_body: Statement[] } }
  | { Other: { raw: string } };

interface Choice {
  name: string;
  consuming: boolean;
  controllers: string[];
  parameters: Field[];
  return_type: DamlType;
  body: Statement[];
  body_raw: string;
  span: Span;
}

interface Template {
  name: string;
  fields: Field[];
  signatories: string[];
  observers: string[];
  ensure_clause: EnsureClause | null;
  choices: Choice[];
  span: Span;
}

interface DamlFunction {
  name: string;
  body: Statement[];
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
  functions: DamlFunction[];
  source: string;
}

/** Report a finding at a node's span, or at an explicit 1-based line number. */
declare function report(node: { span: Span } | number, message: string): void;
