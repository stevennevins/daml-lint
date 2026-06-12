//! AST ground-truth integration tests against the daml-finance corpus.
//!
//! Facts below were hand-verified against the sources once (grep + read);
//! these tests pin them so parser changes cannot silently regress structure
//! extraction. They skip when /tmp/finance-lint-repo is absent.

#![cfg(test)]

use crate::ir::*;
use crate::parser::parse_daml_with_diagnostics;
use std::path::{Path, PathBuf};

const ROOT: &str = "/tmp/finance-lint-repo/daml/src/main/daml/Daml/Finance";

fn load(rel: &str) -> Option<DamlModule> {
    let path = PathBuf::from(ROOT).join(rel);
    if !path.exists() {
        eprintln!("corpus missing, skipping ground-truth test for {}", rel);
        return None;
    }
    let source = std::fs::read_to_string(&path).unwrap();
    let (module, diags) = parse_daml_with_diagnostics(&source, Path::new(rel));
    assert!(
        diags.is_empty(),
        "parse diagnostics in {}: {:?}",
        rel,
        diags
    );
    Some(module)
}

fn template<'a>(m: &'a DamlModule, name: &str) -> &'a Template {
    m.templates
        .iter()
        .find(|t| t.name == name)
        .unwrap_or_else(|| panic!("template {} not found", name))
}

fn interface<'a>(m: &'a DamlModule, name: &str) -> &'a Interface {
    m.interfaces
        .iter()
        .find(|i| i.name == name)
        .unwrap_or_else(|| panic!("interface {} not found", name))
}

#[test]
fn settlement_instruction_template() {
    let Some(m) = load("Settlement/V4/Instruction.daml") else {
        return;
    };
    assert_eq!(m.name, "Daml.Finance.Settlement.V4.Instruction");
    let qualified_aliased = m
        .imports
        .iter()
        .filter(|i| i.qualified && i.alias.is_some())
        .count();
    assert_eq!(qualified_aliased, 9);

    let t = template(&m, "Instruction");
    assert_eq!(t.fields.len(), 12);
    assert_eq!(
        t.signatories,
        vec!["instructor", "consenters", "signedSenders", "signedReceivers"]
    );
    assert_eq!(t.key_type.as_deref(), Some("InstructionKey"));
    assert!(t.key_expr.is_some());
    let instances: Vec<&str> = t
        .interface_instances
        .iter()
        .map(|i| i.interface_name.as_str())
        .collect();
    assert_eq!(instances, vec!["Disclosure.I", "Instruction.I"]);

    // releasePreviousAllocation exercises Lockable.Release and fetches the
    // holding (hand-verified at source lines ~301-311).
    let f = m
        .functions
        .iter()
        .find(|f| f.name == "releasePreviousAllocation")
        .expect("function releasePreviousAllocation");
    fn has_kind(stmts: &[Statement], pred: &dyn Fn(&Statement) -> bool) -> bool {
        stmts.iter().any(|s| {
            pred(s)
                || matches!(s, Statement::TryCatch { try_body, catch_body, .. }
                    if has_kind(try_body, pred) || has_kind(catch_body, pred))
        })
    }
    assert!(has_kind(&f.body, &|s| matches!(
        s,
        Statement::Exercise { .. }
    )));
    assert!(has_kind(&f.body, &|s| matches!(s, Statement::Fetch { .. })));
}

#[test]
fn interface_settlement_instruction() {
    let Some(m) = load("Interface/Settlement/V4/Instruction.daml") else {
        return;
    };
    let i = interface(&m, "Instruction");
    assert_eq!(i.viewtype.as_deref(), Some("V"));
    let methods: Vec<&str> = i.methods.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(methods, vec!["allocate", "approve", "execute", "cancel"]);
    let choices: Vec<(&str, bool)> = i
        .choices
        .iter()
        .map(|c| (c.name.as_str(), c.consuming))
        .collect();
    assert_eq!(
        choices,
        vec![
            ("GetView", false),
            ("Allocate", true),
            ("Approve", true),
            ("Execute", true),
            ("Cancel", true),
        ]
    );
}

#[test]
fn account_template_with_ensure() {
    let Some(m) = load("Account/V4/Account.daml") else {
        return;
    };
    let t = template(&m, "Account");
    assert_eq!(t.fields.len(), 8);
    assert_eq!(
        t.signatories,
        vec!["custodian", "owner", "Lockable.getLockers this"]
    );
    let ensure = t.ensure_clause.as_ref().expect("Account has ensure");
    // `ensure isValidLock lock && (not . Set.null $ controllers.outgoing)`
    assert!(matches!(&ensure.expr, Expr::BinOp { op, .. } if op == "&&"));
    let instances: Vec<&str> = t
        .interface_instances
        .iter()
        .map(|i| i.interface_name.as_str())
        .collect();
    assert_eq!(instances, vec!["Account.I", "Lockable.I", "Disclosure.I"]);
    // Module also declares the account Factory template.
    assert_eq!(template(&m, "Factory").fields.len(), 2);
}

#[test]
fn interface_account_reference_template() {
    let Some(m) = load("Interface/Account/V4/Account.daml") else {
        return;
    };
    let i = interface(&m, "Account");
    assert_eq!(i.requires, vec!["Disclosure.I"]);
    let names: Vec<&str> = i.choices.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, vec!["GetView", "Credit", "Debit", "Remove"]);
    let remove = i.choices.iter().find(|c| c.name == "Remove").unwrap();
    assert_eq!(remove.controllers, vec!["signatory this"]);

    // The Reference helper template: choices controlled by `signatory this`.
    let r = template(&m, "Reference");
    assert_eq!(r.fields.len(), 3);
    assert_eq!(r.key_type.as_deref(), Some("AccountKey"));
    let by_name = |n: &str| r.choices.iter().find(|c| c.name == n).unwrap();
    assert!(!by_name("GetCid").consuming);
    assert_eq!(by_name("GetCid").controllers, vec!["viewer"]);
    assert_eq!(by_name("SetCid").controllers, vec!["signatory this"]);
    assert_eq!(by_name("SetObservers").controllers, vec!["signatory this"]);
    // Structured controller expression: App of Var "signatory" to "this".
    match &by_name("SetCid").controller_exprs[0] {
        Expr::App { func, args, .. } => {
            assert!(matches!(&**func, Expr::Var { name, .. } if name == "signatory"));
            assert!(matches!(&args[0], Expr::Var { name, .. } if name == "this"));
        }
        other => panic!("expected App for 'signatory this', got {:?}", other),
    }
}

#[test]
fn holding_fungible_template() {
    let Some(m) = load("Holding/V4/Fungible.daml") else {
        return;
    };
    let t = template(&m, "Fungible");
    assert_eq!(t.fields.len(), 5);
    assert!(t.ensure_clause.is_some());
    assert_eq!(
        t.signatories,
        vec![
            "account.custodian",
            "account.owner",
            "Lockable.getLockers this"
        ]
    );
    assert_eq!(t.interface_instances.len(), 4);
}

#[test]
fn interface_holding_requires() {
    let Some(m) = load("Interface/Holding/V4/Holding.daml") else {
        return;
    };
    let i = interface(&m, "Holding");
    assert_eq!(i.requires, vec!["Lockable.I", "Disclosure.I"]);
    assert_eq!(i.viewtype.as_deref(), Some("V"));
}

#[test]
fn settlement_batch_module() {
    let Some(m) = load("Settlement/V4/Batch.daml") else {
        return;
    };
    let t = template(&m, "Batch");
    assert_eq!(t.fields.len(), 8);
    assert_eq!(t.signatories, vec!["instructor", "consenters"]);
    let fns: Vec<&str> = m.functions.iter().map(|f| f.name.as_str()).collect();
    for expected in ["routedSteps", "instructionIds", "buildKey"] {
        assert!(fns.contains(&expected), "function {} missing", expected);
    }
}

#[test]
fn interface_disclosure_module() {
    let Some(m) = load("Interface/Util/V3/Disclosure.daml") else {
        return;
    };
    let i = interface(&m, "Disclosure");
    assert_eq!(i.viewtype.as_deref(), Some("V"));
    let methods: Vec<&str> = i.methods.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(
        methods,
        vec!["setObservers", "addObservers", "removeObservers"]
    );
    let choices: Vec<&str> = i.choices.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        choices,
        vec!["GetView", "SetObservers", "AddObservers", "RemoveObservers"]
    );
    assert!(m.functions.iter().any(|f| f.name == "flattenObservers"));
}

#[test]
fn token_instrument_template() {
    let Some(m) = load("Instrument/Token/V4/Instrument.daml") else {
        return;
    };
    let t = template(&m, "Instrument");
    assert_eq!(t.fields.len(), 8);
    assert_eq!(t.signatories, vec!["depository", "issuer"]);
    assert_eq!(t.interface_instances.len(), 3);
}

#[test]
fn lifecycle_distribution_rule_template() {
    let Some(m) = load("Lifecycle/V4/Rule/Distribution.daml") else {
        return;
    };
    let t = template(&m, "Rule");
    assert_eq!(t.fields.len(), 5);
    assert_eq!(t.signatories, vec!["providers"]);
    let instances: Vec<&str> = t
        .interface_instances
        .iter()
        .map(|i| i.interface_name.as_str())
        .collect();
    assert_eq!(instances, vec!["Lifecycle.I"]);
}

/// Whole-corpus phase gate at the parser level: every file parses with
/// zero diagnostics.
#[test]
fn corpus_parses_clean() {
    let root = Path::new("/tmp/finance-lint-repo/daml");
    if !root.exists() {
        return;
    }
    let mut files = Vec::new();
    collect(root, &mut files);
    assert!(files.len() > 600, "corpus incomplete: {}", files.len());
    let mut diag_count = 0;
    for f in &files {
        let src = std::fs::read_to_string(f).unwrap();
        let (_, diags) = parse_daml_with_diagnostics(&src, f);
        if !diags.is_empty() {
            eprintln!("{}: {:?}", f.display(), diags);
        }
        diag_count += diags.len();
    }
    assert_eq!(diag_count, 0, "parse diagnostics across corpus");
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let p = entry.path();
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().is_some_and(|e| e == "daml") {
            out.push(p);
        }
    }
}
