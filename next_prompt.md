# Next: lossless trivia for formatting — without regressing the parser

## Problems to address

1. The lexer throws comments away (there's literally a test:
   `line_comment_with_keywords_produces_no_tokens`). Fine for linting, fatal
   for formatting. Need it to emit comment/blank-line trivia with spans so the
   printer can re-attach them — and node positions already exist everywhere,
   which is exactly what comment re-attachment needs.
2. Typed AST ≠ lossless CST. Need to check what the parser normalizes away
   (parens, operator layout). The desugar oracle covers us while finding out.

## Current state — the baseline you must not regress

As of `main` @ `8d916e3` (June 2026). Every number below is a gate: re-measure
before you start (machines drift, corpora at HEAD drift), pin your own
baseline, then hold it through every phase.

### Test suite

- `cargo test` → **91 tests, 0 failed**. Suites that matter most:
  - `src/corpus_tests.rs` — 10 hand-verified AST ground-truth tests against
    daml-finance files + a corpus-wide zero-parse-diagnostics gate. These
    skip silently when the corpus is absent — **make sure the corpus exists
    or you are testing nothing.**
  - `src/adversarial_tests.rs` — hostile inputs (comments/strings containing
    keywords, nested block comments, tabs, unicode, CRLF, BOM, 10k-line file,
    5000-deep parens, unterminated everything) plus `sdk_corpus_syntax_gaps`,
    which pins every syntax class that once failed (view patterns, `\case`,
    lazy patterns, operator equations, inline `with...where`, compact choice
    headers, comma guards). Each of those regressed at least once — do not
    weaken them.
  - `src/detectors/script.rs::test_every_node_kind_reaches_scripts` — the
    node census. Any new IR node kind must be added here or it can silently
    fail to reach rule scripts.

### Corpus 1: daml-finance (the linting target — primary gate)

- Location: `/tmp/finance-lint-repo/daml`, 634 `.daml` files. Regenerate if
  missing:
  ```sh
  git clone --depth 1 https://github.com/digital-asset/daml-finance /tmp/daml-finance
  cd /tmp/daml-finance && find . -name "*.daml" -exec cp --parents {} /tmp/finance-lint-repo/daml/ \;
  ```
- **Parse gate: 0 parse diagnostics** on stderr across all 634 files.
- **Perf gate:** full scan with builtins + all 9 example rules **< 2s**
  release build (currently ~1.2s; builtins-only ~0.16s):
  ```sh
  cargo build --release
  time ./target/release/daml-lint /tmp/finance-lint-repo/daml --format json \
    $(for f in examples/*.js; do echo --rules $f; done) \
    --fail-on critical -o /tmp/scan.json 2>/tmp/diag.txt
  grep -c 'parse:' /tmp/diag.txt   # must be 0
  ```
- **Finding regression gate — audited counts (total 970):**

  | detector | count |
  |---|---|
  | function-ledger-actions | 210 |
  | unqualified-da-import | 306 |
  | template-requires-ensure | 170 |
  | unbounded-fields | 80 |
  | missing-ensure-decimal | 76 |
  | consuming-choice-signatory-controller | 50 |
  | unguarded-division | 46 |
  | no-bare-contractid-field | 22 |
  | no-create-in-nonconsuming | 6 |
  | head-of-list-query | 2 |
  | missing-positive-amount | 2 |
  | no-trace, choice-param-shadows-field, unguarded-division-ast, archive-before-execute | 0 |

  Compare with set-difference, not just totals — lost findings can hide
  behind equal counts:
  ```python
  old = {(f['file'], f['line'], f['detector']) for f in json.load(open('base.json'))['findings']}
  new = {(f['file'], f['line'], f['detector']) for f in json.load(open('scan.json'))['findings']}
  assert not old - new, sorted(old - new)   # every lost finding needs a written justification
  ```

### Corpus 2: digital-asset/daml SDK (the stress corpus)

- Location: `/tmp/daml-repo` (shallow clone of
  https://github.com/digital-asset/daml), **1123** `.daml` files at the
  pinned checkout. This corpus contains compiler stdlib internals and
  deliberately-broken fixtures — 100% clean is not expected; **hangs and
  crashes are the real signal**.
- **Gate: 1120/1123 files parse with zero diagnostics, 0 hangs, 0 panics.**
  The 3 permitted failures, each with a reason:
  - `daml-stdlib-src/DA/BigNumeric.daml` — CPP `#ifndef` guards **two**
    alternate `module` headers; unparseable without a preprocessor.
  - `daml-stdlib-src/DA/ContractKeys.daml` — same dual-module-header CPP.
  - `daml-stdlib-src/DA/Stack.daml` — GHC implicit params (`?callStack`),
    out of DAML surface syntax.
  Any file beyond these three is a regression. Any *new* clean file is fine.
- **Hang detection must be per-file with a timeout** — a whole-directory scan
  hides one spinning file behind aggregate slowness. This corpus found 3
  infinite loops the friendly corpus never did:
  ```sh
  find /tmp/daml-repo -name '*.daml' | sort > /tmp/sdk-files.txt
  while read -r f; do
    timeout 5 ./target/release/daml-lint "$f" --format json --fail-on critical -o /dev/null 2>/tmp/one.txt
    [ $? -eq 124 ] && echo "HANG: $f"
    grep -q 'parse:' /tmp/one.txt && echo "DIAG: $f"
  done < /tmp/sdk-files.txt
  ```
- Reference point: Artifex1/tree-sitter-daml parses 1115/1123 of the same
  list (and fails 2 daml-finance files we parse). Staying ≥ its number is a
  nice-to-have; the hard gate is the 1120/0/0 above.

### Process patterns that earned their keep

- **Every phase ends with both corpus runs.** Failure counts must be
  monotonically non-increasing phase over phase; every new failure gets
  triaged in the commit message (parser bug vs. genuinely exotic syntax).
- **Bisect hangs/failures by file prefix** (`head -n $mid file > slice.daml`,
  binary search on the first failing prefix) — fastest route from "this 2000-
  line file breaks" to the exact construct.
- **Build a minimal repro before fixing**, then keep it as a test. Every fix
  in `adversarial_tests.rs` and the layout/parse tests started as a corpus
  failure.
- **The "no progress" invariant:** any parser loop that recovers from errors
  must consume at least one token per iteration. The 3 infinite loops were
  all error-recovery paths that left the cursor on a token nothing would
  consume (unmatched `)`); block-loop prologues now discard stray closers.
  If you add a loop, add the same guard.
- **Formatter-specific oracle (for this work order):** lossless round-trip —
  `render(parse(src)) == src` byte-for-byte over *both corpora* is the gate
  for trivia preservation; for the typed AST, `parse(render(parse(src)))`
  must produce an identical tree (catches what the printer normalizes).
  Wire these as per-file checks with timeouts, same as the hang scan.
- Raw-field compat: rules still read `body_raw`/`raw_text`/statement `raw`.
  Trivia work touches the lexer — if you change token spans, the
  `body_raw` source-line slicing in `src/parser.rs` (`lower_choice`,
  `lower_function`) and every builtin's line-offset math must keep producing
  byte-identical strings, or the finding regression gate will tell you.
