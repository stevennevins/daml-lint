//! Lowering: typed AST (src/ast.rs, built by src/parse.rs) → rule-facing IR
//! (src/ir.rs).
//!
//! This replaces the old line-based keyword shim. The IR shapes are the
//! stable contract with rule scripts; raw-text fields (`body_raw`,
//! `raw_text`, statement `raw`) are reconstructed from real parse trees so
//! existing rules keep working.

use crate::ast::{self, Consuming, Decl, DoStmt, Expr, TemplateBodyDecl};
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

    let span = |pos: ast::Pos| Span {
        file: file.to_path_buf(),
        line: pos.line,
        column: pos.column,
    };

    let imports = module
        .imports
        .iter()
        .map(|i| Import {
            module_name: i.module_name.clone(),
            qualified: i.qualified,
            alias: i.alias.clone(),
            span: span(i.pos),
        })
        .collect();

    let mut templates = Vec::new();
    let mut functions = Vec::new();

    for decl in &module.decls {
        match decl {
            Decl::Template(t) => templates.push(lower_template(t, file, &lines)),
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
        functions,
    };
    let diags = diags
        .into_iter()
        .map(|d| (d.pos.line, d.pos.column, d.message))
        .collect();
    (ir, diags)
}

fn lower_template(t: &ast::TemplateDecl, file: &Path, lines: &[&str]) -> Template {
    let span = |pos: ast::Pos| Span {
        file: file.to_path_buf(),
        line: pos.line,
        column: pos.column,
    };

    let fields = t
        .fields
        .iter()
        .map(|f| Field {
            name: f.name.clone(),
            type_: DamlType::from_str(&f.type_text),
            span: span(f.pos),
        })
        .collect();

    let mut signatories = Vec::new();
    let mut observers = Vec::new();
    let mut ensure_clause = None;
    let mut choices = Vec::new();

    for item in &t.body {
        match item {
            TemplateBodyDecl::Signatory { parties, .. } => {
                signatories.extend(party_names(parties));
            }
            TemplateBodyDecl::Observer { parties, .. } => {
                observers.extend(party_names(parties));
            }
            TemplateBodyDecl::Ensure { expr, pos } => {
                ensure_clause = Some(EnsureClause {
                    raw_text: format!("ensure {}", expr.render()),
                    span: span(*pos),
                });
            }
            TemplateBodyDecl::Choice(c) => choices.push(lower_choice(c, file, lines)),
            _ => {}
        }
    }

    Template {
        name: t.name.clone(),
        fields,
        signatories,
        observers,
        ensure_clause,
        choices,
        span: span(t.pos),
    }
}

/// Flatten party expressions into comparable strings: a list literal
/// contributes one entry per element (`signatory [a, b]` → "a", "b").
fn party_names(exprs: &[Expr]) -> Vec<String> {
    let mut out = Vec::new();
    for e in exprs {
        match e {
            Expr::List { items, .. } => out.extend(items.iter().map(|i| i.render())),
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
            span: Span {
                file: file.to_path_buf(),
                line: f.pos.line,
                column: f.pos.column,
            },
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
        parameters,
        return_type: if c.return_type_text.is_empty() {
            DamlType::Unknown
        } else {
            DamlType::from_str(&c.return_type_text)
        },
        body,
        body_raw,
        span: Span {
            file: file.to_path_buf(),
            line: c.pos.line,
            column: c.pos.column,
        },
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
            collect_actions(&b.expr, &mut acts);
            body.extend(acts);
        }
    }

    Function {
        name: f.name.clone(),
        body,
        body_raw,
        span: Span {
            file: file.to_path_buf(),
            line: f.pos.line,
            column: f.pos.column,
        },
    }
}

/// Statements of a choice/function body expression: a do block yields its
/// statements; any other expression is a single statement.
fn statements_of_expr(expr: &Expr) -> Vec<Statement> {
    match expr {
        Expr::Do { stmts, .. } => lower_do(stmts),
        other => {
            let mut acts = Vec::new();
            if collect_actions(other, &mut acts) {
                acts
            } else {
                vec![Statement::Other {
                    raw: other.render(),
                }]
            }
        }
    }
}

fn lower_do(stmts: &[DoStmt]) -> Vec<Statement> {
    let mut out = Vec::new();
    for stmt in stmts {
        match stmt {
            DoStmt::Let { bindings, .. } => {
                for b in bindings {
                    let mut name = b.pat.render();
                    for p in &b.params {
                        name.push(' ');
                        name.push_str(&p.render());
                    }
                    out.push(Statement::Let {
                        name,
                        expr: b.expr.render(),
                    });
                    // A plain `let x = create ...` binds an Update value
                    // without executing it, but a let-bound local helper
                    // (`let go x = do archive x`) performs its actions when
                    // invoked from this body — surface those.
                    if !b.params.is_empty() {
                        let mut acts = Vec::new();
                        collect_actions(&b.expr, &mut acts);
                        out.extend(acts);
                    }
                }
            }
            DoStmt::Bind { pat, expr, .. } => {
                let mut acts = Vec::new();
                if collect_actions(expr, &mut acts) {
                    out.extend(acts);
                } else {
                    out.push(Statement::Other {
                        raw: format!("{} <- {}", pat.render(), expr.render()),
                    });
                }
            }
            DoStmt::Expr { expr, .. } => {
                let mut acts = Vec::new();
                if collect_actions(expr, &mut acts) {
                    out.extend(acts);
                } else {
                    out.push(Statement::Other {
                        raw: expr.render(),
                    });
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
fn collect_actions(expr: &Expr, out: &mut Vec<Statement>) -> bool {
    let before = out.len();
    match expr {
        Expr::Do { stmts, .. } => {
            out.extend(lower_do(stmts));
        }
        Expr::Try { body, handlers, .. } => {
            let try_body = statements_of_expr(body);
            let mut catch_body = Vec::new();
            for h in handlers {
                catch_body.extend(statements_of_expr(&h.body));
            }
            out.push(Statement::TryCatch {
                try_body,
                catch_body,
            });
        }
        Expr::If {
            then_branch,
            else_branch,
            ..
        } => {
            collect_actions(then_branch, out);
            collect_actions(else_branch, out);
        }
        Expr::Case { alts, .. } => {
            for a in alts {
                collect_actions(&a.body, out);
            }
        }
        Expr::LetIn { body, .. } => {
            collect_actions(body, out);
        }
        Expr::Lambda { body, .. } => {
            collect_actions(body, out);
        }
        Expr::Neg { expr, .. } => {
            collect_actions(expr, out);
        }
        Expr::BinOp { op, lhs, rhs, pos } => {
            // `create $ Foo with ...` — `$` is application.
            if op == "$" {
                let as_app = Expr::App {
                    func: lhs.clone(),
                    args: vec![(**rhs).clone()],
                    pos: *pos,
                };
                if classify_app(&as_app, out) {
                    return out.len() > before;
                }
            }
            collect_actions(lhs, out);
            collect_actions(rhs, out);
        }
        Expr::App { args, .. } => {
            if !classify_app(expr, out) {
                for a in args {
                    collect_actions(a, out);
                }
            }
        }
        Expr::Tuple { items, .. } | Expr::List { items, .. } => {
            for i in items {
                collect_actions(i, out);
            }
        }
        _ => {}
    }
    out.len() > before
}

/// If `expr` is an application of a ledger-action head, push the matching
/// statement(s) and return true.
fn classify_app(expr: &Expr, out: &mut Vec<Statement>) -> bool {
    let args = expr.app_args();
    if args.is_empty() {
        return false;
    }
    let head_name = match expr.app_head() {
        Expr::Var {
            qualifier: None,
            name,
            ..
        } => name.as_str(),
        _ => return false,
    };
    let arg_text = |i: usize| args.get(i).map(|a| a.render()).unwrap_or_default();
    match head_name {
        "create" | "createCmd" => {
            out.push(Statement::Create {
                template_name: template_name_of(args.first()),
                raw: expr.render(),
            });
            true
        }
        "exercise" | "exerciseByKey" | "exerciseCmd" | "exerciseByKeyCmd" => {
            out.push(Statement::Exercise {
                cid_expr: arg_text(0),
                choice_name: choice_name_of(args.get(1)),
                raw: expr.render(),
            });
            true
        }
        "createAndExerciseCmd" => {
            out.push(Statement::Create {
                template_name: template_name_of(args.first()),
                raw: expr.render(),
            });
            out.push(Statement::Exercise {
                cid_expr: arg_text(0),
                choice_name: choice_name_of(args.get(1)),
                raw: expr.render(),
            });
            true
        }
        "fetch" => {
            out.push(Statement::Fetch {
                cid_expr: arg_text(0),
            });
            true
        }
        "fetchAndArchive" => {
            out.push(Statement::Archive {
                cid_expr: arg_text(0),
            });
            out.push(Statement::Fetch {
                cid_expr: arg_text(0),
            });
            true
        }
        "archive" => {
            out.push(Statement::Archive {
                cid_expr: arg_text(0),
            });
            true
        }
        "assert" | "assertMsg" => {
            out.push(Statement::Assert {
                condition: expr.render(),
            });
            true
        }
        _ => false,
    }
}

fn template_name_of(arg: Option<&Expr>) -> String {
    match arg {
        Some(Expr::Record { base, .. }) => template_name_of(Some(base)),
        Some(Expr::Con {
            qualifier, name, ..
        }) => match qualifier {
            Some(q) => format!("{}.{}", q, name),
            None => name.clone(),
        },
        Some(Expr::Var { name, .. }) if name == "this" => "this".to_string(),
        _ => String::new(),
    }
}

fn choice_name_of(arg: Option<&Expr>) -> String {
    match arg {
        Some(Expr::Record { base, .. }) => choice_name_of(Some(base)),
        Some(Expr::Con {
            qualifier, name, ..
        }) => match qualifier {
            Some(q) => format!("{}.{}", q, name),
            None => name.clone(),
        },
        Some(Expr::App { func, .. }) => choice_name_of(Some(func)),
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
