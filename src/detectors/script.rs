use crate::detector::{parse_severity, Detector, Finding, Severity};
use crate::ir::DamlModule;
use rquickjs::{CatchResultExt, Context, Ctx, Function, Object, Runtime, Value};
use std::cell::RefCell;
use std::rc::Rc;
use std::path::Path;

/// AST-based custom detector: a JavaScript rule loaded via --rules.
///
/// Modeled on solhint custom rules: the script declares metadata constants
/// and subscribes to AST node types by defining visitor functions. Each
/// visitor receives the node as an object mirroring the IR (src/ir.rs), with
/// a `span` carrying line/column. Findings are reported with
/// `report(node, msg)` or `report(line, msg)`.
///
/// const NAME = "no-foo-template";
/// const SEVERITY = "medium";
/// const DESCRIPTION = "Templates cannot be named Foo";   // optional
///
/// function on_template(template) {
///     if (template.name === "Foo") {
///         report(template, "Templates cannot be named Foo");
///     }
/// }
///
/// Visitors: on_template(template), on_choice(choice, template),
/// on_field(field, template), on_function(function), on_import(import),
/// and check(module) for whole-module logic. Visitors must be `function`
/// declarations (arrow functions assigned to const are not discovered).
const VISITORS: &[&str] = &[
    "on_template",
    "on_choice",
    "on_field",
    "on_function",
    "on_import",
    "on_interface",
    "check",
];

/// Interrupt-handler invocations before a script is killed. QuickJS calls the
/// handler periodically during execution; a runaway loop must not hang CI.
const MAX_INTERRUPT_CHECKS: u64 = 100_000;

pub struct ScriptDetector {
    name: String,
    severity: Severity,
    description: String,
    path: String,
    /// One runtime+context per rule, with the script evaluated once at load
    /// time and reused across modules (a fresh QuickJS runtime + re-eval per
    /// file made large scans QuickJS-bound, not parser-bound). Visitor
    /// functions are stateless by contract; the per-module `report` sink is
    /// swapped in before each run.
    _runtime: Runtime,
    context: Context,
    /// Interrupt-check counter, reset per module.
    interrupt_count: Rc<std::cell::Cell<u64>>,
}

fn new_runtime() -> Result<(Runtime, Rc<std::cell::Cell<u64>>), String> {
    let rt = Runtime::new().map_err(|e| e.to_string())?;
    let count = Rc::new(std::cell::Cell::new(0u64));
    let handler_count = count.clone();
    rt.set_interrupt_handler(Some(Box::new(move || {
        handler_count.set(handler_count.get() + 1);
        handler_count.get() > MAX_INTERRUPT_CHECKS
    })));
    Ok((rt, count))
}

/// Read a top-level string constant. `const` bindings are lexical, not
/// globalThis properties, so they're read by evaluating an expression.
fn read_const(ctx: &Ctx, name: &str) -> Option<String> {
    ctx.eval::<Option<String>, _>(format!("typeof {n} === 'string' ? {n} : null", n = name))
        .ok()
        .flatten()
}

fn invoke<'js, A: rquickjs::function::IntoArgs<'js>>(
    ctx: &Ctx<'js>,
    rule: &str,
    f: &Function<'js>,
    visitor: &str,
    args: A,
) -> Result<(), String> {
    f.call::<_, ()>(args)
        .catch(ctx)
        .map_err(|e| format!("rule '{}': {} failed: {}", rule, visitor, e))
}

fn parse_node<'js>(ctx: &Ctx<'js>, rule: &str, json: String) -> Result<Value<'js>, String> {
    ctx.json_parse(json)
        .catch(ctx)
        .map_err(|e| format!("rule '{}': {}", rule, e))
}

pub fn load_script(path: &Path) -> Result<Box<dyn Detector>, String> {
    let source = std::fs::read_to_string(path)
        .map_err(|e| format!("could not read rules script {}: {}", path.display(), e))?;

    let (rt, interrupt_count) = new_runtime()?;
    let context = Context::full(&rt).map_err(|e| e.to_string())?;
    let loaded = context.with(|ctx| {
        // report() must exist at load time so top-level code referencing it parses.
        register_report(&ctx, Rc::new(RefCell::new(Vec::new())))?;
        ctx.eval::<(), _>(source.as_bytes())
            .catch(&ctx)
            .map_err(|e| format!("invalid rules script {}: {}", path.display(), e))?;

        let name = read_const(&ctx, "NAME").ok_or_else(|| {
            format!("rules script {}: missing `const NAME = \"...\"`", path.display())
        })?;
        let severity_str = read_const(&ctx, "SEVERITY").ok_or_else(|| {
            format!("rules script {}: missing `const SEVERITY = \"...\"`", path.display())
        })?;
        let severity = parse_severity(&severity_str).ok_or_else(|| {
            format!(
                "rule '{}': unknown severity '{}'. Use critical, high, medium, low, or info.",
                name, severity_str
            )
        })?;
        let description = read_const(&ctx, "DESCRIPTION").unwrap_or_default();

        let globals = ctx.globals();
        let has_visitor = VISITORS
            .iter()
            .any(|v| globals.get::<_, Function>(*v).is_ok());
        if !has_visitor {
            return Err(format!(
                "rule '{}': script defines none of the visitor functions ({})",
                name,
                VISITORS.join(", ")
            ));
        }

        Ok((name, severity, description))
    });
    let (name, severity, description) = loaded?;
    let _ = source;
    Ok(Box::new(ScriptDetector {
        name,
        severity,
        description,
        path: path.display().to_string(),
        _runtime: rt,
        context,
        interrupt_count,
    }) as Box<dyn Detector>)
}

/// (line, column, message) reported by the script.
type Reported = Rc<RefCell<Vec<(usize, usize, String)>>>;

fn json<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string(v).expect("IR types always serialize")
}

fn register_report(ctx: &Ctx, sink: Reported) -> Result<(), String> {
    let report = Function::new(ctx.clone(), move |arg: Value, message: String| {
        let (line, column) = location_of(&arg);
        sink.borrow_mut().push((line, column, message));
    })
    .map_err(|e| e.to_string())?;
    ctx.globals()
        .set("report", report)
        .map_err(|e| e.to_string())
}

/// First argument of report(): a node object (location from its span) or a
/// line number.
fn location_of(arg: &Value) -> (usize, usize) {
    if let Some(line) = arg.as_number() {
        return ((line as i64).max(1) as usize, 1);
    }
    if let Some(obj) = arg.as_object() {
        if let Ok(span) = obj.get::<_, Object>("span") {
            let line: i64 = span.get("line").unwrap_or(1);
            let column: i64 = span.get("column").unwrap_or(1);
            return (line.max(1) as usize, column.max(1) as usize);
        }
    }
    (1, 1)
}

impl ScriptDetector {
    fn run(&self, module: &DamlModule) -> Result<Vec<Finding>, String> {
        let reported: Reported = Rc::new(RefCell::new(Vec::new()));

        self.interrupt_count.set(0);
        self.context.with(|ctx| -> Result<(), String> {
            // Fresh sink per module; replaces the previous report binding.
            register_report(&ctx, reported.clone())?;

            let globals = ctx.globals();
            let visitor = |name: &str| globals.get::<_, Function>(name).ok();
            let rule = self.name.as_str();

            for template in &module.templates {
                let t_json = json(template);
                if let Some(f) = visitor("on_template") {
                    let t = parse_node(&ctx, rule, t_json.clone())?;
                    invoke(&ctx, rule, &f, "on_template", (t,))?;
                }
                if let Some(f) = visitor("on_choice") {
                    for choice in &template.choices {
                        let c = parse_node(&ctx, rule, json(choice))?;
                        let t = parse_node(&ctx, rule, t_json.clone())?;
                        invoke(&ctx, rule, &f, "on_choice", (c, t))?;
                    }
                }
                if let Some(f) = visitor("on_field") {
                    for field in &template.fields {
                        let fd = parse_node(&ctx, rule, json(field))?;
                        let t = parse_node(&ctx, rule, t_json.clone())?;
                        invoke(&ctx, rule, &f, "on_field", (fd, t))?;
                    }
                }
            }
            if let Some(f) = visitor("on_function") {
                for function in &module.functions {
                    let fun = parse_node(&ctx, rule, json(function))?;
                    invoke(&ctx, rule, &f, "on_function", (fun,))?;
                }
            }
            if let Some(f) = visitor("on_interface") {
                for interface in &module.interfaces {
                    let i = parse_node(&ctx, rule, json(interface))?;
                    invoke(&ctx, rule, &f, "on_interface", (i,))?;
                }
            }
            if let Some(f) = visitor("on_import") {
                for import in &module.imports {
                    let i = parse_node(&ctx, rule, json(import))?;
                    invoke(&ctx, rule, &f, "on_import", (i,))?;
                }
            }
            if let Some(f) = visitor("check") {
                let m = parse_node(&ctx, rule, json(module))?;
                invoke(&ctx, rule, &f, "check", (m,))?;
            }
            Ok(())
        })?;

        let findings = reported
            .borrow()
            .iter()
            .map(|(line, column, message)| Finding {
                detector: self.name.clone(),
                severity: self.severity,
                file: module.file.clone(),
                line: *line,
                column: *column,
                message: message.clone(),
                evidence: module
                    .source
                    .lines()
                    .nth(line.saturating_sub(1))
                    .unwrap_or("")
                    .trim()
                    .to_string(),
            })
            .collect();
        Ok(findings)
    }
}

impl Detector for ScriptDetector {
    fn name(&self) -> &str {
        &self.name
    }

    fn severity(&self) -> Severity {
        self.severity
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn detect(&self, module: &DamlModule) -> Vec<Finding> {
        // Detector::detect can't return errors; a script that fails at runtime
        // is a broken rule and the scan results can't be trusted — fail loud.
        self.run(module).unwrap_or_else(|e| {
            eprintln!("Error: rules script {}: {}", self.path, e);
            std::process::exit(2);
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_daml;
    use std::path::Path;

    fn load_script_from_str(label: &str, script: &str) -> Result<Box<dyn Detector>, String> {
        let path = std::env::temp_dir().join(format!(
            "daml-lint-test-{}-{}.js",
            label,
            std::process::id()
        ));
        std::fs::write(&path, script).unwrap();
        let result = load_script(&path);
        std::fs::remove_file(&path).ok();
        result
    }

    const TEMPLATE_NO_ENSURE: &str = r#"module Test where

template Iou
  with
    issuer : Party
    owner : Party
    amount : Decimal
  where
    signatory issuer
    observer owner

    choice Transfer : ()
      controller owner
      do
        pure ()
"#;

    #[test]
    fn test_on_template_visitor_reports() {
        let det = load_script_from_str(
            "on-template",
            r#"
const NAME = "template-requires-ensure";
const SEVERITY = "medium";

function on_template(template) {
    if (template.ensure_clause === null) {
        report(template, `Template '${template.name}' has no ensure clause`);
    }
}
"#,
        )
        .unwrap();
        let module = parse_daml(TEMPLATE_NO_ENSURE, Path::new("Test.daml"));
        let findings = det.detect(&module);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].detector, "template-requires-ensure");
        assert!(findings[0].message.contains("Iou"));
    }

    #[test]
    fn test_on_choice_visitor_gets_template_context() {
        let det = load_script_from_str(
            "on-choice",
            r#"
const NAME = "consuming-choice-signatory-controller";
const SEVERITY = "medium";

function on_choice(choice, template) {
    if (!choice.consuming) {
        return;
    }
    if (choice.controllers.some(c => template.signatories.includes(c))) {
        return;
    }
    report(choice, `Consuming choice '${choice.name}' has no signatory controller`);
}
"#,
        )
        .unwrap();
        let module = parse_daml(TEMPLATE_NO_ENSURE, Path::new("Test.daml"));
        let findings = det.detect(&module);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].message.contains("Transfer"));
    }

    #[test]
    fn test_check_visitor_and_line_report() {
        let det = load_script_from_str(
            "check-module",
            r#"
const NAME = "max-one-template";
const SEVERITY = "low";

function check(module) {
    if (module.templates.length > 0) {
        report(1, `Module '${module.name}' has templates`);
    }
}
"#,
        )
        .unwrap();
        let module = parse_daml(TEMPLATE_NO_ENSURE, Path::new("Test.daml"));
        let findings = det.detect(&module);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].line, 1);
    }

    #[test]
    fn test_statement_bodies_inspectable() {
        let det = load_script_from_str(
            "statements",
            r#"
const NAME = "no-create-in-choice";
const SEVERITY = "low";

function on_choice(choice) {
    for (const stmt of choice.body) {
        if ("Create" in stmt) {
            report(choice, `Choice '${choice.name}' creates contracts`);
        }
    }
}
"#,
        )
        .unwrap();
        let module = parse_daml(TEMPLATE_NO_ENSURE, Path::new("Test.daml"));
        det.detect(&module);
    }

    #[test]
    fn test_missing_name_rejected() {
        let result = load_script_from_str(
            "no-name",
            r#"
const SEVERITY = "low";
function on_template(t) {}
"#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_no_visitor_rejected() {
        let result = load_script_from_str(
            "no-visitor",
            r#"
const NAME = "x";
const SEVERITY = "low";
"#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_bad_severity_rejected() {
        let result = load_script_from_str(
            "bad-severity",
            r#"
const NAME = "x";
const SEVERITY = "banana";
function on_template(t) {}
"#,
        );
        match result {
            Err(e) => assert!(e.contains("banana")),
            Ok(_) => panic!("bad severity should be rejected"),
        }
    }

    #[test]
    fn test_syntax_error_rejected() {
        let result = load_script_from_str("syntax-err", "function on_template(t) {");
        assert!(result.is_err());
    }

    fn raw_detector(name: &str, source: &str) -> ScriptDetector {
        let (runtime, interrupt_count) = new_runtime().unwrap();
        let context = Context::full(&runtime).unwrap();
        context.with(|ctx| {
            register_report(&ctx, Rc::new(RefCell::new(Vec::new()))).unwrap();
            ctx.eval::<(), _>(source.as_bytes()).unwrap();
        });
        ScriptDetector {
            name: name.to_string(),
            severity: Severity::Low,
            description: String::new(),
            path: format!("{}.js", name),
            _runtime: runtime,
            context,
            interrupt_count,
        }
    }

    #[test]
    fn test_runtime_error_surfaces_rule_and_visitor() {
        let script = raw_detector(
            "boom",
            r#"function on_template(t) { t.does.not.exist; }"#,
        );
        let module = parse_daml(TEMPLATE_NO_ENSURE, Path::new("Test.daml"));
        let err = script.run(&module).unwrap_err();
        assert!(err.contains("boom"));
        assert!(err.contains("on_template"));
    }

    #[test]
    fn test_infinite_loop_interrupted() {
        let script = raw_detector(
            "spin",
            r#"
const NAME = "spin";
const SEVERITY = "low";
function on_template(t) { while (true) {} }
"#,
        );
        let module = parse_daml(TEMPLATE_NO_ENSURE, Path::new("Test.daml"));
        assert!(script.run(&module).is_err());
    }

    /// Interrupt counter resets between modules: a long (but finite) rule
    /// run on many modules must not trip the runaway-loop guard.
    #[test]
    fn test_interrupt_counter_resets_per_module() {
        let script = raw_detector(
            "busy",
            r#"
const NAME = "busy";
const SEVERITY = "low";
function on_template(t) { let x = 0; for (let i = 0; i < 200000; i++) { x += i; } }
"#,
        );
        let module = parse_daml(TEMPLATE_NO_ENSURE, Path::new("Test.daml"));
        for _ in 0..5 {
            assert!(script.run(&module).is_ok());
        }
    }

    /// Exercises every script-visible node kind: all scalar and parameterized
    /// field types, ensure clauses, choice parameters, nonconsuming choices,
    /// every Statement variant (with TryCatch recursion), qualified aliased
    /// imports, and top-level functions.
    #[test]
    fn test_every_node_kind_reaches_scripts() {
        let probe = r#"module Probe where

import qualified DA.Map as Map
import DA.Time

template Probe
  with
    owner : Party
    note : Text
    amount : Decimal
    count : Int
    active : Bool
    issued : Date
    stamp : Time
    tags : [Text]
    backup : Optional Party
    parent : ContractId Probe
    scores : TextMap Int
    extra : Custom
  where
    signatory owner
    ensure amount > 0.0

    choice Reissue : ContractId Probe
      with
        newOwner : Party
      controller owner
      do
        let total = amount + 1.0
        assert (total > 0.0)
        p <- fetch parent
        archive parent
        cid <- create this with owner = newOwner
        result <- exercise cid Noop
        try do
          pure ()
        catch
          (e : AnyException) -> pure ()
        pure cid

    nonconsuming choice Noop : ()
      observer owner
      controller owner
      do
        pure ()

    key (owner, note) : (Party, Text)
    maintainer key._1

    interface instance Probeable for Probe where
      view = ProbeView owner

interface Probeable where
  viewtype ProbeView
  getProbeOwner : Party

  nonconsuming choice GetProbeView : ProbeView
    with
      viewer : Party
    controller viewer
    do
      pure (view this)

helper : Int -> Int
helper x = x + 1

describe owner xs total parts = do
  let scaled = map (\y -> y * 2) xs
  let pair = (total / parts, [total, parts])
  let label = if total > 0.0 then "pos" else show (-total)
  let picked = case xs of
        [] -> None
        h :: _ -> Some h
  let result = let inner = Map.Map in inner
  pure (FooCon with field1 = label)
"#;
        let det = load_script_from_str(
            "census",
            r#"
const NAME = "node-census";
const SEVERITY = "info";

function exprKinds(e, seen) {
  if (e === null || typeof e !== "object") return;
  const k = Object.keys(e)[0];
  seen.add("Expr:" + k);
  const p = e[k];
  for (const key of Object.keys(p)) {
    const v = p[key];
    if (key === "span") continue;
    if (Array.isArray(v)) {
      for (const item of v) {
        if (item && typeof item === "object") {
          if ("body" in item && "pattern" in item) exprKinds(item.body, seen);
          else if ("value" in item && "name" in item) {
            if (item.value !== null) exprKinds(item.value, seen);
          } else if (Object.keys(item).length === 1) {
            if (k === "DoBlock") stmtKinds([item], seen);
            else exprKinds(item, seen);
          }
        }
      }
    } else if (v && typeof v === "object") {
      exprKinds(v, seen);
    }
  }
}

function stmtKinds(stmts, seen) {
  for (const s of stmts) {
    const k = Object.keys(s)[0];
    seen.add(k);
    const p = s[k];
    if (p.binder !== undefined && p.binder !== null) seen.add("Binder");
    if (k === "TryCatch") {
      stmtKinds(p.try_body, seen);
      stmtKinds(p.catch_body, seen);
    }
    if (p.value) exprKinds(p.value, seen);
    if (p.condition_expr) exprKinds(p.condition_expr, seen);
    if (p.cid) exprKinds(p.cid, seen);
    if (p.argument) exprKinds(p.argument, seen);
    if (p.expr && typeof p.expr === "object") exprKinds(p.expr, seen);
  }
}

function check(m) {
  const seen = new Set();
  for (const t of m.templates) {
    if (t.ensure_clause !== null) {
      seen.add("Ensure");
      exprKinds(t.ensure_clause.expr, seen);
    }
    if (t.key_expr !== null) seen.add("KeyExpr");
    if (t.key_type !== null) seen.add("KeyType");
    if (t.maintainer_exprs.length > 0) seen.add("Maintainer");
    if (t.signatory_exprs.length > 0) seen.add("SignatoryExpr");
    if (t.interface_instances.length > 0) {
      seen.add("InterfaceInstance");
      if (t.interface_instances[0].methods.length > 0) seen.add("InstanceMethod");
    }
    for (const f of t.fields) {
      if (typeof f.type_ === "string") seen.add("Scalar:" + f.type_);
      else seen.add("Param:" + Object.keys(f.type_)[0]);
    }
    for (const c of t.choices) {
      if (c.parameters.length > 0) seen.add("ChoiceParams");
      if (!c.consuming) seen.add("Nonconsuming");
      if (c.controller_exprs.length > 0) seen.add("ControllerExpr");
      if (c.observer_exprs.length > 0) seen.add("ChoiceObserver");
      stmtKinds(c.body, seen);
    }
  }
  for (const i of m.interfaces) {
    seen.add("Interface");
    if (i.viewtype !== null) seen.add("Viewtype");
    if (i.methods.length > 0) seen.add("InterfaceMethod");
    if (i.choices.length > 0) seen.add("InterfaceChoice");
  }
  for (const i of m.imports) {
    if (i.qualified && i.alias !== null) seen.add("QualifiedAlias");
  }
  for (const fn of m.functions) {
    seen.add("Function");
    if (fn.type_signature !== null) seen.add("TypeSignature");
    stmtKinds(fn.body, seen);
  }
  for (const k of Array.from(seen).sort()) report(1, k);
}
"#,
        )
        .unwrap();
        let module = parse_daml(probe, Path::new("Probe.daml"));
        let seen: Vec<String> = det.detect(&module).into_iter().map(|f| f.message).collect();

        for expected in [
            "Scalar:Party",
            "Scalar:Text",
            "Scalar:Decimal",
            "Scalar:Int",
            "Scalar:Bool",
            "Scalar:Date",
            "Scalar:Time",
            "Param:List",
            "Param:Optional",
            "Param:ContractId",
            "Param:TextMap",
            "Param:Named",
            "Ensure",
            "ChoiceParams",
            "Nonconsuming",
            "Let",
            "Assert",
            "Fetch",
            "Archive",
            "Create",
            "Exercise",
            "TryCatch",
            "QualifiedAlias",
            "Function",
            // v2 structured surface
            "Binder",
            "KeyExpr",
            "KeyType",
            "Maintainer",
            "SignatoryExpr",
            "ControllerExpr",
            "ChoiceObserver",
            "InterfaceInstance",
            "InstanceMethod",
            "Interface",
            "Viewtype",
            "InterfaceMethod",
            "InterfaceChoice",
            "TypeSignature",
            "Expr:Var",
            "Expr:Con",
            "Expr:Lit",
            "Expr:App",
            "Expr:BinOp",
            "Expr:Neg",
            "Expr:Lambda",
            "Expr:If",
            "Expr:Case",
            "Expr:LetIn",
            "Expr:Record",
            "Expr:Tuple",
            "Expr:List",
        ] {
            assert!(
                seen.iter().any(|m| m == expected),
                "node kind '{}' did not reach the script; saw: {:?}",
                expected,
                seen
            );
        }
    }

    #[test]
    fn test_demo_scripts_load() {
        assert!(load_script(Path::new("examples/template-requires-ensure.js")).is_ok());
        assert!(
            load_script(Path::new("examples/consuming-choice-signatory-controller.js")).is_ok()
        );
        assert!(load_script(Path::new("examples/no-trace.js")).is_ok());
        assert!(load_script(Path::new("examples/no-create-in-nonconsuming.js")).is_ok());
        assert!(load_script(Path::new("examples/no-bare-contractid-field.js")).is_ok());
        assert!(load_script(Path::new("examples/unqualified-da-import.js")).is_ok());
        assert!(load_script(Path::new("examples/function-ledger-actions.js")).is_ok());
        assert!(load_script(Path::new("examples/choice-param-shadows-field.js")).is_ok());
    }
}
