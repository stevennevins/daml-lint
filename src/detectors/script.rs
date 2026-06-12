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
    "check",
];

/// Interrupt-handler invocations before a script is killed. QuickJS calls the
/// handler periodically during execution; a runaway loop must not hang CI.
const MAX_INTERRUPT_CHECKS: u64 = 100_000;

pub struct ScriptDetector {
    name: String,
    severity: Severity,
    description: String,
    source: String,
    path: String,
}

fn new_runtime() -> Result<Runtime, String> {
    let rt = Runtime::new().map_err(|e| e.to_string())?;
    let count = std::cell::Cell::new(0u64);
    rt.set_interrupt_handler(Some(Box::new(move || {
        count.set(count.get() + 1);
        count.get() > MAX_INTERRUPT_CHECKS
    })));
    Ok(rt)
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

    let rt = new_runtime()?;
    let context = Context::full(&rt).map_err(|e| e.to_string())?;
    context.with(|ctx| {
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

        Ok(Box::new(ScriptDetector {
            name,
            severity,
            description,
            source,
            path: path.display().to_string(),
        }) as Box<dyn Detector>)
    })
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

        let rt = new_runtime()?;
        let context = Context::full(&rt).map_err(|e| e.to_string())?;
        context.with(|ctx| -> Result<(), String> {
            register_report(&ctx, reported.clone())?;
            ctx.eval::<(), _>(self.source.as_bytes())
                .catch(&ctx)
                .map_err(|e| format!("rule '{}': {}", self.name, e))?;

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

    #[test]
    fn test_runtime_error_surfaces_rule_and_visitor() {
        let script = ScriptDetector {
            name: "boom".to_string(),
            severity: Severity::Low,
            description: String::new(),
            source: r#"function on_template(t) { t.does.not.exist; }"#.to_string(),
            path: "test.js".to_string(),
        };
        let module = parse_daml(TEMPLATE_NO_ENSURE, Path::new("Test.daml"));
        let err = script.run(&module).unwrap_err();
        assert!(err.contains("boom"));
        assert!(err.contains("on_template"));
    }

    #[test]
    fn test_infinite_loop_interrupted() {
        let script = ScriptDetector {
            name: "spin".to_string(),
            severity: Severity::Low,
            description: String::new(),
            source: r#"
const NAME = "spin";
const SEVERITY = "low";
function on_template(t) { while (true) {} }
"#
            .to_string(),
            path: "spin.js".to_string(),
        };
        let module = parse_daml(TEMPLATE_NO_ENSURE, Path::new("Test.daml"));
        assert!(script.run(&module).is_err());
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
      controller owner
      do
        pure ()

helper x = x + 1
"#;
        let det = load_script_from_str(
            "census",
            r#"
const NAME = "node-census";
const SEVERITY = "info";

function stmtKinds(stmts, seen) {
  for (const s of stmts) {
    const k = Object.keys(s)[0];
    seen.add(k);
    if (k === "TryCatch") {
      stmtKinds(s.TryCatch.try_body, seen);
      stmtKinds(s.TryCatch.catch_body, seen);
    }
  }
}

function check(m) {
  const seen = new Set();
  for (const t of m.templates) {
    if (t.ensure_clause !== null) seen.add("Ensure");
    for (const f of t.fields) {
      if (typeof f.type_ === "string") seen.add("Scalar:" + f.type_);
      else seen.add("Param:" + Object.keys(f.type_)[0]);
    }
    for (const c of t.choices) {
      if (c.parameters.length > 0) seen.add("ChoiceParams");
      if (!c.consuming) seen.add("Nonconsuming");
      stmtKinds(c.body, seen);
    }
  }
  for (const i of m.imports) {
    if (i.qualified && i.alias !== null) seen.add("QualifiedAlias");
  }
  if (m.functions.length > 0) seen.add("Function");
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
    }
}
