# daml-lint

[![License: AGPL v3](https://img.shields.io/badge/License-AGPL_v3-blue.svg)](https://www.gnu.org/licenses/agpl-3.0)

> [!WARNING]
> This software is experimental and not intended for production use. Use at your own risk.

Static analysis scanner for [DAML](https://www.digitalasset.com/developers) smart contracts. Catches security vulnerabilities and anti-patterns through AST pattern matching, similar to what [Slither](https://github.com/crytic/slither) does for Solidity.

## Detectors

| Detector | Severity | Description |
|----------|----------|-------------|
| `missing-ensure-decimal` | HIGH | Template has Decimal fields without an `ensure` clause bounding them to > 0 |
| `unguarded-division` | HIGH | Division operation without a prior guard checking the denominator is non-zero |
| `missing-positive-amount` | HIGH | Choice accepts amount/quantity/price parameter without asserting it is positive |
| `archive-before-execute` | HIGH | Contract archived before a `try/catch` block — contract is lost if execution fails |
| `head-of-list-query` | MEDIUM | Pattern match on head of `queryFilter` result — non-deterministic ordering risk |
| `unbounded-fields` | MEDIUM | Text, List, or TextMap fields without size bounds in the `ensure` clause |

## Installation

Requires [Rust](https://rustup.rs/) 1.70+.

```sh
git clone https://github.com/OpenZeppelin/daml-lint.git
cd daml-lint
cargo install --path .
```

## Usage

Scan a single file:

```sh
daml-lint src/MyContract.daml
```

Scan a directory recursively:

```sh
daml-lint ./daml/
```

Choose an output format:

```sh
daml-lint ./daml/ --format sarif    # SARIF JSON (GitHub / IDE integration)
daml-lint ./daml/ --format markdown # Human-readable (default)
daml-lint ./daml/ --format json     # Machine-readable JSON
```

Write results to a file:

```sh
daml-lint ./daml/ --format sarif --output report.sarif
```

### Custom detectors

Define your own detectors as AST rule scripts and pass them with `--rules`
(repeatable), in the style of [solhint custom rules](https://github.com/protofire/solhint/blob/master/docs/writing-plugins.md):

```sh
daml-lint ./daml/ --rules my-rule.js --rules another-rule.js
```

A rule is TypeScript/JavaScript (executed by an embedded QuickJS engine):
constants for metadata, plus visitor functions named after the node types you
care about — like solhint's `ContractDefinition(node)` callbacks. Write rules
in TypeScript against [examples/daml-lint.d.ts](examples/daml-lint.d.ts) for
type checking and autocomplete:

```typescript
const NAME = "template-requires-ensure";
const SEVERITY = "medium";
const DESCRIPTION = "Every template must declare an ensure clause";   // optional

function on_template(template: Template): void {
  if (template.ensure_clause === null) {
    report(template, `Template '${template.name}' has no ensure clause`);
  }
}
```

then compile to the JavaScript file you pass to `--rules`:

```sh
npx esbuild my-rule.ts --outfile=my-rule.js   # or tsc
```

(Plain JavaScript rules work directly — the compile step is only for TypeScript.)

Visitors (define any subset, at least one):

| Function | Called for | Node fields |
|---|---|---|
| `on_template(template)` | each template | `name`, `fields`, `signatories`, `observers`, `ensure_clause` (`null` if absent), `choices`, `span` |
| `on_choice(choice, template)` | each choice | `name`, `consuming`, `controllers`, `parameters`, `return_type`, `body`, `body_raw`, `span` |
| `on_field(field, template)` | each template field | `name`, `type_`, `span` |
| `on_function(function)` | each top-level function | `name`, `body`, `body_raw`, `span` |
| `on_import(import)` | each import | `module_name`, `qualified`, `alias` |
| `check(m)` | once per module | `name`, `file`, `imports`, `templates`, `functions`, `source` |

Report findings with `report(node, message)` (location taken from the node's
`span`) or `report(line, message)`. The rule's `SEVERITY` applies to all its
findings. Node shapes are declared in
[examples/daml-lint.d.ts](examples/daml-lint.d.ts) and mirror the IR in
[src/ir.rs](src/ir.rs); statement nodes in `body` are objects keyed by kind,
e.g. `"Create" in stmt`.

Heads up: visitors must be `function` declarations — arrow functions assigned
to `const` are not discovered. If a script fails at runtime the scan aborts
with exit code 2; rule errors are never swallowed. A runaway loop is
interrupted so a broken rule can't hang CI. The engine runs JavaScript
(ES2023) — no Node APIs, no `require`/`import`, no filesystem or network.

`SEVERITY` is one of `critical`, `high`, `medium`, `low`, `info`. Custom rules
run alongside the built-in detectors, appear in all output formats, and count
toward `--fail-on`. Rule names must not collide with built-in detector names
or each other.

Examples:

- [examples/template-requires-ensure.ts](examples/template-requires-ensure.ts) — structural check on a single node
- [examples/consuming-choice-signatory-controller.ts](examples/consuming-choice-signatory-controller.ts) — cross-references choice controllers against template signatories
- [examples/no-create-in-nonconsuming.ts](examples/no-create-in-nonconsuming.ts) — walks choice body statements, recursing into try/catch
- [examples/no-trace.ts](examples/no-trace.ts) — banned-token check over raw source lines

Each example ships with its compiled `.js` next to it — that's the file
`--rules` takes.

To check that a rule script parses without running a scan, point the tool at a nonexistent path — rule errors are reported before file discovery. (A valid script then prints `No .daml files found.`, which also exits 2 — go by the message, not the exit code.)

### CI gating

Use `--fail-on` to control when the tool returns a non-zero exit code:

```sh
daml-lint ./daml/ --fail-on medium   # fail on medium or above
daml-lint ./daml/ --fail-on critical # fail only on critical
```

## Output Formats

- **SARIF** — Standard format for static analysis tools. Integrates with GitHub Code Scanning and IDEs.
- **Markdown** — Human-readable report grouped by severity. Good for pull request comments.
- **JSON** — Flat findings array with summary counts. Good for dashboards and aggregation.

## Exit Codes

| Code | Meaning |
|------|---------|
| 0 | No findings at or above the `--fail-on` threshold |
| 1 | One or more findings at or above the threshold |
| 2 | CLI error (invalid format, no files found, etc.) |

