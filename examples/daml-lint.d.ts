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

/** Builtin scalar types serialize as strings ("Party", "Decimal", ...);
 *  parameterized types as single-key objects ({ List: "Party" }, { ContractId: ... }). */
type DamlType = string | { [kind: string]: DamlType | DamlType[] };

interface Field {
  name: string;
  type_: DamlType;
  span: Span;
}

interface EnsureClause {
  raw_text: string;
  span: Span;
}

/** Statements are single-key objects tagged by kind:
 *  { Let: {...} } | { Assert: {...} } | { Fetch: {...} } | { Archive: {...} } |
 *  { Create: {...} } | { Exercise: {...} } | { TryCatch: {...} } | { Other: {...} } */
type Statement = { [kind: string]: unknown };

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
