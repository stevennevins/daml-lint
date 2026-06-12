# Goal: Replace daml-lint's line-heuristic parser with a real DAML parser producing a true AST

## Mission

daml-lint (this repo) lets users write custom lint rules in TypeScript/JavaScript
(run by an embedded QuickJS engine, see `src/detectors/script.rs`). Rules receive
"AST" nodes — but the tree is produced by `src/parser.rs`, a line-based keyword
shim, not a grammar. The tree is structurally real down to statement granularity
and **strings below that**: `Statement::Exercise { raw }`, `EnsureClause
{ raw_text }`, `body_raw`. Rule authors who need expression-level facts ("is this
denominator guarded", "what arguments did this exercise pass") are forced into
substring matching, inheriting the shim's precision ceiling.

Build a real parser: lexer → layout resolution → recursive-descent parse →
typed AST with spans on every node, exposed to rule scripts. Rule authors must
get an actual AST, not raw-text leaves.

## Hard-won context — read before designing

1. **DAML is Haskell-flavored with layout (indentation) syntax** plus its own
   keywords: `template`, `choice`, `nonconsuming`, `signatory`, `observer`,
   `ensure`, `controller`, `key`, `maintainer`, `interface`, `viewtype`,
   `with`/`where` blocks, `do` notation. There is **no public tree-sitter
   grammar for DAML**.
2. **tree-sitter was already tried and failed twice here**: the Haskell grammar
   treats DAML keywords as plain identifiers (no structure extracted), and
   tree-sitter-haskell's C scanner corrupted the heap (SIGABRT, 100%
   reproducible on multi-file scans against daml-finance — see commit history
   on branch `custom-detectors`). Do not reach for tree-sitter-haskell again.
   A purpose-written tree-sitter-daml grammar is acceptable only if you write
   and fuzz the external scanner yourself; a hand-rolled pure-Rust
   lexer + layout algorithm + recursive descent is the recommended path
   (full control, no C, no new failure modes).
3. **The lexer is non-negotiable and comes first.** The shim's worst bug class
   was comments and string literals leaking into statements (`-- electing to
   exercise the option` parsed as a ledger Exercise). A tokenizer kills that
   class permanently: line comments `--`, block comments `{- -}` (nested, as
   in Haskell), string literals with escapes, then the Haskell layout
   algorithm (offside rule) to turn indentation into virtual braces.
4. **Parse failures must not kill the scan.** This is a linter: real codebases
   contain files that won't parse. Per-file error recovery — emit a parse
   diagnostic, produce a partial AST (or skip the file with a warning), keep
   scanning. Never `panic!`, never `exit` from the parser.
5. **The current IR is the rule-facing contract.** `src/ir.rs` types serialize
   via serde to JSON and land in JS rules; `examples/daml-lint.d.ts` is the
   TypeScript mirror (keep them in lockstep — there is a test that loads every
   example rule, and the node-census test
   `test_every_node_kind_reaches_scripts` asserts every node kind reaches
   scripts; extend it for every new node kind you add).

## Requirements

### Parser
- Pure Rust, no C dependencies. Current dep set is clap, serde, serde_json,
  rquickjs — keep it that lean; justify any addition.
- Pipeline: lexer (with spans) → layout resolution → recursive descent →
  typed AST. Every node carries a span (line, column — 1-based).
- Expression-level AST below statements: applications, operators (with enough
  fixity handling to be useful), literals, identifiers (qualified vs not),
  lambdas, if/case, do-blocks, record construction/update (`create Foo with
  x = 1`), let-bindings. `exercise cid Choice with arg = v` must become a
  node with a contract-id expression, a choice name, and argument bindings —
  not a string.
- Declarations: module header, imports (qualified/alias/import lists),
  templates (fields, signatory/observer/ensure/key/maintainer expressions,
  choices with parameters/controllers/return type/body), interfaces +
  instances (currently invisible to the shim — the daml-finance audit showed
  interface methods being mis-extracted as top-level functions), top-level
  functions (multi-equation = one function), data/type declarations (at
  minimum recorded with name + span).

### Rule-facing API
- Keep the existing visitor model (`on_template`, `on_choice`, `on_field`,
  `on_function`, `on_import`, `check`) working — existing rules in `examples/`
  must run unmodified or with mechanical, documented changes.
- Replace string leaves with expression nodes. Design the JSON encoding for
  expressions deliberately (tagged unions like the current `Statement`
  encoding work well in TS: `{ App: {...} } | { Lit: {...} } | ...`).
  Update `examples/daml-lint.d.ts` with precise discriminated unions and
  verify all example `.ts` files type-check: `npx -p typescript tsc --noEmit
  --strict --lib es2023 <rule>.ts daml-lint.d.ts`.
- Consider keeping `raw`/`body_raw` fields alongside structured nodes during
  a deprecation window so existing user rules don't break silently.

### Verification (the bar is execution against real code, not inspection)

The primary verification harness is the daml-finance corpus at
`/tmp/finance-lint-repo/daml` — all 634 `*.daml` modules from
https://github.com/digital-asset/daml-finance (if missing, regenerate:
shallow-clone to /tmp/daml-finance, `find . -name "*.daml" -exec cp --parents
{} /tmp/finance-lint-repo/daml/ \;`). **Every phase ends by running against
this corpus**, not just the final one:

- **Phase gate, every phase**: full corpus scan completes with no crash, no
  hang; report parse-failure count and triage every failure (parser bug vs
  genuinely exotic syntax). Track the failure count phase over phase — it
  must be monotonically non-increasing.
- **AST ground-truth checks**: for known-rich corpus files, assert specific
  parsed facts as integration tests — e.g.
  `Daml/Finance/Settlement/V4/Instruction.daml`: template `Instruction` with
  13 fields, consuming choices `Allocate`/`Approve`/`Cancel`/`Execute`,
  ~20 qualified aliased imports; `Account/V4/Account.daml`: template with an
  ensure clause and `Credit`/`Debit` choices; the interface `Reference`
  templates: choices controlled by `signatory this`. Pick at least 10 such
  files, hand-verify the facts against the source once, then encode them as
  tests so parser changes can't silently regress structure extraction.
- TDD throughout. Unit tests per lexer/layout/parser stage; snapshot tests of
  full-module ASTs for a fixed set of corpus files.
- **Finding regression gate**: with the 8 example rules + 6 builtins, the
  audited-correct baseline on that corpus is: template-requires-ensure 170,
  unqualified-da-import 306, consuming-choice-signatory-controller 50,
  no-bare-contractid-field 20, function-ledger-actions 14 (these 14 are
  verified true positives — file:line list in PR #1 discussion),
  no-create-in-nonconsuming 6, no-trace 0, choice-param-shadows-field 0.
  Deviations require per-finding justification (a real parser SHOULD fix the
  known false negatives: `exerciseCmd` not detected; unqualified pure
  `exercise` calls indistinguishable from ledger exercise; the bare `fetch
  cid` form without `<-`; statement-after-catch swallowed into catch_body).
- Builtin detectors (`src/detectors/*.rs`) consume the IR too — port them and
  keep their tests green (`cargo test`, currently 31 tests).
- Adversarial pass at the end: spawn a reviewer that writes hostile DAML
  (comments containing keywords, strings containing `template`, nested block
  comments, tabs vs spaces, unicode identifiers, 10k-line files) and verifies
  parse correctness + no hangs. Fuzz the lexer if practical.
- Performance: full 634-file corpus scan with all rules under 2 seconds
  release build (current shim does it in 0.06s; you have headroom, not a
  blank check).

### Process
- Work on a branch off `custom-detectors` (PR #1, repo stevennevins/daml-lint).
- Phase the work; each phase compiles, tests green, committed: (1) lexer +
  layout, (2) declaration parsing → existing IR shapes (drop-in for the shim,
  corpus + regression gates pass), (3) expression AST + new statement nodes +
  d.ts v2 + example-rule updates, (4) interface/instance support, (5)
  adversarial pass + docs.
- Update README's custom-detectors section and `daml-lint.d.ts` docs as the
  API changes. The README's claims are tested by reviewers who run them —
  keep every command in it literally true.
- Do not change versions of existing dependencies. Do not reintroduce
  tree-sitter. Fail loud: parse diagnostics go to stderr with file:line.

## Definition of done

A rule author can write, in TypeScript against `daml-lint.d.ts`, a rule that
inspects the denominator expression of a division inside a choice body and
checks whether a preceding statement asserts it non-zero — entirely on typed
nodes, without touching a single raw-text field — and that rule type-checks,
runs against daml-finance, and reports correct spans. The unguarded-division
builtin being reimplementable as a custom rule on public AST alone is the
acceptance test.
