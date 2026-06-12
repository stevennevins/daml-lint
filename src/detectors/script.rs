use crate::detector::{parse_severity, Detector, Finding, Severity};
use crate::ir::DamlModule;
use rhai::{Dynamic, Engine, Scope};
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

/// AST-based custom detector: a Rhai script loaded via --rules.
///
/// Modeled on solhint custom rules: the script declares metadata as constants
/// and subscribes to AST node types by defining visitor functions. Each
/// visitor receives the node as a map mirroring the IR (src/ir.rs), with a
/// `span` carrying line/column. Findings are reported with `report(node, msg)`
/// or `report(line, msg)`.
///
/// const NAME = "no-foo-template";
/// const SEVERITY = "medium";
/// const DESCRIPTION = "Templates cannot be named Foo";   // optional
///
/// fn on_template(template) {
///     if template.name == "Foo" {
///         report(template, "Templates cannot be named Foo");
///     }
/// }
///
/// Visitors: on_template(template), on_choice(choice [, template]),
/// on_field(field [, template]), on_function(function), on_import(import),
/// and check(module) for whole-module logic.
const VISITORS: &[&str] = &[
    "on_template",
    "on_choice",
    "on_field",
    "on_function",
    "on_import",
    "check",
];

pub struct ScriptDetector {
    name: String,
    severity: Severity,
    description: String,
    source: String,
    path: String,
}

fn new_engine() -> Engine {
    let mut engine = Engine::new();
    // Debug builds halve rhai's default expression-depth limits, rejecting
    // ordinary nested rules — lift the limit, scripts are trusted code.
    engine.set_max_expr_depths(0, 0);
    // ...but a rule with an infinite loop must not hang CI forever.
    engine.set_max_operations(10_000_000);
    // IR node maps always contain every key, so a missing property is a typo —
    // fail loud instead of silently evaluating to ().
    engine.set_fail_on_invalid_map_property(true);
    engine
}

pub fn load_script(path: &Path) -> Result<Box<dyn Detector>, String> {
    let source = std::fs::read_to_string(path)
        .map_err(|e| format!("could not read rules script {}: {}", path.display(), e))?;

    let engine = new_engine();
    let ast = engine
        .compile(&source)
        .map_err(|e| format!("invalid rules script {}: {}", path.display(), e))?;

    // Run the top level to populate the constants.
    let mut scope = Scope::new();
    engine
        .run_ast_with_scope(&mut scope, &ast)
        .map_err(|e| format!("rules script {} failed: {}", path.display(), e))?;

    let name: String = scope
        .get_value("NAME")
        .ok_or_else(|| format!("rules script {}: missing `const NAME = \"...\"`", path.display()))?;
    let severity_str: String = scope.get_value("SEVERITY").ok_or_else(|| {
        format!("rules script {}: missing `const SEVERITY = \"...\"`", path.display())
    })?;
    let severity = parse_severity(&severity_str).ok_or_else(|| {
        format!(
            "rule '{}': unknown severity '{}'. Use critical, high, medium, low, or info.",
            name, severity_str
        )
    })?;
    let description: String = scope.get_value("DESCRIPTION").unwrap_or_default();

    let has_visitor = ast
        .iter_functions()
        .any(|f| VISITORS.contains(&f.name));
    if !has_visitor {
        return Err(format!(
            "rule '{}': script defines none of the visitor functions ({})",
            name,
            VISITORS.join(", ")
        ));
    }
    // A visitor with the wrong arity would silently never be called.
    for f in ast.iter_functions() {
        if !VISITORS.contains(&f.name) {
            continue;
        }
        let two_arg_ok = matches!(f.name, "on_choice" | "on_field");
        if f.params.len() != 1 && !(two_arg_ok && f.params.len() == 2) {
            return Err(format!(
                "rule '{}': {} takes {} parameter(s) — expected 1{}",
                name,
                f.name,
                f.params.len(),
                if two_arg_ok { " or 2" } else { "" }
            ));
        }
    }

    Ok(Box::new(ScriptDetector {
        name,
        severity,
        description,
        source,
        path: path.display().to_string(),
    }))
}

/// (line, column, message) reported by the script.
type Reported = Rc<RefCell<Vec<(usize, usize, String)>>>;

impl ScriptDetector {
    fn run(&self, module: &DamlModule) -> Result<Vec<Finding>, String> {
        let reported: Reported = Rc::new(RefCell::new(Vec::new()));

        let mut engine = new_engine();
        let sink = reported.clone();
        engine.register_fn("report", move |node: rhai::Map, message: &str| {
            let (line, column) = span_of(&node);
            sink.borrow_mut().push((line, column, message.to_string()));
        });
        let sink = reported.clone();
        engine.register_fn("report", move |line: i64, message: &str| {
            sink.borrow_mut().push((line.max(1) as usize, 1, message.to_string()));
        });

        let ast = engine
            .compile(&self.source)
            .map_err(|e| format!("rule '{}': {}", self.name, e))?;
        let mut scope = Scope::new();
        engine
            .run_ast_with_scope(&mut scope, &ast)
            .map_err(|e| format!("rule '{}': {}", self.name, e))?;

        // Which visitors exist, by (name, arity)
        let arities: Vec<(String, usize)> = ast
            .iter_functions()
            .map(|f| (f.name.to_string(), f.params.len()))
            .collect();
        let has = |name: &str, arity: usize| arities.iter().any(|(n, a)| n == name && *a == arity);
        let call = |scope: &mut Scope, name: &str, args: Vec<Dynamic>| -> Result<(), String> {
            // eval_ast(false): the top level already ran once above — without
            // this, call_fn re-evaluates it before every visitor call.
            let options = rhai::CallFnOptions::new().eval_ast(false);
            engine
                .call_fn_with_options::<Dynamic>(options, scope, &ast, name, args)
                .map(|_| ())
                .map_err(|e| format!("rule '{}': {} failed: {}", self.name, name, e))
        };

        for template in &module.templates {
            let t_dyn = rhai::serde::to_dynamic(template).map_err(|e| e.to_string())?;
            if has("on_template", 1) {
                call(&mut scope, "on_template", vec![t_dyn.clone()])?;
            }
            for choice in &template.choices {
                let c_dyn = rhai::serde::to_dynamic(choice).map_err(|e| e.to_string())?;
                if has("on_choice", 2) {
                    call(&mut scope, "on_choice", vec![c_dyn.clone(), t_dyn.clone()])?;
                } else if has("on_choice", 1) {
                    call(&mut scope, "on_choice", vec![c_dyn])?;
                }
            }
            for field in &template.fields {
                let f_dyn = rhai::serde::to_dynamic(field).map_err(|e| e.to_string())?;
                if has("on_field", 2) {
                    call(&mut scope, "on_field", vec![f_dyn.clone(), t_dyn.clone()])?;
                } else if has("on_field", 1) {
                    call(&mut scope, "on_field", vec![f_dyn])?;
                }
            }
        }
        if has("on_function", 1) {
            for function in &module.functions {
                let f_dyn = rhai::serde::to_dynamic(function).map_err(|e| e.to_string())?;
                call(&mut scope, "on_function", vec![f_dyn])?;
            }
        }
        if has("on_import", 1) {
            for import in &module.imports {
                let i_dyn = rhai::serde::to_dynamic(import).map_err(|e| e.to_string())?;
                call(&mut scope, "on_import", vec![i_dyn])?;
            }
        }
        if has("check", 1) {
            let m_dyn = rhai::serde::to_dynamic(module).map_err(|e| e.to_string())?;
            call(&mut scope, "check", vec![m_dyn])?;
        }

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

fn span_of(node: &rhai::Map) -> (usize, usize) {
    if let Some(span) = node.get("span").and_then(|s| s.read_lock::<rhai::Map>()) {
        let line = span.get("line").and_then(|v| v.as_int().ok()).unwrap_or(1);
        let column = span.get("column").and_then(|v| v.as_int().ok()).unwrap_or(1);
        (line.max(1) as usize, column.max(1) as usize)
    } else {
        (1, 1)
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
            "daml-lint-test-{}-{}.rhai",
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

fn on_template(template) {
    if template.ensure_clause == () {
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

fn on_choice(choice, template) {
    if !choice.consuming {
        return;
    }
    for controller in choice.controllers {
        if controller in template.signatories {
            return;
        }
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

fn check(m) {
    if m.templates.len() > 0 {
        report(1, `Module '${m.name}' has templates`);
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
    fn test_missing_name_rejected() {
        let result = load_script_from_str(
            "no-name",
            r#"
const SEVERITY = "low";
fn on_template(t) {}
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
    fn test_wrong_arity_visitor_rejected() {
        let result = load_script_from_str(
            "arity",
            r#"
const NAME = "x";
const SEVERITY = "low";
fn on_choice(c, t, extra) {}
"#,
        );
        match result {
            Err(e) => assert!(e.contains("expected 1 or 2")),
            Ok(_) => panic!("3-parameter visitor should be rejected, not silently ignored"),
        }
    }

    #[test]
    fn test_nested_expressions_compile() {
        // Debug builds halve rhai's expression-depth limits; this README-shaped
        // rule must compile in both profiles.
        let det = load_script_from_str(
            "nested",
            r#"
const NAME = "nested";
const SEVERITY = "low";

fn on_choice(choice) {
    for stmt in choice.body {
        if stmt.contains("Create") {
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
    fn test_syntax_error_rejected() {
        let result = load_script_from_str("syntax-err", "fn on_template(t) {");
        assert!(result.is_err());
    }

    #[test]
    fn test_demo_scripts_load() {
        assert!(load_script(Path::new("examples/template-requires-ensure.rhai")).is_ok());
        assert!(
            load_script(Path::new("examples/consuming-choice-signatory-controller.rhai")).is_ok()
        );
        assert!(load_script(Path::new("examples/no-trace.rhai")).is_ok());
    }
}
