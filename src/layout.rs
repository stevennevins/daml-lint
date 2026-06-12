//! Layout resolution: insert virtual braces/semicolons per the Haskell
//! offside rule (adapted for DAML, where `with` also opens a layout block).
//!
//! Input is the raw token stream from the lexer; output is the same stream
//! with `VLBrace`/`VRBrace`/`VSemi` inserted, so the parser never has to
//! look at columns.

use crate::lexer::{Pos, Tok, Token};

/// Keywords that open a layout block. DAML adds `with` (template fields,
/// choice parameters, record construction) and `catch` (exception handler
/// alternatives) to Haskell's set.
fn is_layout_keyword(tok: &Tok) -> bool {
    matches!(
        tok.keyword(),
        Some("where" | "do" | "of" | "let" | "with" | "catch")
    )
}

#[derive(Debug)]
struct Context {
    /// Column of the block; 0 for an explicit `{ }` context (no offside).
    col: usize,
    /// Line the block was opened on (same-line `where` closure rule).
    line: usize,
    /// Keyword that opened it ("let" matters for the `in` rule).
    opened_by: &'static str,
    /// Bracket nesting depth at open time, so `)` can close blocks that
    /// were opened inside the parentheses: `(do stmts)`.
    bracket_depth: usize,
}

pub fn resolve_layout(tokens: Vec<Token>) -> Vec<Token> {
    let mut out: Vec<Token> = Vec::with_capacity(tokens.len() + tokens.len() / 4);
    let mut stack: Vec<Context> = Vec::new();
    let mut bracket_depth = 0usize;
    // Set after a layout keyword: the next token starts a block.
    let mut expecting_open: Option<&'static str> = None;
    let mut last_line = 0usize;

    let opened_kw = |tok: &Tok| -> &'static str {
        match tok.keyword() {
            Some("where") => "where",
            Some("do") => "do",
            Some("of") => "of",
            Some("let") => "let",
            Some("with") => "with",
            Some("catch") => "catch",
            _ => "",
        }
    };

    // A file that doesn't start with `module` opens an implicit top context
    // at its first token's column (Haskell rule for missing module headers).
    if let Some(first) = tokens.first() {
        if !first.tok.is_keyword("module") {
            stack.push(Context {
                col: first.pos.column,
                line: first.pos.line,
                opened_by: "module",
                bracket_depth: 0,
            });
            out.push(virtual_tok(Tok::VLBrace, first.pos));
            last_line = first.pos.line;
        }
    }

    let close = |out: &mut Vec<Token>, pos: Pos| {
        out.push(virtual_tok(Tok::VRBrace, pos));
    };

    for token in &tokens {
        let pos = token.pos;
        let col = pos.column;

        if let Some(kw) = expecting_open.take() {
            if matches!(token.tok, Tok::LBrace) {
                // Explicit block: push a no-offside context.
                stack.push(Context {
                    col: 0,
                    line: pos.line,
                    opened_by: kw,
                    bracket_depth,
                });
                out.push(token.clone());
                last_line = pos.line;
                continue;
            }
            let enclosing = stack.iter().rev().find(|c| c.col > 0).map_or(0, |c| c.col);
            if col > enclosing {
                stack.push(Context {
                    col,
                    line: pos.line,
                    opened_by: kw,
                    bracket_depth,
                });
                out.push(virtual_tok(Tok::VLBrace, pos));
                last_line = pos.line;
                // fall through to emit the token itself
            } else {
                // Token not indented past the enclosing block: empty block,
                // then let the normal offside logic below handle the token.
                out.push(virtual_tok(Tok::VLBrace, pos));
                out.push(virtual_tok(Tok::VRBrace, pos));
            }
        }

        // Offside check at the first token of each new line.
        let mut offside_closed_let = false;
        if pos.line != last_line {
            while let Some(top) = stack.last() {
                if top.col > 0 && col < top.col {
                    if top.opened_by == "let" {
                        offside_closed_let = true;
                    }
                    close(&mut out, pos);
                    stack.pop();
                } else {
                    break;
                }
            }
            // `where` never starts a new block item — the rule below closes
            // the block instead, so a VSemi here would be orphaned.
            if let Some(top) = stack.last() {
                if top.col > 0 && col == top.col && !token.tok.is_keyword("where") {
                    out.push(virtual_tok(Tok::VSemi, pos));
                }
            }
            last_line = pos.line;
        }

        // `where` closes any block at or right of it (`do ... where` at the
        // same indentation must end the do-block; GHC handles this via the
        // parse-error rule).
        if token.tok.is_keyword("where") {
            while let Some(top) = stack.last() {
                // Close blocks at/right of the `where`, and with-blocks
                // opened on the same line (`template S with p : Party
                // where ...` — the inline with-block ends at the where).
                if top.col > 0
                    && (top.col >= col
                        || (top.line == pos.line && top.opened_by == "with"))
                    && top.opened_by != "module"
                {
                    close(&mut out, pos);
                    stack.pop();
                } else {
                    break;
                }
            }
        }

        // `in` closes the matching `let` block when still open (same-line
        // `let x = 1 in x`). If the offside check above already closed the
        // matching let, the top context belongs to something enclosing —
        // leave it alone.
        if token.tok.is_keyword("in") && !offside_closed_let {
            if let Some(top) = stack.last() {
                if top.col > 0 && top.opened_by == "let" {
                    close(&mut out, pos);
                    stack.pop();
                }
            }
        }

        match token.tok {
            Tok::LParen | Tok::LBracket => bracket_depth += 1,
            Tok::RParen | Tok::RBracket | Tok::Comma => {
                // Close implicit blocks opened inside this bracket pair
                // before the bracket closes over them: `(do stmts)`,
                // `[f x, do y]`.
                let target = if matches!(token.tok, Tok::Comma) {
                    bracket_depth
                } else {
                    bracket_depth.saturating_sub(1)
                };
                while let Some(top) = stack.last() {
                    if top.col > 0 && top.bracket_depth > target {
                        close(&mut out, pos);
                        stack.pop();
                    } else {
                        break;
                    }
                }
                if !matches!(token.tok, Tok::Comma) {
                    bracket_depth = bracket_depth.saturating_sub(1);
                }
            }
            Tok::RBrace => {
                // Close an explicit context if one is on top.
                if stack.last().is_some_and(|c| c.col == 0) {
                    stack.pop();
                }
            }
            _ => {}
        }

        let was_backslash = out
            .last()
            .is_some_and(|t| matches!(&t.tok, Tok::Op(o) if o == "\\"));
        out.push(token.clone());

        if is_layout_keyword(&token.tok) {
            expecting_open = Some(opened_kw(&token.tok));
        } else if token.tok.is_keyword("case") && was_backslash {
            // `\case` alternatives form a layout block like `of`.
            expecting_open = Some("of");
        }
    }

    // EOF closes everything implicit.
    let eof = tokens.last().map_or(Pos { line: 1, column: 1 }, |t| t.pos);
    if expecting_open.is_some() {
        out.push(virtual_tok(Tok::VLBrace, eof));
        out.push(virtual_tok(Tok::VRBrace, eof));
    }
    for ctx in stack.iter().rev() {
        if ctx.col > 0 {
            out.push(virtual_tok(Tok::VRBrace, eof));
        }
    }

    out
}

fn virtual_tok(tok: Tok, pos: Pos) -> Token {
    Token { tok, pos }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::lex;

    /// Render the laid-out stream compactly: `{` `}` `;` for virtual tokens,
    /// token text otherwise.
    fn layout_str(src: &str) -> String {
        let (tokens, errors) = lex(src);
        assert!(errors.is_empty(), "lex errors: {:?}", errors);
        resolve_layout(tokens)
            .iter()
            .map(|t| match &t.tok {
                Tok::VLBrace => "{".to_string(),
                Tok::VRBrace => "}".to_string(),
                Tok::VSemi => ";".to_string(),
                Tok::LowerId { qualifier, name } | Tok::UpperId { qualifier, name } => {
                    match qualifier {
                        Some(q) => format!("{}.{}", q, name),
                        None => name.clone(),
                    }
                }
                Tok::Op(o) => o.clone(),
                Tok::IntLit(n) | Tok::DecimalLit(n) => n.clone(),
                Tok::StringLit(s) => format!("{:?}", s),
                Tok::CharLit(c) => format!("'{}'", c),
                Tok::LParen => "(".into(),
                Tok::RParen => ")".into(),
                Tok::LBracket => "[".into(),
                Tok::RBracket => "]".into(),
                Tok::LBrace => "{{".into(),
                Tok::RBrace => "}}".into(),
                Tok::Comma => ",".into(),
                Tok::Semi => ";;".into(),
                Tok::Backtick => "`".into(),
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn module_where_opens_top_block() {
        assert_eq!(
            layout_str("module M where\n\nf = 1\ng = 2\n"),
            "module M where { f = 1 ; g = 2 }"
        );
    }

    #[test]
    fn template_with_where_blocks() {
        let src = "module M where\n\ntemplate Foo\n  with\n    x : Int\n    y : Party\n  where\n    signatory y\n";
        assert_eq!(
            layout_str(src),
            "module M where { template Foo with { x : Int ; y : Party } where { signatory y } }"
        );
    }

    #[test]
    fn do_block_and_dedent() {
        let src = "module M where\nf = do\n  a\n  b\ng = 1\n";
        assert_eq!(
            layout_str(src),
            "module M where { f = do { a ; b } ; g = 1 }"
        );
    }

    #[test]
    fn same_line_record_with() {
        let src = "module M where\nf = do\n  cid <- create this with owner = p\n  pure cid\n";
        assert_eq!(
            layout_str(src),
            "module M where { f = do { cid <- create this with { owner = p } ; pure cid } }"
        );
    }

    #[test]
    fn let_in_same_line() {
        assert_eq!(
            layout_str("module M where\nf = let x = 1 in x\n"),
            "module M where { f = let { x = 1 } in x }"
        );
    }

    #[test]
    fn let_in_multiline() {
        let src = "module M where\nf =\n  let x = 1\n      y = 2\n  in x\n";
        assert_eq!(
            layout_str(src),
            "module M where { f = let { x = 1 ; y = 2 } in x }"
        );
    }

    #[test]
    fn paren_closes_do_block() {
        assert_eq!(
            layout_str("module M where\nf = g (do\n  a) b\n"),
            "module M where { f = g ( do { a } ) b }"
        );
    }

    #[test]
    fn where_at_do_indent_closes_do() {
        let src = "module M where\nf = do\n  a\n  where\n    g = 1\n";
        assert_eq!(
            layout_str(src),
            "module M where { f = do { a } where { g = 1 } }"
        );
    }

    #[test]
    fn file_without_module_header() {
        assert_eq!(layout_str("f = 1\ng = 2\n"), "{ f = 1 ; g = 2 }");
    }

    /// Phase gate: lexer + layout must survive the whole daml-finance
    /// corpus (no panic, no hang, balanced virtual braces). Skips silently
    /// when the corpus isn't present (CI without /tmp fixtures).
    #[test]
    fn corpus_lex_and_layout_survives() {
        let root = std::path::Path::new("/tmp/finance-lint-repo/daml");
        if !root.exists() {
            return;
        }
        let mut files = Vec::new();
        collect_daml(root, &mut files);
        assert!(files.len() > 600, "corpus incomplete: {} files", files.len());
        let mut lex_errors = 0usize;
        for f in &files {
            let src = std::fs::read_to_string(f).unwrap();
            let (tokens, errors) = lex(&src);
            lex_errors += errors.len();
            let laid = resolve_layout(tokens);
            let opens = laid.iter().filter(|t| t.tok == Tok::VLBrace).count();
            let closes = laid.iter().filter(|t| t.tok == Tok::VRBrace).count();
            assert_eq!(opens, closes, "unbalanced virtual braces in {:?}", f);
        }
        assert_eq!(lex_errors, 0, "lex errors across corpus");
    }

    fn collect_daml(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let p = entry.path();
            if p.is_dir() {
                collect_daml(&p, out);
            } else if p.extension().is_some_and(|e| e == "daml") {
                out.push(p);
            }
        }
    }

    #[test]
    fn case_of_alternatives() {
        let src = "module M where\nf x = case x of\n  1 -> a\n  _ -> b\n";
        assert_eq!(
            layout_str(src),
            "module M where { f x = case x of { 1 -> a ; _ -> b } }"
        );
    }
}
