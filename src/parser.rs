//! Lowering: typed AST (src/ast.rs, built by src/parse.rs) → rule-facing IR
//! (src/ir.rs).
//!
//! This replaces the old line-based keyword shim. The IR shapes are the
//! stable contract with rule scripts; raw-text fields (`body_raw`,
//! `raw_text`, statement `raw`) are reconstructed from real parse trees so
//! existing rules keep working, while structured `Expr` payloads carry the
//! actual parse tree.

use crate::ast::{self, Consuming, Decl, DoStmt, TemplateBodyDecl};
use crate::ir::*;
use crate::parse::parse_module;
use std::path::Path;

/// Parse a DAML source file into a DamlModule IR. Never panics; parse
/// problems degrade to partial structure.
pub fn parse_daml(source: &str, file: &Path) -> DamlModule {
    parse_daml_with_diagnostics(source, file).0
}

/// (line, column, message) diagnostics for the caller to report.
pub type Diagnostic = (usize, usize, String);

pub fn parse_daml_with_diagnostics(source: &str, file: &Path) -> (DamlModule, Vec<Diagnostic>) {
    let (module, diags) = parse_module(source);
    let lines: Vec<&str> = source.lines().collect();

    let imports = module
        .imports
        .iter()
        .map(|i| Import {
            module_name: i.module_name.clone(),
            qualified: i.qualified,
            alias: i.alias.clone(),
            span: span_at(file, i.pos),
        })
        .collect();

    let mut templates = Vec::new();
    let mut interfaces = Vec::new();
    let mut functions = Vec::new();

    for decl in &module.decls {
        match decl {
            Decl::Template(t) => templates.push(lower_template(t, file, &lines)),
            Decl::Interface(i) => interfaces.push(lower_interface(i, file, &lines)),
            Decl::Function(f) => {
                if f.equations.is_empty() {
                    continue; // type signature without a body
                }
                functions.push(lower_function(f, file, &lines));
            }
            _ => {}
        }
    }

    let ir = DamlModule {
        name: module.name,
        file: file.to_path_buf(),
        source: source.to_string(),
        imports,
        templates,
        interfaces,
        functions,
    };
    let diags = diags
        .into_iter()
        .map(|d| (d.pos.line, d.pos.column, d.message))
        .collect();
    (ir, diags)
}

fn span_at(file: &Path, pos: ast::Pos) -> Span {
    Span {
        file: file.to_path_buf(),
        line: pos.line,
        column: pos.column,
    }
}

fn src_pos(pos: ast::Pos) -> SrcPos {
    SrcPos {
        line: pos.line,
        column: pos.column,
    }
}

// ----- expressions -------------------------------------------------------

fn lower_expr(e: &ast::Expr) -> Expr {
    let span = src_pos(e.pos());
    match e {
        ast::Expr::Var {
            qualifier, name, ..
        } => Expr::Var {
            name: name.clone(),
            qualifier: qualifier.clone(),
            span,
        },
        ast::Expr::Con {
            qualifier, name, ..
        } => Expr::Con {
            name: name.clone(),
            qualifier: qualifier.clone(),
            span,
        },
        ast::Expr::Lit { kind, text, .. } => Expr::Lit {
            kind: match kind {
                ast::LitKind::Int => "Int",
                ast::LitKind::Decimal => "Decimal",
                ast::LitKind::Text => "Text",
                ast::LitKind::Char => "Char",
            }
            .to_string(),
            value: text.clone(),
            span,
        },
        ast::Expr::App { func, args, .. } => Expr::App {
            func: Box::new(lower_expr(func)),
            args: args.iter().map(lower_expr).collect(),
            span,
        },
        ast::Expr::BinOp { op, lhs, rhs, .. } => Expr::BinOp {
            op: op.clone(),
            lhs: Box::new(lower_expr(lhs)),
            rhs: Box::new(lower_expr(rhs)),
            span,
        },
        ast::Expr::Neg { expr, .. } => Expr::Neg {
            expr: Box::new(lower_expr(expr)),
            span,
        },
        ast::Expr::Lambda { params, body, .. } => Expr::Lambda {
            params: params.iter().map(|p| p.render()).collect(),
            body: Box::new(lower_expr(body)),
            span,
        },
        ast::Expr::If {
            cond,
            then_branch,
            else_branch,
            ..
        } => Expr::If {
            cond: Box::new(lower_expr(cond)),
            then_branch: Box::new(lower_expr(then_branch)),
            else_branch: Box::new(lower_expr(else_branch)),
            span,
        },
        ast::Expr::Case {
            scrutinee, alts, ..
        } => Expr::Case {
            scrutinee: Box::new(lower_expr(scrutinee)),
            alts: alts
                .iter()
                .map(|a| CaseAlt {
                    pattern: a.pat.render(),
                    body: lower_expr(&a.body),
                })
                .collect(),
            span,
        },
        ast::Expr::Do { stmts, .. } => Expr::DoBlock {
            statements: lower_do(stmts),
            span,
        },
        ast::Expr::LetIn { bindings, body, .. } => Expr::LetIn {
            bindings: bindings
                .iter()
                .map(|b| LetBinding {
                    name: binding_name(b),
                    value: lower_expr(&b.expr),
                })
                .collect(),
            body: Box::new(lower_expr(body)),
            span,
        },
        ast::Expr::Record { base, fields, .. } => Expr::Record {
            base: Box::new(lower_expr(base)),
            fields: fields
                .iter()
                .map(|f| RecordField {
                    name: f.name.clone(),
                    value: f.value.as_ref().map(lower_expr),
                })
                .collect(),
            span,
        },
        ast::Expr::Tuple { items, .. } => Expr::Tuple {
            items: items.iter().map(lower_expr).collect(),
            span,
        },
        ast::Expr::List { items, .. } => Expr::List {
            items: items.iter().map(lower_expr).collect(),
            span,
        },
        // No structured rule-facing encoding (yet): sections, try-in-
        // expression-position, recovered parse errors.
        ast::Expr::Try { .. } | ast::Expr::Section { .. } | ast::Expr::Error { .. } => {
            Expr::Unknown {
                raw: e.render(),
                span,
            }
        }
    }
}

fn binding_name(b: &ast::Binding) -> String {
    let mut name = b.pat.render();
    for p in &b.params {
        name.push(' ');
        name.push_str(&p.render());
    }
    name
}

// ----- declarations ------------------------------------------------------

fn lower_template(t: &ast::TemplateDecl, file: &Path, lines: &[&str]) -> Template {
    let fields = t
        .fields
        .iter()
        .map(|f| Field {
            name: f.name.clone(),
            type_: DamlType::from_str(&f.type_text),
            span: span_at(file, f.pos),
        })
        .collect();

    let mut signatories = Vec::new();
    let mut observers = Vec::new();
    let mut signatory_exprs = Vec::new();
    let mut observer_exprs = Vec::new();
    let mut ensure_clause = None;
    let mut key_expr = None;
    let mut key_type = None;
    let mut maintainer_exprs = Vec::new();
    let mut choices = Vec::new();
    let mut interface_instances = Vec::new();

    for item in &t.body {
        match item {
            TemplateBodyDecl::Signatory { parties, .. } => {
                signatories.extend(party_names(parties));
                signatory_exprs.extend(parties.iter().map(lower_expr));
            }
            TemplateBodyDecl::Observer { parties, .. } => {
                observers.extend(party_names(parties));
                observer_exprs.extend(parties.iter().map(lower_expr));
            }
            TemplateBodyDecl::Ensure { expr, pos } => {
                ensure_clause = Some(EnsureClause {
                    raw_text: format!("ensure {}", expr.render()),
                    expr: lower_expr(expr),
                    span: span_at(file, *pos),
                });
            }
            TemplateBodyDecl::Key {
                expr, type_text, ..
            } => {
                key_expr = Some(lower_expr(expr));
                if !type_text.is_empty() {
                    key_type = Some(type_text.clone());
                }
            }
            TemplateBodyDecl::Maintainer { expr, .. } => {
                maintainer_exprs.push(lower_expr(expr));
            }
            TemplateBodyDecl::Choice(c) => choices.push(lower_choice(c, file, lines)),
            TemplateBodyDecl::InterfaceInstance(ii) => {
                interface_instances.push(InterfaceInstance {
                    interface_name: ii.interface_name.clone(),
                    methods: ii.methods.iter().map(binding_name).collect(),
                    span: span_at(file, ii.pos),
                });
            }
            TemplateBodyDecl::Other { .. } => {}
        }
    }

    Template {
        name: t.name.clone(),
        fields,
        signatories,
        observers,
        signatory_exprs,
        observer_exprs,
        ensure_clause,
        key_expr,
        key_type,
        maintainer_exprs,
        choices,
        interface_instances,
        span: span_at(file, t.pos),
    }
}

fn lower_interface(i: &ast::InterfaceDecl, file: &Path, lines: &[&str]) -> Interface {
    Interface {
        name: i.name.clone(),
        requires: i.requires.clone(),
        viewtype: i.viewtype.clone(),
        methods: i
            .methods
            .iter()
            .map(|m| InterfaceMethod {
                name: m.name.clone(),
                type_text: m.type_text.clone(),
                span: span_at(file, m.pos),
            })
            .collect(),
        choices: i.choices.iter().map(|c| lower_choice(c, file, lines)).collect(),
        span: span_at(file, i.pos),
    }
}

/// Flatten party expressions into comparable strings: a list literal
/// contributes one entry per element (`signatory [a, b]` → "a", "b").
fn party_names(exprs: &[ast::Expr]) -> Vec<String> {
    let mut out = Vec::new();
    for e in exprs {
        match e {
            ast::Expr::List { items, .. } => out.extend(items.iter().map(|i| i.render())),
            other => out.push(other.render()),
        }
    }
    out
}

fn lower_choice(c: &ast::ChoiceDecl, file: &Path, lines: &[&str]) -> Choice {
    let parameters = c
        .params
        .iter()
        .map(|f| Field {
            name: f.name.clone(),
            type_: DamlType::from_str(&f.type_text),
            span: span_at(file, f.pos),
        })
        .collect();

    // body_raw is the original source slice (line-faithful: built-in
    // detectors scan it by line offset from the choice span).
    let first = c.pos.line; // 1-based header line
    let last = c.end_line.min(lines.len());
    let body_raw = if first < last {
        lines[first..last].join("\n")
    } else {
        String::new()
    };

    let body = match &c.body {
        Some(expr) => statements_of_expr(expr),
        None => Vec::new(),
    };

    Choice {
        name: c.name.clone(),
        consuming: c.consuming == Consuming::Consuming,
        controllers: party_names(&c.controllers),
        controller_exprs: c.controllers.iter().map(lower_expr).collect(),
        observer_exprs: c.observers.iter().map(lower_expr).collect(),
        parameters,
        return_type: if c.return_type_text.is_empty() {
            DamlType::Unknown
        } else {
            DamlType::from_str(&c.return_type_text)
        },
        body,
        body_raw,
        span: span_at(file, c.pos),
    }
}

fn lower_function(f: &ast::FunctionDecl, file: &Path, lines: &[&str]) -> Function {
    let first = f.pos.line.saturating_sub(1);
    let last = f.end_line.min(lines.len());
    let body_raw = if first < last {
        lines[first..last].join("\n")
    } else {
        String::new()
    };

    let mut body = Vec::new();
    for eq in &f.equations {
        if eq.guards.is_empty() {
            body.extend(statements_of_expr(&eq.body));
        } else {
            for (_, guard_body) in &eq.guards {
                body.extend(statements_of_expr(guard_body));
            }
        }
        // `where` helpers can perform ledger actions when invoked; surface
        // their actions like the line shim did.
        for b in &eq.where_bindings {
            let mut acts = Vec::new();
            collect_actions(&b.expr, None, &mut acts);
            body.extend(acts);
        }
    }

    Function {
        name: f.name.clone(),
        type_signature: f.type_text.clone(),
        body,
        body_raw,
        span: span_at(file, f.pos),
    }
}

// ----- statements --------------------------------------------------------

/// Statements of a choice/function body expression: a do block yields its
/// statements; any other expression is a single statement.
fn statements_of_expr(expr: &ast::Expr) -> Vec<Statement> {
    match expr {
        ast::Expr::Do { stmts, .. } => lower_do(stmts),
        other => {
            let mut acts = Vec::new();
            if collect_actions(other, None, &mut acts) {
                acts
            } else {
                vec![other_statement(other, None)]
            }
        }
    }
}

fn other_statement(expr: &ast::Expr, binder: Option<&ast::Pat>) -> Statement {
    let raw = match binder {
        Some(p) => format!("{} <- {}", p.render(), expr.render()),
        None => expr.render(),
    };
    Statement::Other {
        raw,
        expr: lower_expr(expr),
        binder: binder.map(|p| p.render()),
        span: src_pos(expr.pos()),
    }
}

fn lower_do(stmts: &[DoStmt]) -> Vec<Statement> {
    let mut out = Vec::new();
    for stmt in stmts {
        match stmt {
            DoStmt::Let { bindings, .. } => {
                for b in bindings {
                    out.push(Statement::Let {
                        name: binding_name(b),
                        expr: b.expr.render(),
                        value: lower_expr(&b.expr),
                        span: src_pos(b.pos),
                    });
                    // A plain `let x = create ...` binds an Update value
                    // without executing it, but a let-bound local helper
                    // (`let go x = do archive x`) performs its actions when
                    // invoked from this body — surface those.
                    if !b.params.is_empty() {
                        let mut acts = Vec::new();
                        collect_actions(&b.expr, None, &mut acts);
                        out.extend(acts);
                    }
                }
            }
            DoStmt::Bind { pat, expr, .. } => {
                let mut acts = Vec::new();
                if collect_actions(expr, Some(pat), &mut acts) {
                    out.extend(acts);
                } else {
                    out.push(other_statement(expr, Some(pat)));
                }
            }
            DoStmt::Expr { expr, .. } => {
                let mut acts = Vec::new();
                if collect_actions(expr, None, &mut acts) {
                    out.extend(acts);
                } else {
                    out.push(other_statement(expr, None));
                }
            }
        }
    }
    out
}

/// Walk an expression collecting ledger-action statements (create,
/// exercise, fetch, archive, assert, try/catch). Returns true if anything
/// was collected. Only unqualified applications count: `Lifecycle.exercise`
/// is a user function, not the ledger action.
fn collect_actions(
    expr: &ast::Expr,
    binder: Option<&ast::Pat>,
    out: &mut Vec<Statement>,
) -> bool {
    let before = out.len();
    match expr {
        ast::Expr::Do { stmts, .. } => {
            out.extend(lower_do(stmts));
        }
        ast::Expr::Try {
            body, handlers, ..
        } => {
            let try_body = statements_of_expr(body);
            let mut catch_body = Vec::new();
            for h in handlers {
                catch_body.extend(statements_of_expr(&h.body));
            }
            out.push(Statement::TryCatch {
                try_body,
                catch_body,
                span: src_pos(expr.pos()),
            });
        }
        ast::Expr::If {
            then_branch,
            else_branch,
            ..
        } => {
            collect_actions(then_branch, None, out);
            collect_actions(else_branch, None, out);
        }
        ast::Expr::Case { alts, .. } => {
            for a in alts {
                collect_actions(&a.body, None, out);
            }
        }
        ast::Expr::LetIn { body, .. } => {
            collect_actions(body, None, out);
        }
        ast::Expr::Lambda { body, .. } => {
            collect_actions(body, None, out);
        }
        ast::Expr::Neg { expr, .. } => {
            collect_actions(expr, None, out);
        }
        ast::Expr::BinOp { op, lhs, rhs, pos } => {
            // `create $ Foo with ...` — `$` is application.
            if op == "$" {
                let as_app = ast::Expr::App {
                    func: lhs.clone(),
                    args: vec![(**rhs).clone()],
                    pos: *pos,
                };
                if classify_app(&as_app, binder, out) {
                    return out.len() > before;
                }
            }
            collect_actions(lhs, None, out);
            collect_actions(rhs, None, out);
        }
        ast::Expr::App { args, .. } => {
            if !classify_app(expr, binder, out) {
                for a in args {
                    collect_actions(a, None, out);
                }
            }
        }
        ast::Expr::Tuple { items, .. } | ast::Expr::List { items, .. } => {
            for i in items {
                collect_actions(i, None, out);
            }
        }
        _ => {}
    }
    out.len() > before
}

/// If `expr` is an application of a ledger-action head, push the matching
/// statement(s) and return true.
fn classify_app(
    expr: &ast::Expr,
    binder: Option<&ast::Pat>,
    out: &mut Vec<Statement>,
) -> bool {
    let args = expr.app_args();
    if args.is_empty() {
        return false;
    }
    let head_name = match expr.app_head() {
        ast::Expr::Var {
            qualifier: None,
            name,
            ..
        } => name.as_str(),
        _ => return false,
    };
    let arg_text = |i: usize| args.get(i).map(|a| a.render()).unwrap_or_default();
    let arg_expr = |i: usize| {
        args.get(i).map(lower_expr).unwrap_or(Expr::Unknown {
            raw: String::new(),
            span: src_pos(expr.pos()),
        })
    };
    let binder_name = binder.map(|p| p.render());
    let span = src_pos(expr.pos());
    match head_name {
        "create" | "createCmd" => {
            out.push(Statement::Create {
                template_name: template_name_of(args.first()),
                raw: expr.render(),
                argument: arg_expr(0),
                binder: binder_name,
                span,
            });
            true
        }
        "exercise" | "exerciseByKey" | "exerciseCmd" | "exerciseByKeyCmd" => {
            out.push(Statement::Exercise {
                cid_expr: arg_text(0),
                choice_name: choice_name_of(args.get(1)),
                raw: expr.render(),
                cid: arg_expr(0),
                argument: args.get(1).map(lower_expr),
                binder: binder_name,
                span,
            });
            true
        }
        "createAndExerciseCmd" => {
            out.push(Statement::Create {
                template_name: template_name_of(args.first()),
                raw: expr.render(),
                argument: arg_expr(0),
                binder: binder_name.clone(),
                span,
            });
            out.push(Statement::Exercise {
                cid_expr: arg_text(0),
                choice_name: choice_name_of(args.get(1)),
                raw: expr.render(),
                cid: arg_expr(0),
                argument: args.get(1).map(lower_expr),
                binder: binder_name,
                span,
            });
            true
        }
        "fetch" => {
            out.push(Statement::Fetch {
                cid_expr: arg_text(0),
                cid: arg_expr(0),
                binder: binder_name,
                span,
            });
            true
        }
        "fetchAndArchive" => {
            out.push(Statement::Archive {
                cid_expr: arg_text(0),
                cid: arg_expr(0),
                span,
            });
            out.push(Statement::Fetch {
                cid_expr: arg_text(0),
                cid: arg_expr(0),
                binder: binder_name,
                span,
            });
            true
        }
        "archive" => {
            out.push(Statement::Archive {
                cid_expr: arg_text(0),
                cid: arg_expr(0),
                span,
            });
            true
        }
        "assert" | "assertMsg" => {
            out.push(Statement::Assert {
                condition: expr.render(),
                condition_expr: lower_expr(expr),
                span,
            });
            true
        }
        _ => false,
    }
}

fn template_name_of(arg: Option<&ast::Expr>) -> String {
    match arg {
        Some(ast::Expr::Record { base, .. }) => template_name_of(Some(base)),
        Some(ast::Expr::Con {
            qualifier, name, ..
        }) => match qualifier {
            Some(q) => format!("{}.{}", q, name),
            None => name.clone(),
        },
        Some(ast::Expr::Var { name, .. }) if name == "this" => "this".to_string(),
        _ => String::new(),
    }
}

fn choice_name_of(arg: Option<&ast::Expr>) -> String {
    match arg {
        Some(ast::Expr::Record { base, .. }) => choice_name_of(Some(base)),
        Some(ast::Expr::Con {
            qualifier, name, ..
        }) => match qualifier {
            Some(q) => format!("{}.{}", q, name),
            None => name.clone(),
        },
        Some(ast::Expr::App { func, .. }) => choice_name_of(Some(func)),
        _ => String::new(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_parse_simple_template() {
        let source = r#"module Test where

template SimpleHolding
  with
    admin : Party
    amount : Decimal
  where
    signatory admin
    ensure amount > 0.0

    choice Transfer : ContractId SimpleHolding
      with
        newOwner : Party
      controller admin
      do
        create this with admin = newOwner
"#;
        let module = parse_daml(source, Path::new("Test.daml"));
        assert_eq!(module.name, "Test");
        assert_eq!(module.templates.len(), 1);

        let t = &module.templates[0];
        assert_eq!(t.name, "SimpleHolding");
        assert_eq!(t.fields.len(), 2);
        assert_eq!(t.fields[0].name, "admin");
        assert!(matches!(t.fields[0].type_, DamlType::Party));
        assert_eq!(t.fields[1].name, "amount");
        assert!(t.fields[1].type_.is_decimal());
        assert!(t.ensure_clause.is_some());
        assert!(t.ensure_clause.as_ref().unwrap().raw_text.contains("amount > 0.0"));
        assert_eq!(t.choices.len(), 1);
        assert_eq!(t.choices[0].name, "Transfer");
        assert_eq!(t.choices[0].parameters.len(), 1);
        // The real parser extracts structure the shim could not:
        assert!(matches!(
            t.choices[0].return_type,
            DamlType::ContractId(_)
        ));
        assert!(t.choices[0]
            .body
            .iter()
            .any(|s| matches!(s, Statement::Create { template_name, .. } if template_name == "this")));
    }

    #[test]
    fn test_parse_template_without_ensure() {
        let source = r#"module Test where

template OpenMiningRound
  with
    admin : Party
    amuletPrice : Decimal
    tickDuration : RelTime
  where
    signatory admin
"#;
        let module = parse_daml(source, Path::new("Round.daml"));
        assert_eq!(module.templates.len(), 1);
        let t = &module.templates[0];
        assert_eq!(t.name, "OpenMiningRound");
        assert!(t.ensure_clause.is_none());
        assert_eq!(t.fields.len(), 3);
        assert!(t.fields[1].type_.is_decimal());
    }

    #[test]
    fn test_parse_nonconsuming_choice() {
        let source = r#"module Test where

template Foo
  with
    owner : Party
  where
    signatory owner

    nonconsuming choice GetInfo : Text
      controller owner
      do
        pure "info"
"#;
        let module = parse_daml(source, Path::new("Foo.daml"));
        assert_eq!(module.templates[0].choices.len(), 1);
        assert!(!module.templates[0].choices[0].consuming);
    }

    #[test]
    fn test_comment_with_exercise_keyword_is_not_a_statement() {
        let source = r#"module Test where

template Foo
  with
    owner : Party
  where
    signatory owner

    choice Go : ()
      controller owner
      do
        -- electing to exercise the option
        pure ()
"#;
        let module = parse_daml(source, Path::new("Foo.daml"));
        let body = &module.templates[0].choices[0].body;
        assert!(
            !body.iter().any(|s| matches!(s, Statement::Exercise { .. })),
            "comment text must not become an Exercise statement: {:?}",
            body
        );
    }

    #[test]
    fn test_exercise_extracts_cid_and_choice() {
        let source = r#"module Test where

template Foo
  with
    owner : Party
  where
    signatory owner

    choice Go : ()
      controller owner
      do
        result <- exercise optionCid Elect with electorParty = owner
        pure ()
"#;
        let module = parse_daml(source, Path::new("Foo.daml"));
        let body = &module.templates[0].choices[0].body;
        let ex = body
            .iter()
            .find_map(|s| match s {
                Statement::Exercise {
                    cid_expr,
                    choice_name,
                    ..
                } => Some((cid_expr.clone(), choice_name.clone())),
                _ => None,
            })
            .expect("exercise statement");
        assert_eq!(ex.0, "optionCid");
        assert_eq!(ex.1, "Elect");
    }

    #[test]
    fn test_signatory_list_flattened() {
        let source = r#"module Test where

template Foo
  with
    a : Party
    b : Party
  where
    signatory [a, b]
"#;
        let module = parse_daml(source, Path::new("Foo.daml"));
        assert_eq!(module.templates[0].signatories, vec!["a", "b"]);
    }

    #[test]
    fn test_interface_methods_are_not_functions() {
        let source = r#"module Test where

interface Base where
  viewtype View
  getOwner : Party

  nonconsuming choice GetView : View
    with
      viewer : Party
    controller viewer
    do
      pure (view this)
"#;
        let module = parse_daml(source, Path::new("Base.daml"));
        assert!(
            module.functions.is_empty(),
            "interface methods must not be extracted as top-level functions: {:?}",
            module.functions.iter().map(|f| &f.name).collect::<Vec<_>>()
        );
    }
}
