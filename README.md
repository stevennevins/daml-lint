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

Define your own detectors and pass them with `--rules` (repeatable). Two kinds:

- **AST rules** (`.rhai`) — script with visitor functions over the parsed module, in the style of [solhint custom rules](https://github.com/protofire/solhint/blob/master/docs/writing-plugins.md). Use these for anything structural.
- **Regex rules** (`.json`) — match raw source lines. Use these for simple banned-token checks.

```sh
daml-lint ./daml/ --rules my-rules.rhai --rules more-rules.json
```

#### AST rules

A rule is a [Rhai](https://rhai.rs) script: constants for metadata, plus visitor
functions named after the node types you care about — like solhint's
`ContractDefinition(node)` callbacks:

```rhai
const NAME = "template-requires-ensure";
const SEVERITY = "medium";
const DESCRIPTION = "Every template must declare an ensure clause";   // optional

fn on_template(template) {
    if template.ensure_clause == () {
        report(template, `Template '${template.name}' has no ensure clause`);
    }
}
```

Visitors (define any subset, at least one):

| Function | Called for | Node fields |
|---|---|---|
| `on_template(template)` | each template | `name`, `fields`, `signatories`, `observers`, `ensure_clause` (`()` if absent), `choices`, `span` |
| `on_choice(choice [, template])` | each choice | `name`, `consuming`, `controllers`, `parameters`, `return_type`, `body`, `body_raw`, `span` |
| `on_field(field [, template])` | each template field | `name`, `type_`, `span` |
| `on_function(function)` | each top-level function | `name`, `body`, `body_raw`, `span` |
| `on_import(import)` | each import | `module_name`, `qualified`, `alias` |
| `check(m)` | once per module | `name`, `file`, `imports`, `templates`, `functions`, `source` |

Report findings with `report(node, message)` (location taken from the node's
`span`) or `report(line, message)`. The rule's `SEVERITY` applies to all its
findings. Node shapes mirror the IR in [src/ir.rs](src/ir.rs); statement nodes
in `body` are maps keyed by kind, e.g. `stmt.contains("Create")`.

Heads up: `module` is a reserved word in Rhai — name your `check` parameter
something else (e.g. `m`). If a script fails at runtime the scan aborts with
exit code 2; rule errors are never swallowed.

See [examples/template-requires-ensure.rhai](examples/template-requires-ensure.rhai)
and [examples/consuming-choice-signatory-controller.rhai](examples/consuming-choice-signatory-controller.rhai)
(the latter cross-references choice controllers against template signatories —
the kind of rule regex can't express).

#### Regex rules

Each rule scans every source line and reports a finding where the pattern matches:

```json
[
  {
    "name": "no-trace",
    "severity": "low",
    "description": "Debug trace left in code",
    "pattern": "\\btrace\\b",
    "message": "Remove debug trace calls before deploying"
  }
]
```

`severity` is one of `critical`, `high`, `medium`, `low`, `info`. `pattern` is a Rust regex; `description` is optional. Custom rules run alongside the built-in detectors and appear in all output formats. See [examples/custom-rules.json](examples/custom-rules.json) for a complete example.

Things to know:

- Backslashes in regexes must be doubled for JSON: write `"\\btrace\\b"` to mean the regex `\btrace\b`. A single-backslash `"\b"` is rejected with an error.
- Patterns match raw source lines, including comments and string literals — `-- TODO: remove trace` will trigger a `trace` rule. A line matching N times yields N findings.
- Patterns are matched one line at a time, so multiline constructs can't be matched.
- Rule names must not collide with built-in detector names or each other.

To check that a rules file parses without running a scan, point the tool at a nonexistent path — rule errors are reported before file discovery. (A valid rules file then prints `No .daml files found.`, which also exits 2 — go by the message, not the exit code.)

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

