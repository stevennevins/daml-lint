//! Adversarial inputs: hostile DAML that must parse without panic, hang,
//! or phantom structure. The lexer/layout/parser pipeline is the defense;
//! these tests pin the failure modes the old line shim had.

#![cfg(test)]

use crate::ir::*;
use crate::parser::parse_daml_with_diagnostics;
use std::path::Path;

fn parse(source: &str) -> DamlModule {
    parse_daml_with_diagnostics(source, Path::new("hostile.daml")).0
}

#[test]
fn keywords_in_comments_create_no_structure() {
    let m = parse(
        "module M where\n\
         -- template Fake with choice Evil : () controller attacker\n\
         {- template Hidden\n\
              with\n\
                x : Party\n\
         -}\n\
         f = 1\n",
    );
    assert!(m.templates.is_empty());
    assert_eq!(m.functions.len(), 1);
}

#[test]
fn keywords_in_strings_create_no_structure() {
    let m = parse(
        "module M where\n\
         f = \"template Fake with x : Party where signatory attacker\"\n\
         g = \"exercise cid Evil\"\n",
    );
    assert!(m.templates.is_empty());
    for func in &m.functions {
        assert!(
            !func.body.iter().any(|s| matches!(s, Statement::Exercise { .. })),
            "string literal must not become an Exercise"
        );
    }
}

#[test]
fn nested_block_comments_with_fake_terminators() {
    // Nesting: the inner `-}` must not close the outer comment. (Note
    // `--` inside a block comment is NOT special — a following `-}`
    // still closes, per Haskell.)
    let m = parse(
        "module M where\n\
         {- outer {- inner -} still outer, f_in_comment = 1 -}\n\
         real = 2\n",
    );
    assert_eq!(m.functions.len(), 1);
    assert_eq!(m.functions[0].name, "real");
}

#[test]
fn tabs_mixed_with_spaces() {
    // Tab advances to the next 8-column stop; the field block resolves
    // the same whether indented with tabs or spaces.
    let m = parse(
        "module M where\n\
         template T\n\
         \twith\n\
         \t\tx : Party\n\
         \twhere\n\
         \t\tsignatory x\n",
    );
    assert_eq!(m.templates.len(), 1);
    assert_eq!(m.templates[0].fields.len(), 1);
    assert_eq!(m.templates[0].signatories, vec!["x"]);
}

#[test]
fn unicode_identifiers() {
    let m = parse(
        "module M where\n\
         template Vertrag\n  with\n    eigentümer : Party\n    größe : Decimal\n  where\n    signatory eigentümer\n",
    );
    assert_eq!(m.templates[0].fields.len(), 2);
    assert_eq!(m.templates[0].fields[0].name, "eigentümer");
}

#[test]
fn ten_thousand_line_file_parses_quickly() {
    let mut src = String::from("module Big where\n\n");
    for i in 0..1000 {
        src.push_str(&format!(
            "template T{i}\n  with\n    owner : Party\n    amount : Decimal\n  where\n    signatory owner\n    ensure amount > 0.0\n\n    choice C{i} : ()\n      controller owner\n      do\n        pure ()\n\n"
        ));
    }
    assert!(src.lines().count() > 10_000);
    let start = std::time::Instant::now();
    let (m, diags) = parse_daml_with_diagnostics(&src, Path::new("big.daml"));
    assert!(diags.is_empty());
    assert_eq!(m.templates.len(), 1000);
    assert!(
        start.elapsed().as_secs() < 5,
        "10k-line file took {:?}",
        start.elapsed()
    );
}

#[test]
fn deeply_nested_parens_no_stack_overflow() {
    let mut src = String::from("module M where\nf = ");
    src.push_str(&"(".repeat(5000));
    src.push('1');
    src.push_str(&")".repeat(5000));
    src.push('\n');
    let _ = parse(&src); // must not crash
}

#[test]
fn deeply_nested_patterns_no_stack_overflow() {
    let mut src = String::from("module M where\nf ");
    src.push_str(&"(Just ".repeat(2000));
    src.push('x');
    src.push_str(&")".repeat(2000));
    src.push_str(" = 1\n");
    let _ = parse(&src);
}

#[test]
fn unterminated_everything_no_hang() {
    for hostile in [
        "module M where\nf = \"never closed\ng = 2\n",
        "module M where\n{- never closed",
        "module M where\nf = (((((\n",
        "module M where\ntemplate T\n  with\n",
        "module M where\nf = do\n",
        "module M where\nf = let x = \n",
        "template",
        "",
        "\n\n\n",
        "-- only a comment\n",
        "\u{FEFF}module M where\nf = 1\n", // BOM
    ] {
        let _ = parse(hostile); // must terminate without panic
    }
}

#[test]
fn crlf_line_endings() {
    let m = parse("module M where\r\n\r\ntemplate T\r\n  with\r\n    x : Party\r\n  where\r\n    signatory x\r\n");
    assert_eq!(m.templates.len(), 1);
    assert_eq!(m.templates[0].fields.len(), 1);
}

#[test]
fn string_with_escaped_quotes_and_comment_markers() {
    let m = parse(
        "module M where\nf = \"a \\\" -- not a comment {- not a block\"\ng = 2\n",
    );
    assert_eq!(m.functions.len(), 2);
}

#[test]
fn operator_that_looks_like_comment() {
    // `-->` and `--^` are operators; `--` and `---` start comments.
    let m = parse("module M where\nf = a --> b\ng = c --- this is a comment\n");
    assert_eq!(m.functions.len(), 2);
    // f's body must contain the --> application, g's must not see the comment
    assert!(m.functions[0].body_raw.contains("-->"));
}

#[test]
fn pathological_one_liner_template() {
    let m = parse(
        "module M where\ntemplate T with { x : Party } where { signatory x }\n",
    );
    assert_eq!(m.templates.len(), 1);
    assert_eq!(m.templates[0].signatories, vec!["x"]);
}

#[test]
fn comment_between_template_and_fields() {
    let m = parse(concat!(
        "module M where\n",
        "template T\n",
        "  -- fields below\n",
        "  with\n",
        "    -- the owner\n",
        "    x : Party\n",
        "  where\n",
        "    signatory x\n",
    ));
    assert_eq!(m.templates[0].fields.len(), 1);
}

/// Regression: operator bindings in where blocks left the cursor on an
/// unmatched ')' that block loops refused to consume — infinite loop.
/// Found by scanning the daml SDK corpus (Control/Exception/Base.daml).
#[test]
fn operator_binding_in_where_no_hang() {
    let m = parse(concat!(
        "module M where\n",
        "f x = implode x\n",
        "  where\n",
        "    implode : [Text] -> Text = primitive @\"BEImplodeText\"\n",
        "    (==) : Text -> Text -> Bool = primitive @\"BEEqual\"\n",
        "    helper y = y\n",
        "g = 2\n",
    ));
    assert!(m.functions.iter().any(|f| f.name == "g"));
}

/// CPP directives at column 1 (daml-prim/stdlib use LANGUAGE CPP) are
/// line-based and skipped; `#` elsewhere stays an operator.
#[test]
fn cpp_directives_skipped() {
    let m = parse(concat!(
        "module M where\n",
        "#ifdef DAML_BIGNUMERIC\n",
        "f = 1\n",
        "#endif\n",
        "g = 2\n",
    ));
    assert_eq!(m.functions.len(), 2);
}

/// An empty `with` block (comment-only) must not swallow the following
/// controller clause (deliberate fixture in the daml SDK test suite).
#[test]
fn empty_with_block_before_controller() {
    let m = parse(concat!(
        "module M where\n",
        "template T\n",
        "  with\n",
        "    owner : Party\n",
        "  where\n",
        "    signatory owner\n",
        "    choice F : ()\n",
        "      with -- superfluous, no fields\n",
        "      controller owner\n",
        "      do pure ()\n",
    ));
    let c = &m.templates[0].choices[0];
    assert_eq!(c.name, "F");
    assert_eq!(c.controllers, vec!["owner"]);
    assert!(c.parameters.is_empty());
}

#[test]
fn huge_single_line() {
    let mut src = String::from("module M where\nf = ");
    for i in 0..20_000 {
        src.push_str(&format!("g{} ", i));
    }
    src.push('\n');
    let _ = parse(&src);
}
