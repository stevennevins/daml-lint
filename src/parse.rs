//! Recursive-descent parser: laid-out token stream → typed AST (src/ast.rs).
//!
//! Error recovery is per-declaration: an unparseable declaration becomes
//! `Decl::Unknown` plus a diagnostic, and parsing continues at the next
//! virtual semicolon. The parser never panics and never aborts the file.

use crate::ast::*;
use crate::layout::resolve_layout;
use crate::lexer::{lex, Pos, Tok, Token};

pub fn parse_module(source: &str) -> (Module, Vec<ParseDiagnostic>) {
    let (tokens, lex_errors) = lex(source);
    let tokens = resolve_layout(tokens);
    let mut p = Parser {
        toks: tokens,
        i: 0,
        depth: 0,
        diags: lex_errors
            .into_iter()
            .map(|e| ParseDiagnostic {
                message: e.message,
                pos: e.pos,
            })
            .collect(),
    };
    let module = p.module();
    (module, p.diags)
}

struct Parser {
    toks: Vec<Token>,
    i: usize,
    diags: Vec<ParseDiagnostic>,
    /// Expression/pattern recursion depth; bounded so hostile inputs
    /// (thousands of nested parens) cannot overflow the stack.
    depth: u32,
}

const MAX_DEPTH: u32 = 128;

impl Parser {
    // ----- cursor primitives -------------------------------------------

    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.i).map(|t| &t.tok)
    }

    fn peek_at(&self, n: usize) -> Option<&Tok> {
        self.toks.get(self.i + n).map(|t| &t.tok)
    }

    fn pos(&self) -> Pos {
        self.toks
            .get(self.i)
            .or_else(|| self.toks.last())
            .map_or(Pos { line: 1, column: 1 }, |t| t.pos)
    }

    fn prev_line(&self) -> usize {
        if self.i == 0 {
            1
        } else {
            self.toks[self.i - 1].pos.line
        }
    }

    fn bump(&mut self) -> Option<Token> {
        let t = self.toks.get(self.i).cloned();
        if t.is_some() {
            self.i += 1;
        }
        t
    }

    fn at_keyword(&self, kw: &str) -> bool {
        self.peek().is_some_and(|t| t.is_keyword(kw))
    }

    fn eat_keyword(&mut self, kw: &str) -> bool {
        if self.at_keyword(kw) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn at_op(&self, op: &str) -> bool {
        self.peek().is_some_and(|t| t.is_op(op))
    }

    fn eat_op(&mut self, op: &str) -> bool {
        if self.at_op(op) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn at(&self, tok: &Tok) -> bool {
        self.peek() == Some(tok)
    }

    fn eat(&mut self, tok: &Tok) -> bool {
        if self.at(tok) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn diag(&mut self, message: impl Into<String>) {
        let pos = self.pos();
        self.diags.push(ParseDiagnostic {
            message: message.into(),
            pos,
        });
    }

    /// Skip tokens until the end of the current block item: a VSemi or
    /// VRBrace at nesting depth zero (relative to here). Consumes neither.
    fn skip_to_item_end(&mut self) {
        let mut depth = 0usize;
        let mut brackets = 0usize;
        while let Some(t) = self.peek() {
            match t {
                Tok::VLBrace => depth += 1,
                Tok::VRBrace => {
                    if depth == 0 {
                        return;
                    }
                    depth -= 1;
                }
                Tok::VSemi if depth == 0 && brackets == 0 => return,
                Tok::LParen | Tok::LBracket | Tok::LBrace => brackets += 1,
                Tok::RParen | Tok::RBracket | Tok::RBrace => {
                    if brackets == 0 {
                        // Closing bracket of an enclosing construct: stop
                        // before it so the caller can match it.
                        return;
                    }
                    brackets -= 1;
                }
                _ => {}
            }
            self.i += 1;
        }
    }

    /// Raw text of tokens from `start` to the current position.
    fn slice_text(&self, start: usize) -> String {
        render_tokens(&self.toks[start..self.i])
    }

    // ----- module ------------------------------------------------------

    fn module(&mut self) -> Module {
        let pos = self.pos();
        let mut name = "Unknown".to_string();

        if self.eat_keyword("module") {
            if let Some(Tok::UpperId { qualifier, name: n }) = self.peek().cloned() {
                self.bump();
                name = match qualifier {
                    Some(q) => format!("{}.{}", q, n),
                    None => n,
                };
            }
            // Optional export list.
            if self.at(&Tok::LParen) {
                self.skip_balanced_parens();
            }
            if !self.eat_keyword("where") {
                self.diag("expected 'where' after module header");
            }
        }

        let mut imports = Vec::new();
        let mut decls: Vec<Decl> = Vec::new();

        let in_block = self.eat(&Tok::VLBrace) || self.eat(&Tok::LBrace);
        loop {
            while self.eat(&Tok::VSemi) || self.eat(&Tok::Semi) {}
            match self.peek() {
                None => break,
                Some(Tok::VRBrace) | Some(Tok::RBrace) => {
                    self.bump();
                    break;
                }
                _ => {}
            }
            let before = self.i;
            self.declaration(&mut imports, &mut decls);
            if self.i == before {
                // Defensive: guarantee progress even on a parser bug.
                self.bump();
            }
        }
        let _ = in_block;

        merge_functions(&mut decls);

        Module {
            name,
            pos,
            imports,
            decls,
        }
    }

    fn skip_balanced_parens(&mut self) {
        let mut depth = 0usize;
        while let Some(t) = self.peek() {
            match t {
                Tok::LParen => depth += 1,
                Tok::RParen => {
                    depth -= 1;
                    if depth == 0 {
                        self.i += 1;
                        return;
                    }
                }
                _ => {}
            }
            self.i += 1;
        }
    }

    fn declaration(&mut self, imports: &mut Vec<ImportDecl>, decls: &mut Vec<Decl>) {
        let pos = self.pos();
        let start = self.i;
        match self.peek() {
            Some(t) if t.is_keyword("import") => {
                if let Some(imp) = self.import_decl() {
                    imports.push(imp);
                }
                self.skip_to_item_end();
            }
            Some(t) if t.is_keyword("template") => {
                // `template T = ...` (template-let synonym) is exotic; only
                // `template Name with/where` is a template declaration.
                match self.template_decl() {
                    Some(t) => decls.push(Decl::Template(t)),
                    None => {
                        self.skip_to_item_end();
                        decls.push(Decl::Unknown {
                            raw: self.slice_text(start),
                            pos,
                        });
                    }
                }
            }
            Some(t) if t.is_keyword("interface") => {
                match self.interface_decl() {
                    Some(i) => decls.push(Decl::Interface(i)),
                    None => {
                        self.skip_to_item_end();
                        decls.push(Decl::Unknown {
                            raw: self.slice_text(start),
                            pos,
                        });
                    }
                }
            }
            Some(t)
                if matches!(
                    t.keyword(),
                    Some("infix" | "infixl" | "infixr")
                ) =>
            {
                self.skip_to_item_end();
            }
            Some(t)
                if matches!(
                    t.keyword(),
                    Some(
                        "data" | "type" | "newtype" | "class" | "instance" | "exception"
                            | "deriving"
                    )
                ) =>
            {
                let keyword = t.keyword().unwrap().to_string();
                self.bump();
                let name = match self.peek() {
                    Some(Tok::UpperId { qualifier, name }) => match qualifier {
                        Some(q) => format!("{}.{}", q, name),
                        None => name.clone(),
                    },
                    _ => String::new(),
                };
                self.skip_to_item_end();
                decls.push(Decl::TypeDef { keyword, name, pos });
            }
            Some(Tok::LowerId { .. }) => {
                match self.function_item() {
                    Some(d) => decls.push(d),
                    None => {
                        self.skip_to_item_end();
                        decls.push(Decl::Unknown {
                            raw: self.slice_text(start),
                            pos,
                        });
                    }
                }
            }
            // Operator definition or signature: `(<=) = curry Lte`.
            Some(Tok::LParen)
                if matches!(self.peek_at(1), Some(Tok::Op(_)))
                    && self.peek_at(2) == Some(&Tok::RParen) =>
            {
                self.skip_to_item_end();
                decls.push(Decl::Unknown {
                    raw: self.slice_text(start),
                    pos,
                });
            }
            // Top-level pattern binding: `[a, b, c] = ...`, `(x, y) = ...`.
            Some(Tok::LParen) | Some(Tok::LBracket) => {
                if self.binding().is_none() {
                    self.diag("unparseable top-level pattern binding");
                }
                self.skip_to_item_end();
                decls.push(Decl::Unknown {
                    raw: self.slice_text(start),
                    pos,
                });
            }
            _ => {
                self.diag(format!("unrecognized declaration: {:?}", self.peek()));
                self.skip_to_item_end();
                decls.push(Decl::Unknown {
                    raw: self.slice_text(start),
                    pos,
                });
            }
        }
    }

    // ----- imports -----------------------------------------------------

    fn import_decl(&mut self) -> Option<ImportDecl> {
        let pos = self.pos();
        self.bump(); // import
        let mut qualified = self.eat_keyword("qualified");
        let module_name = match self.peek().cloned() {
            Some(Tok::UpperId { qualifier, name }) => {
                self.bump();
                match qualifier {
                    Some(q) => format!("{}.{}", q, name),
                    None => name,
                }
            }
            _ => {
                self.diag("expected module name after 'import'");
                return None;
            }
        };
        // ImportQualifiedPost style: `import DA.Map qualified as Map`.
        if self.eat_keyword("qualified") {
            qualified = true;
        }
        let mut alias = None;
        if self.eat_keyword("as") {
            if let Some(Tok::UpperId { qualifier, name }) = self.peek().cloned() {
                self.bump();
                alias = Some(match qualifier {
                    Some(q) => format!("{}.{}", q, name),
                    None => name,
                });
            }
        }
        // `hiding (...)` / import list — consumed by skip_to_item_end.
        Some(ImportDecl {
            module_name,
            qualified,
            alias,
            pos,
        })
    }

    // ----- templates ---------------------------------------------------

    fn upper_name(&mut self) -> Option<String> {
        match self.peek().cloned() {
            Some(Tok::UpperId { qualifier, name }) => {
                self.bump();
                Some(match qualifier {
                    Some(q) => format!("{}.{}", q, name),
                    None => name,
                })
            }
            _ => None,
        }
    }

    fn template_decl(&mut self) -> Option<TemplateDecl> {
        let pos = self.pos();
        self.bump(); // template
        if self.at_keyword("instance") {
            return None; // legacy `template instance` — not a template
        }
        let name = self.upper_name()?;

        let mut fields = Vec::new();
        if self.eat_keyword("with") {
            fields = self.field_block();
        }
        let mut body = Vec::new();
        if self.eat_keyword("where") {
            body = self.template_body();
        }
        Some(TemplateDecl {
            name,
            fields,
            body,
            pos,
        })
    }

    /// `{ name : Type ; name2, name3 : Type ; ... }` (virtual or explicit).
    fn field_block(&mut self) -> Vec<FieldDecl> {
        let mut fields = Vec::new();
        if !(self.eat(&Tok::VLBrace) || self.eat(&Tok::LBrace)) {
            return fields;
        }
        loop {
            while self.eat(&Tok::VSemi) || self.eat(&Tok::Semi) {}
            match self.peek() {
                None => break,
                Some(Tok::VRBrace) | Some(Tok::RBrace) => {
                    self.bump();
                    break;
                }
                _ => {}
            }
            let before = self.i;
            // One or more comma-separated names, then `:`, then the type.
            let mut names: Vec<(String, Pos)> = Vec::new();
            loop {
                match self.peek().cloned() {
                    Some(Tok::LowerId {
                        qualifier: None,
                        name,
                    }) => {
                        let p = self.pos();
                        self.bump();
                        names.push((name, p));
                    }
                    _ => break,
                }
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
            if names.is_empty() || !self.eat_op(":") {
                self.diag("expected 'name : Type' field");
                self.skip_to_item_end();
                let _ = before;
                continue;
            }
            let ty_start = self.i;
            self.skip_to_item_end();
            let type_text = self.slice_text(ty_start);
            for (name, p) in names {
                fields.push(FieldDecl {
                    name,
                    type_text: type_text.clone(),
                    pos: p,
                });
            }
        }
        fields
    }

    fn template_body(&mut self) -> Vec<TemplateBodyDecl> {
        let mut body = Vec::new();
        if !(self.eat(&Tok::VLBrace) || self.eat(&Tok::LBrace)) {
            return body;
        }
        loop {
            while self.eat(&Tok::VSemi) || self.eat(&Tok::Semi) {}
            match self.peek() {
                None => break,
                Some(Tok::VRBrace) | Some(Tok::RBrace) => {
                    self.bump();
                    break;
                }
                _ => {}
            }
            let pos = self.pos();
            let start = self.i;
            let decl = self.template_body_item(pos, start);
            body.push(decl);
        }
        body
    }

    fn template_body_item(&mut self, pos: Pos, start: usize) -> TemplateBodyDecl {
        match self.peek().and_then(|t| t.keyword()) {
            Some("signatory") => {
                self.bump();
                let parties = self.expr_comma_list();
                self.skip_to_item_end();
                TemplateBodyDecl::Signatory { parties, pos }
            }
            Some("observer") => {
                self.bump();
                let parties = self.expr_comma_list();
                self.skip_to_item_end();
                TemplateBodyDecl::Observer { parties, pos }
            }
            Some("ensure") => {
                self.bump();
                let expr = self.expr();
                self.skip_to_item_end();
                TemplateBodyDecl::Ensure { expr, pos }
            }
            Some("key") => {
                self.bump();
                let expr_start = self.i;
                let expr = self.expr();
                let mut type_text = String::new();
                if self.eat_op(":") {
                    let ty_start = self.i;
                    self.skip_to_item_end();
                    type_text = self.slice_text(ty_start);
                } else {
                    // The expression parser consumes `: Type` annotations;
                    // recover the key type from the last top-level colon.
                    let mut depth = 0i32;
                    let mut colon = None;
                    for j in expr_start..self.i {
                        match &self.toks[j].tok {
                            Tok::LParen | Tok::LBracket => depth += 1,
                            Tok::RParen | Tok::RBracket => depth -= 1,
                            Tok::Op(o) if o == ":" && depth == 0 => colon = Some(j),
                            _ => {}
                        }
                    }
                    if let Some(j) = colon {
                        type_text = render_tokens(&self.toks[j + 1..self.i]);
                    }
                    self.skip_to_item_end();
                }
                TemplateBodyDecl::Key {
                    expr,
                    type_text,
                    pos,
                }
            }
            Some("maintainer") => {
                self.bump();
                let expr = self.expr();
                self.skip_to_item_end();
                TemplateBodyDecl::Maintainer { expr, pos }
            }
            Some("choice") | Some("nonconsuming") | Some("preconsuming")
            | Some("postconsuming") => match self.choice_decl() {
                Some(c) => TemplateBodyDecl::Choice(c),
                None => {
                    self.skip_to_item_end();
                    TemplateBodyDecl::Other {
                        raw: self.slice_text(start),
                        pos,
                    }
                }
            },
            Some("interface") => match self.interface_instance_decl() {
                Some(ii) => TemplateBodyDecl::InterfaceInstance(ii),
                None => {
                    self.skip_to_item_end();
                    TemplateBodyDecl::Other {
                        raw: self.slice_text(start),
                        pos,
                    }
                }
            },
            Some("controller") => {
                // Legacy Daml 1.x `controller <party> can` choice blocks are
                // not analyzed — fail loud instead of silently dropping the
                // choices inside.
                self.diag(
                    "legacy 'controller ... can' syntax is not supported; \
                     choices inside this block are not analyzed",
                );
                self.skip_to_item_end();
                TemplateBodyDecl::Other {
                    raw: self.slice_text(start),
                    pos,
                }
            }
            _ => {
                self.skip_to_item_end();
                TemplateBodyDecl::Other {
                    raw: self.slice_text(start),
                    pos,
                }
            }
        }
    }

    fn choice_decl(&mut self) -> Option<ChoiceDecl> {
        let pos = self.pos();
        let consuming = match self.peek().and_then(|t| t.keyword()) {
            Some("nonconsuming") => {
                self.bump();
                Consuming::NonConsuming
            }
            Some("preconsuming") => {
                self.bump();
                Consuming::PreConsuming
            }
            Some("postconsuming") => {
                self.bump();
                Consuming::PostConsuming
            }
            _ => Consuming::Consuming,
        };
        if !self.eat_keyword("choice") {
            return None;
        }
        let name = self.upper_name()?;
        let mut return_type_text = String::new();
        if self.eat_op(":") {
            let ty_start = self.i;
            self.skip_type_tokens();
            return_type_text = self.slice_text(ty_start);
        }
        let mut params = Vec::new();
        if self.eat_keyword("with") {
            params = self.field_block();
        }
        let mut observers = Vec::new();
        let mut controllers = Vec::new();
        loop {
            if self.eat_keyword("observer") {
                observers = self.expr_comma_list_no_do();
            } else if self.eat_keyword("controller") {
                controllers = self.expr_comma_list_no_do();
            } else {
                break;
            }
        }
        let body = if self.peek().is_some_and(|t| {
            !matches!(t, Tok::VSemi | Tok::VRBrace | Tok::Semi | Tok::RBrace)
        }) {
            Some(self.expr())
        } else {
            None
        };
        let end_line = self.prev_line();
        self.skip_to_item_end();
        Some(ChoiceDecl {
            name,
            consuming,
            return_type_text,
            params,
            controllers,
            observers,
            body,
            pos,
            end_line: end_line.max(pos.line),
        })
    }

    /// Consume type tokens up to (not including) a layout boundary or a
    /// `with`/`controller`/`observer`/`do`/`where` keyword at bracket depth 0.
    fn skip_type_tokens(&mut self) {
        let mut brackets = 0usize;
        while let Some(t) = self.peek() {
            match t {
                Tok::VSemi | Tok::VRBrace | Tok::VLBrace | Tok::Semi => return,
                Tok::LParen | Tok::LBracket => brackets += 1,
                Tok::RParen | Tok::RBracket => {
                    if brackets == 0 {
                        return;
                    }
                    brackets -= 1;
                }
                _ if brackets == 0
                    && matches!(
                        t.keyword(),
                        Some("with" | "controller" | "observer" | "do" | "where")
                    ) =>
                {
                    return
                }
                _ => {}
            }
            self.i += 1;
        }
    }

    // ----- interfaces ----------------------------------------------------

    fn interface_decl(&mut self) -> Option<InterfaceDecl> {
        let pos = self.pos();
        self.bump(); // interface
        if self.at_keyword("instance") {
            // Top-level retroactive interface instance: skip gracefully.
            return None;
        }
        let name = self.upper_name()?;
        let mut requires = Vec::new();
        if self.eat_keyword("requires") {
            while let Some(r) = self.upper_name() {
                requires.push(r);
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
        }
        if !self.eat_keyword("where") {
            return None;
        }
        let mut viewtype = None;
        let mut methods = Vec::new();
        let mut choices = Vec::new();
        if !(self.eat(&Tok::VLBrace) || self.eat(&Tok::LBrace)) {
            return Some(InterfaceDecl {
                name,
                requires,
                viewtype,
                methods,
                choices,
                pos,
            });
        }
        loop {
            while self.eat(&Tok::VSemi) || self.eat(&Tok::Semi) {}
            match self.peek() {
                None => break,
                Some(Tok::VRBrace) | Some(Tok::RBrace) => {
                    self.bump();
                    break;
                }
                _ => {}
            }
            match self.peek().and_then(|t| t.keyword()) {
                Some("viewtype") => {
                    self.bump();
                    viewtype = self.upper_name();
                    self.skip_to_item_end();
                }
                Some("choice") | Some("nonconsuming") | Some("preconsuming")
                | Some("postconsuming") => {
                    if let Some(c) = self.choice_decl() {
                        choices.push(c);
                    } else {
                        self.skip_to_item_end();
                    }
                }
                _ => {
                    // Method signature `name : Type`; anything else (default
                    // implementations, ensure, ...) is skipped.
                    let mpos = self.pos();
                    if let Some(Tok::LowerId {
                        qualifier: None,
                        name: mname,
                    }) = self.peek().cloned()
                    {
                        if self.peek_at(1).is_some_and(|t| t.is_op(":")) {
                            self.bump();
                            self.bump();
                            let ty_start = self.i;
                            self.skip_to_item_end();
                            methods.push(FieldDecl {
                                name: mname,
                                type_text: self.slice_text(ty_start),
                                pos: mpos,
                            });
                            continue;
                        }
                    }
                    self.skip_to_item_end();
                }
            }
        }
        Some(InterfaceDecl {
            name,
            requires,
            viewtype,
            methods,
            choices,
            pos,
        })
    }

    /// `interface instance I for T where { method-bindings }`
    fn interface_instance_decl(&mut self) -> Option<InterfaceInstanceDecl> {
        let pos = self.pos();
        self.bump(); // interface
        if !self.eat_keyword("instance") {
            return None;
        }
        let interface_name = self.upper_name()?;
        let mut for_template = String::new();
        if self.eat_keyword("for") {
            for_template = self.upper_name().unwrap_or_default();
        }
        let mut methods = Vec::new();
        if self.eat_keyword("where") && (self.eat(&Tok::VLBrace) || self.eat(&Tok::LBrace)) {
            loop {
                while self.eat(&Tok::VSemi) || self.eat(&Tok::Semi) {}
                match self.peek() {
                    None => break,
                    Some(Tok::VRBrace) | Some(Tok::RBrace) => {
                        self.bump();
                        break;
                    }
                    _ => {}
                }
                if let Some(b) = self.binding() {
                    methods.push(b);
                } else {
                    self.skip_to_item_end();
                }
            }
        }
        Some(InterfaceInstanceDecl {
            interface_name,
            for_template,
            methods,
            pos,
        })
    }

    // ----- functions -----------------------------------------------------

    /// A top-level item starting with a lowercase identifier: type
    /// signature or function equation. Operator definitions and other
    /// exotica return None.
    fn function_item(&mut self) -> Option<Decl> {
        let pos = self.pos();
        let name = match self.peek().cloned() {
            Some(Tok::LowerId {
                qualifier: None,
                name,
            }) => name,
            _ => return None,
        };

        // Type signature: `name [, name2] : Type`
        let mut j = self.i + 1;
        let mut is_sig = false;
        loop {
            match self.toks.get(j).map(|t| &t.tok) {
                Some(Tok::Comma) => {
                    j += 1;
                    if matches!(
                        self.toks.get(j).map(|t| &t.tok),
                        Some(Tok::LowerId { qualifier: None, .. })
                    ) {
                        j += 1;
                        continue;
                    }
                    break;
                }
                Some(Tok::Op(o)) if o == ":" => {
                    is_sig = true;
                    break;
                }
                _ => break,
            }
        }
        if is_sig {
            self.bump(); // name
            while self.eat(&Tok::Comma) {
                self.bump(); // more names
            }
            self.eat_op(":");
            let ty_start = self.i;
            self.skip_to_item_end();
            let type_text = self.slice_text(ty_start);
            return Some(Decl::Function(FunctionDecl {
                name,
                type_text: Some(type_text),
                equations: Vec::new(),
                pos,
                end_line: pos.line,
            }));
        }

        // Function equation: name pats (= expr | guards), optional where.
        self.bump(); // name
        let mut params = Vec::new();
        while !self.at_op("=") && !self.at_op("|") {
            match self.peek() {
                None | Some(Tok::VSemi) | Some(Tok::VRBrace) | Some(Tok::Semi)
                | Some(Tok::RBrace) => {
                    self.diag(format!("could not parse equation for '{}'", name));
                    return None;
                }
                _ => {}
            }
            match self.pattern_atom() {
                Some(p) => params.push(p),
                None => {
                    self.diag(format!("bad parameter pattern in '{}'", name));
                    return None;
                }
            }
        }
        let (body, guards) = self.equation_rhs()?;
        let mut where_bindings = Vec::new();
        if self.eat_keyword("where") {
            where_bindings = self.binding_block();
        }
        let end_line = self.prev_line();
        self.skip_to_item_end();
        Some(Decl::Function(FunctionDecl {
            name,
            type_text: None,
            equations: vec![Equation {
                params,
                body,
                guards,
                where_bindings,
                pos,
            }],
            pos,
            end_line: end_line.max(pos.line),
        }))
    }

    /// `= expr` or `| guard = expr | guard = expr ...`
    fn equation_rhs(&mut self) -> Option<(Expr, Vec<(Expr, Expr)>)> {
        if self.eat_op("=") {
            return Some((self.expr(), Vec::new()));
        }
        let mut guards = Vec::new();
        while self.eat_op("|") {
            // Comma-separated guard qualifiers, each a boolean expression
            // or a pattern guard `pat <- expr`.
            let g = loop {
                let g = self.expr();
                if self.eat_op("<-") {
                    let _ = self.expr(); // pattern guard: keep the pattern side
                }
                if !self.eat(&Tok::Comma) {
                    break g;
                }
            };
            if !self.eat_op("=") {
                self.diag("expected '=' after guard");
                return None;
            }
            let e = self.expr();
            guards.push((g, e));
        }
        let first = guards.first()?.1.clone();
        Some((first, guards))
    }

    /// `{ binding ; binding ; ... }` for let/where blocks.
    fn binding_block(&mut self) -> Vec<Binding> {
        let mut bindings = Vec::new();
        if !(self.eat(&Tok::VLBrace) || self.eat(&Tok::LBrace)) {
            return bindings;
        }
        loop {
            while self.eat(&Tok::VSemi) || self.eat(&Tok::Semi) {}
            match self.peek() {
                None => break,
                Some(Tok::VRBrace) | Some(Tok::RBrace) => {
                    self.bump();
                    break;
                }
                _ => {}
            }
            match self.binding() {
                Some(b) => bindings.push(b),
                None => self.skip_to_item_end(),
            }
        }
        bindings
    }

    /// One binding: `pat = expr`, `f x y = expr`, guarded variants, or a
    /// type signature (skipped, returns None).
    fn binding(&mut self) -> Option<Binding> {
        let pos = self.pos();
        let pat = self.pattern_atom()?;
        let mut params = Vec::new();
        loop {
            if self.at_op("=") {
                self.bump();
                let expr = self.expr();
                // Bindings can carry their own where blocks.
                if self.eat_keyword("where") {
                    let _ = self.binding_block();
                }
                return Some(Binding {
                    pat,
                    params,
                    expr,
                    pos,
                });
            }
            if self.at_op("|") {
                let (body, _) = self.equation_rhs()?;
                if self.eat_keyword("where") {
                    let _ = self.binding_block();
                }
                return Some(Binding {
                    pat,
                    params,
                    expr: body,
                    pos,
                });
            }
            if self.at_op(":") {
                // Type signature inside a let/where block — skip it.
                self.skip_to_item_end();
                return None;
            }
            match self.peek() {
                None | Some(Tok::VSemi) | Some(Tok::VRBrace) | Some(Tok::Semi)
                | Some(Tok::RBrace) => return None,
                _ => {}
            }
            params.push(self.pattern_atom()?);
        }
    }

    // ----- patterns ------------------------------------------------------

    fn pattern_atom(&mut self) -> Option<Pat> {
        if self.depth >= MAX_DEPTH {
            return None;
        }
        self.depth += 1;
        let result = self.pattern_atom_inner();
        self.depth -= 1;
        result
    }

    fn pattern_atom_inner(&mut self) -> Option<Pat> {
        let pos = self.pos();
        match self.peek().cloned() {
            Some(Tok::LowerId {
                qualifier: None,
                name,
            }) => {
                self.bump();
                if name == "_" {
                    return Some(Pat::Wild { pos });
                }
                if self.at_op("@") {
                    self.bump();
                    let inner = self.pattern_atom()?;
                    return Some(Pat::As {
                        name,
                        pat: Box::new(inner),
                        pos,
                    });
                }
                Some(Pat::Var { name, pos })
            }
            Some(Tok::Op(o)) if o == "_" => {
                self.bump();
                Some(Pat::Wild { pos })
            }
            Some(Tok::UpperId { qualifier, name }) => {
                self.bump();
                // Record pattern `Foo {..}` / `Foo {x = y}` /
                // `Foo with claim; tag`.
                if self.at(&Tok::LBrace) {
                    self.skip_balanced_braces();
                } else if self.eat_keyword("with") {
                    let _ = self.record_fields();
                }
                Some(Pat::Con {
                    qualifier,
                    name,
                    args: Vec::new(),
                    pos,
                })
            }
            Some(Tok::IntLit(text)) => {
                self.bump();
                Some(Pat::Lit {
                    kind: LitKind::Int,
                    text,
                    pos,
                })
            }
            Some(Tok::DecimalLit(text)) => {
                self.bump();
                Some(Pat::Lit {
                    kind: LitKind::Decimal,
                    text,
                    pos,
                })
            }
            Some(Tok::StringLit(text)) => {
                self.bump();
                Some(Pat::Lit {
                    kind: LitKind::Text,
                    text,
                    pos,
                })
            }
            Some(Tok::CharLit(text)) => {
                self.bump();
                Some(Pat::Lit {
                    kind: LitKind::Char,
                    text,
                    pos,
                })
            }
            Some(Tok::LParen) => {
                self.bump();
                if self.eat(&Tok::RParen) {
                    return Some(Pat::Con {
                        qualifier: None,
                        name: "()".to_string(),
                        args: Vec::new(),
                        pos,
                    });
                }
                let first = self.pattern()?;
                // Type-annotated pattern `(e : AnyException)`: skip the type.
                if self.at_op(":") {
                    let mut depth = 0usize;
                    while let Some(t) = self.peek() {
                        match t {
                            Tok::LParen | Tok::LBracket => depth += 1,
                            Tok::RParen if depth == 0 => break,
                            Tok::RParen | Tok::RBracket => depth -= 1,
                            Tok::VSemi | Tok::VRBrace => break,
                            _ => {}
                        }
                        self.i += 1;
                    }
                }
                if self.at(&Tok::Comma) {
                    let mut items = vec![first];
                    while self.eat(&Tok::Comma) {
                        items.push(self.pattern()?);
                    }
                    self.eat(&Tok::RParen);
                    return Some(Pat::Tuple { items, pos });
                }
                self.eat(&Tok::RParen);
                Some(first)
            }
            Some(Tok::LBracket) => {
                self.bump();
                let mut items = Vec::new();
                if !self.eat(&Tok::RBracket) {
                    loop {
                        items.push(self.pattern()?);
                        if !self.eat(&Tok::Comma) {
                            break;
                        }
                    }
                    self.eat(&Tok::RBracket);
                }
                Some(Pat::List { items, pos })
            }
            _ => None,
        }
    }

    /// Full pattern: constructor applications and infix cons `x :: xs`.
    fn pattern(&mut self) -> Option<Pat> {
        if self.depth >= MAX_DEPTH {
            return None;
        }
        self.depth += 1;
        let result = self.pattern_inner();
        self.depth -= 1;
        result
    }

    fn pattern_inner(&mut self) -> Option<Pat> {
        let pos = self.pos();
        let first = match self.peek().cloned() {
            Some(Tok::UpperId { qualifier, name }) => {
                self.bump();
                if self.at(&Tok::LBrace) || self.at_keyword("with") {
                    if self.eat_keyword("with") {
                        let _ = self.record_fields();
                    } else {
                        self.skip_balanced_braces();
                    }
                    Pat::Con {
                        qualifier,
                        name,
                        args: Vec::new(),
                        pos,
                    }
                } else {
                    let mut args = Vec::new();
                    while let Some(a) = self.try_pattern_atom() {
                        args.push(a);
                    }
                    Pat::Con {
                        qualifier,
                        name,
                        args,
                        pos,
                    }
                }
            }
            _ => self.pattern_atom()?,
        };
        if self.at_op("::") {
            self.bump();
            let rest = self.pattern()?;
            return Some(Pat::Con {
                qualifier: None,
                name: "::".to_string(),
                args: vec![first, rest],
                pos,
            });
        }
        Some(first)
    }

    fn try_pattern_atom(&mut self) -> Option<Pat> {
        match self.peek() {
            Some(Tok::LowerId { qualifier: None, .. })
            | Some(Tok::UpperId { .. })
            | Some(Tok::IntLit(_))
            | Some(Tok::DecimalLit(_))
            | Some(Tok::StringLit(_))
            | Some(Tok::CharLit(_))
            | Some(Tok::LParen)
            | Some(Tok::LBracket) => self.pattern_atom(),
            _ => None,
        }
    }

    fn skip_balanced_braces(&mut self) {
        let mut depth = 0usize;
        while let Some(t) = self.peek() {
            match t {
                Tok::LBrace => depth += 1,
                Tok::RBrace => {
                    depth -= 1;
                    if depth == 0 {
                        self.i += 1;
                        return;
                    }
                }
                _ => {}
            }
            self.i += 1;
        }
    }

    // ----- expressions ---------------------------------------------------

    fn expr(&mut self) -> Expr {
        self.expr_prec(0, true)
    }

    fn expr_no_do(&mut self) -> Expr {
        self.expr_prec(0, false)
    }

    /// Comma-separated expressions (signatory/observer/controller lists).
    fn expr_comma_list(&mut self) -> Vec<Expr> {
        let mut out = vec![self.expr()];
        while self.eat(&Tok::Comma) {
            out.push(self.expr());
        }
        out
    }

    fn expr_comma_list_no_do(&mut self) -> Vec<Expr> {
        let mut out = vec![self.expr_no_do()];
        while self.eat(&Tok::Comma) {
            out.push(self.expr_no_do());
        }
        out
    }

    fn expr_prec(&mut self, min_prec: u8, allow_do: bool) -> Expr {
        let pos = self.pos();
        if self.depth >= MAX_DEPTH {
            // Hostile nesting: degrade to raw text instead of recursing.
            let start = self.i;
            self.skip_to_item_end();
            if self.i == start {
                self.bump();
            }
            return Expr::Error {
                raw: self.slice_text(start),
                pos,
            };
        }
        self.depth += 1;
        let result = self.expr_prec_inner(min_prec, allow_do, pos);
        self.depth -= 1;
        result
    }

    fn expr_prec_inner(&mut self, min_prec: u8, allow_do: bool, pos: Pos) -> Expr {
        let mut lhs = match self.unary(allow_do) {
            Some(e) => e,
            None => {
                // Unparseable here: degrade to raw text up to the item end.
                let start = self.i;
                self.skip_to_item_end();
                if self.i == start {
                    self.bump();
                }
                return Expr::Error {
                    raw: self.slice_text(start),
                    pos,
                };
            }
        };
        loop {
            let (op, prec, right_assoc) = match self.peek() {
                Some(Tok::Op(o)) => {
                    let o = o.clone();
                    if is_reserved_op(&o) {
                        // `e : Type` annotation: consume the type, keep e.
                        if o == ":" {
                            self.bump();
                            self.skip_type_tokens();
                            continue;
                        }
                        break;
                    }
                    let (p, r) = fixity(&o);
                    (o, p, r)
                }
                Some(Tok::Backtick) => {
                    // `e `div` e` — infix function application.
                    let name = match self.peek_at(1) {
                        Some(Tok::LowerId { qualifier, name }) => match qualifier {
                            Some(q) => format!("{}.{}", q, name),
                            None => name.clone(),
                        },
                        Some(Tok::UpperId { qualifier, name }) => match qualifier {
                            Some(q) => format!("{}.{}", q, name),
                            None => name.clone(),
                        },
                        _ => break,
                    };
                    if self.peek_at(2) != Some(&Tok::Backtick) {
                        break;
                    }
                    (format!("`{}`", name), 9, false)
                }
                _ => break,
            };
            if prec < min_prec {
                break;
            }
            if op.starts_with('`') {
                self.bump();
                self.bump();
                self.bump();
            } else {
                self.bump();
            }
            let next_min = if right_assoc { prec } else { prec + 1 };
            let rhs = self.expr_prec(next_min, allow_do);
            lhs = Expr::BinOp {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                pos,
            };
        }
        lhs
    }

    fn unary(&mut self, allow_do: bool) -> Option<Expr> {
        let pos = self.pos();
        if self.at_op("-") {
            self.bump();
            let e = self.unary(allow_do)?;
            return Some(Expr::Neg {
                expr: Box::new(e),
                pos,
            });
        }
        self.application(allow_do)
    }

    fn application(&mut self, allow_do: bool) -> Option<Expr> {
        let pos = self.pos();
        let mut head = self.atom(allow_do)?;
        let mut args = Vec::new();
        loop {
            // Record syntax binds tighter than application:
            // `create Foo with x = 1` applies create to (Foo with {x = 1}).
            if self.at_keyword("with") {
                let target = if let Some(last) = args.pop() {
                    last
                } else {
                    std::mem::replace(
                        &mut head,
                        Expr::Error {
                            raw: String::new(),
                            pos,
                        },
                    )
                };
                self.bump(); // with
                let fields = self.record_fields();
                let tpos = target.pos();
                let rec = Expr::Record {
                    base: Box::new(target),
                    fields,
                    pos: tpos,
                };
                if matches!(head, Expr::Error { ref raw, .. } if raw.is_empty()) {
                    head = rec;
                } else {
                    args.push(rec);
                }
                continue;
            }
            if !allow_do && self.at_keyword("do") {
                break;
            }
            // Type application `f @Type x` — consume and drop the type atom.
            if self.at_op("@") {
                self.bump();
                match self.peek() {
                    Some(Tok::UpperId { .. }) | Some(Tok::LowerId { .. }) => {
                        self.bump();
                    }
                    Some(Tok::LParen) => self.skip_balanced_parens(),
                    Some(Tok::LBracket) => {
                        let mut depth = 0usize;
                        while let Some(t) = self.peek() {
                            match t {
                                Tok::LBracket => depth += 1,
                                Tok::RBracket => {
                                    depth -= 1;
                                    if depth == 0 {
                                        self.i += 1;
                                        break;
                                    }
                                }
                                _ => {}
                            }
                            self.i += 1;
                        }
                    }
                    _ => {}
                }
                continue;
            }
            match self.try_atom(allow_do) {
                Some(a) => args.push(a),
                None => break,
            }
        }
        if args.is_empty() {
            Some(head)
        } else {
            Some(Expr::App {
                func: Box::new(head),
                args,
                pos,
            })
        }
    }

    /// `{ f = e ; g ; .. }` after `with` (virtual block) or explicit braces.
    fn record_fields(&mut self) -> Vec<FieldAssign> {
        let mut fields = Vec::new();
        let explicit = self.at(&Tok::LBrace);
        if !(self.eat(&Tok::VLBrace) || self.eat(&Tok::LBrace)) {
            return fields;
        }
        loop {
            while self.eat(&Tok::VSemi) || self.eat(&Tok::Semi) || self.eat(&Tok::Comma) {}
            match self.peek() {
                None => break,
                Some(Tok::VRBrace) if !explicit => {
                    self.bump();
                    break;
                }
                Some(Tok::RBrace) => {
                    self.bump();
                    break;
                }
                _ => {}
            }
            let pos = self.pos();
            if self.at_op("..") {
                self.bump();
                fields.push(FieldAssign {
                    name: "..".to_string(),
                    value: None,
                    pos,
                });
                continue;
            }
            let name = match self.peek().cloned() {
                Some(Tok::LowerId {
                    qualifier: None,
                    name,
                }) => {
                    self.bump();
                    name
                }
                _ => {
                    self.skip_to_item_end();
                    continue;
                }
            };
            if self.eat_op("=") {
                let value = self.expr_prec(1, true);
                fields.push(FieldAssign {
                    name,
                    value: Some(value),
                    pos,
                });
            } else {
                // Pun: `Foo with owner`.
                fields.push(FieldAssign {
                    name,
                    value: None,
                    pos,
                });
            }
        }
        fields
    }

    fn try_atom(&mut self, allow_do: bool) -> Option<Expr> {
        match self.peek() {
            Some(Tok::LowerId { .. }) => {
                let kw = self.peek().and_then(|t| t.keyword());
                match kw {
                    // Block argument: `script do ...`, `submit p do ...`.
                    Some("do") if allow_do => self.atom(allow_do),
                    // Keywords that begin expressions are fine as atoms in
                    // head position but must not be slurped as arguments.
                    Some(
                        "if" | "case" | "do" | "let" | "try" | "where" | "then" | "else"
                        | "of" | "in" | "controller" | "with" | "catch",
                    ) => None,
                    _ => self.atom(allow_do),
                }
            }
            Some(Tok::UpperId { .. })
            | Some(Tok::IntLit(_))
            | Some(Tok::DecimalLit(_))
            | Some(Tok::StringLit(_))
            | Some(Tok::CharLit(_))
            | Some(Tok::LParen)
            | Some(Tok::LBracket) => self.atom(allow_do),
            // Bare trailing lambda argument: `forA xs \x -> ...`.
            Some(Tok::Op(o)) if o == "\\" => self.atom(allow_do),
            _ => None,
        }
    }

    fn atom(&mut self, allow_do: bool) -> Option<Expr> {
        let pos = self.pos();
        match self.peek().cloned() {
            Some(Tok::LowerId { qualifier, name }) => {
                match name.as_str() {
                    "if" if qualifier.is_none() => return self.if_expr(),
                    "case" if qualifier.is_none() => return self.case_expr(),
                    "do" if qualifier.is_none() => {
                        if !allow_do {
                            return None;
                        }
                        return self.do_expr();
                    }
                    "let" if qualifier.is_none() => return self.let_expr(),
                    "try" if qualifier.is_none() => return self.try_expr(),
                    _ => {}
                }
                self.bump();
                Some(Expr::Var {
                    qualifier,
                    name,
                    pos,
                })
            }
            Some(Tok::UpperId { qualifier, name }) => {
                self.bump();
                let base = Expr::Con {
                    qualifier,
                    name,
                    pos,
                };
                // Explicit-brace record syntax: `Foo {x = 1}`.
                if self.at(&Tok::LBrace) {
                    let fields = self.record_fields();
                    return Some(Expr::Record {
                        base: Box::new(base),
                        fields,
                        pos,
                    });
                }
                Some(base)
            }
            Some(Tok::IntLit(text)) => {
                self.bump();
                Some(Expr::Lit {
                    kind: LitKind::Int,
                    text,
                    pos,
                })
            }
            Some(Tok::DecimalLit(text)) => {
                self.bump();
                Some(Expr::Lit {
                    kind: LitKind::Decimal,
                    text,
                    pos,
                })
            }
            Some(Tok::StringLit(text)) => {
                self.bump();
                Some(Expr::Lit {
                    kind: LitKind::Text,
                    text,
                    pos,
                })
            }
            Some(Tok::CharLit(text)) => {
                self.bump();
                Some(Expr::Lit {
                    kind: LitKind::Char,
                    text,
                    pos,
                })
            }
            Some(Tok::Op(o)) if o == "\\" => self.lambda_expr(),
            Some(Tok::LParen) => self.paren_expr(),
            Some(Tok::LBracket) => self.list_expr(),
            _ => None,
        }
    }

    fn if_expr(&mut self) -> Option<Expr> {
        let pos = self.pos();
        self.bump(); // if
        let cond = self.expr();
        self.eat(&Tok::VSemi); // DoAndIfThenElse style
        if !self.eat_keyword("then") {
            self.diag("expected 'then'");
            return Some(Expr::Error {
                raw: format!("if {}", cond.render()),
                pos,
            });
        }
        let then_branch = self.expr();
        self.eat(&Tok::VSemi);
        if !self.eat_keyword("else") {
            self.diag("expected 'else'");
            return Some(Expr::Error {
                raw: format!("if {} then {}", cond.render(), then_branch.render()),
                pos,
            });
        }
        let else_branch = self.expr();
        Some(Expr::If {
            cond: Box::new(cond),
            then_branch: Box::new(then_branch),
            else_branch: Box::new(else_branch),
            pos,
        })
    }

    fn case_expr(&mut self) -> Option<Expr> {
        let pos = self.pos();
        self.bump(); // case
        let scrutinee = self.expr_no_do();
        if !self.eat_keyword("of") {
            self.diag("expected 'of' in case expression");
            return Some(Expr::Error {
                raw: format!("case {}", scrutinee.render()),
                pos,
            });
        }
        let mut alts = Vec::new();
        if self.eat(&Tok::VLBrace) || self.eat(&Tok::LBrace) {
            loop {
                while self.eat(&Tok::VSemi) || self.eat(&Tok::Semi) {}
                match self.peek() {
                    None => break,
                    Some(Tok::VRBrace) | Some(Tok::RBrace) => {
                        self.bump();
                        break;
                    }
                    _ => {}
                }
                // An alternative can carry a `where` block for its body.
                if self.eat_keyword("where") {
                    let _ = self.binding_block();
                    continue;
                }
                match self.case_alt() {
                    Some(a) => alts.push(a),
                    None => self.skip_to_item_end(),
                }
            }
        }
        Some(Expr::Case {
            scrutinee: Box::new(scrutinee),
            alts,
            pos,
        })
    }

    fn case_alt(&mut self) -> Option<Alt> {
        let pos = self.pos();
        let pat = self.pattern()?;
        if self.at_op("|") {
            // Guarded alternative(s): take the first body, consume all.
            let mut first: Option<Expr> = None;
            while self.eat_op("|") {
                let _guard = self.expr();
                if !self.eat_op("->") {
                    self.diag("expected '->' in guarded case alternative");
                    return None;
                }
                let body = self.expr();
                if first.is_none() {
                    first = Some(body);
                }
            }
            return Some(Alt {
                pat,
                body: first?,
                pos,
            });
        }
        if !self.eat_op("->") {
            self.diag("expected '->' in case alternative");
            return None;
        }
        let body = self.expr();
        Some(Alt { pat, body, pos })
    }

    fn do_expr(&mut self) -> Option<Expr> {
        let pos = self.pos();
        self.bump(); // do
        let mut stmts = Vec::new();
        if self.eat(&Tok::VLBrace) || self.eat(&Tok::LBrace) {
            loop {
                while self.eat(&Tok::VSemi) || self.eat(&Tok::Semi) {}
                match self.peek() {
                    None => break,
                    Some(Tok::VRBrace) | Some(Tok::RBrace) => {
                        self.bump();
                        break;
                    }
                    _ => {}
                }
                stmts.push(self.do_stmt());
            }
        }
        Some(Expr::Do { stmts, pos })
    }

    fn do_stmt(&mut self) -> DoStmt {
        let pos = self.pos();
        if self.at_keyword("let") {
            self.bump();
            let bindings = self.binding_block();
            // `let ... in body` as a statement is an expression.
            if self.eat_keyword("in") {
                let body = self.expr();
                return DoStmt::Expr {
                    expr: Expr::LetIn {
                        bindings,
                        body: Box::new(body),
                        pos,
                    },
                    pos,
                };
            }
            return DoStmt::Let { bindings, pos };
        }
        // Try `pat <- expr` with rollback.
        let snapshot = self.i;
        if let Some(pat) = self.try_bind_pattern() {
            if self.at_op("<-") {
                self.bump();
                let expr = self.expr();
                return DoStmt::Bind { pat, expr, pos };
            }
        }
        self.i = snapshot;
        let expr = self.expr();
        DoStmt::Expr { expr, pos }
    }

    /// Pattern attempt for `pat <- ...`; restores nothing itself (caller
    /// rolls back on failure).
    fn try_bind_pattern(&mut self) -> Option<Pat> {
        self.pattern()
    }

    fn let_expr(&mut self) -> Option<Expr> {
        let pos = self.pos();
        self.bump(); // let
        let bindings = self.binding_block();
        if self.eat_keyword("in") {
            let body = self.expr();
            return Some(Expr::LetIn {
                bindings,
                body: Box::new(body),
                pos,
            });
        }
        // `let` without `in` outside a do block — degrade gracefully.
        Some(Expr::LetIn {
            bindings,
            body: Box::new(Expr::Error {
                raw: String::new(),
                pos,
            }),
            pos,
        })
    }

    fn try_expr(&mut self) -> Option<Expr> {
        let pos = self.pos();
        self.bump(); // try
        let body = self.expr();
        let mut handlers = Vec::new();
        self.eat(&Tok::VSemi);
        if self.eat_keyword("catch") {
            if self.eat(&Tok::VLBrace) || self.eat(&Tok::LBrace) {
                loop {
                    while self.eat(&Tok::VSemi) || self.eat(&Tok::Semi) {}
                    match self.peek() {
                        None => break,
                        Some(Tok::VRBrace) | Some(Tok::RBrace) => {
                            self.bump();
                            break;
                        }
                        _ => {}
                    }
                    match self.case_alt() {
                        Some(a) => handlers.push(a),
                        None => self.skip_to_item_end(),
                    }
                }
            } else if let Some(a) = self.case_alt() {
                // Single-alternative catch on the same line.
                handlers.push(a);
            }
        }
        Some(Expr::Try {
            body: Box::new(body),
            handlers,
            pos,
        })
    }

    fn lambda_expr(&mut self) -> Option<Expr> {
        let pos = self.pos();
        self.bump(); // backslash
        let mut params = Vec::new();
        while !self.at_op("->") {
            match self.pattern_atom() {
                Some(p) => params.push(p),
                None => {
                    self.diag("bad lambda parameter");
                    let start = self.i;
                    self.skip_to_item_end();
                    return Some(Expr::Error {
                        raw: format!("\\{}", self.slice_text(start)),
                        pos,
                    });
                }
            }
        }
        self.bump(); // ->
        let body = self.expr();
        Some(Expr::Lambda {
            params,
            body: Box::new(body),
            pos,
        })
    }

    fn paren_expr(&mut self) -> Option<Expr> {
        let pos = self.pos();
        self.bump(); // (
        if self.eat(&Tok::RParen) {
            return Some(Expr::Con {
                qualifier: None,
                name: "()".to_string(),
                pos,
            });
        }
        // Operator section / operator reference: `(+)`, `(+ 1)`.
        if let Some(Tok::Op(o)) = self.peek().cloned() {
            if !is_reserved_op(&o) && o != "\\" && o != "-" {
                self.bump();
                if self.eat(&Tok::RParen) {
                    return Some(Expr::Section {
                        op: o,
                        operand: None,
                        left: false,
                        pos,
                    });
                }
                let operand = self.expr();
                self.eat(&Tok::RParen);
                return Some(Expr::Section {
                    op: o,
                    operand: Some(Box::new(operand)),
                    left: false,
                    pos,
                });
            }
        }
        let first = self.expr();
        if self.at(&Tok::Comma) {
            let mut items = vec![first];
            while self.eat(&Tok::Comma) {
                items.push(self.expr());
            }
            self.eat(&Tok::RParen);
            return Some(Expr::Tuple { items, pos });
        }
        // Left section: `(x +)`.
        if let Some(Tok::Op(o)) = self.peek().cloned() {
            if !is_reserved_op(&o) && self.peek_at(1) == Some(&Tok::RParen) {
                self.bump();
                self.bump();
                return Some(Expr::Section {
                    op: o,
                    operand: Some(Box::new(first)),
                    left: true,
                    pos,
                });
            }
        }
        self.eat(&Tok::RParen);
        Some(first)
    }

    fn list_expr(&mut self) -> Option<Expr> {
        let pos = self.pos();
        self.bump(); // [
        let mut items = Vec::new();
        if self.eat(&Tok::RBracket) {
            return Some(Expr::List { items, pos });
        }
        loop {
            let e = self.expr();
            // Range: `[a .. b]` / `[a ..]`.
            if self.at_op("..") {
                self.bump();
                let hi = if self.at(&Tok::RBracket) {
                    Expr::Error {
                        raw: String::new(),
                        pos,
                    }
                } else {
                    self.expr()
                };
                self.eat(&Tok::RBracket);
                return Some(Expr::BinOp {
                    op: "..".to_string(),
                    lhs: Box::new(e),
                    rhs: Box::new(hi),
                    pos,
                });
            }
            // List comprehension: degrade the qualifier part to raw text.
            if self.at_op("|") {
                let start = self.i;
                let mut brackets = 1usize;
                while let Some(t) = self.peek() {
                    match t {
                        Tok::LBracket => brackets += 1,
                        Tok::RBracket => {
                            brackets -= 1;
                            if brackets == 0 {
                                break;
                            }
                        }
                        Tok::VSemi | Tok::VRBrace => break,
                        _ => {}
                    }
                    self.i += 1;
                }
                let raw = self.slice_text(start);
                self.eat(&Tok::RBracket);
                return Some(Expr::App {
                    func: Box::new(e),
                    args: vec![Expr::Error { raw, pos }],
                    pos,
                });
            }
            items.push(e);
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.eat(&Tok::RBracket);
        Some(Expr::List { items, pos })
    }
}

/// Operators that structure declarations and can never be expression infix
/// operators.
fn is_reserved_op(op: &str) -> bool {
    matches!(op, "=" | "<-" | "->" | "|" | ":" | "=>" | "@" | "\\" | "..")
}

/// (precedence, right-assoc) — Haskell defaults; unknown operators get
/// infixl 9.
fn fixity(op: &str) -> (u8, bool) {
    match op {
        "$" | "$!" => (1, true),
        ">>=" | ">>" | "=<<" | "<&>" => (2, false),
        "||" => (3, true),
        "&&" => (4, true),
        "==" | "/=" | "<" | "<=" | ">" | ">=" => (5, false),
        "::" | "++" | "<>" => (6, true),
        "+" | "-" => (7, false),
        "*" | "/" => (8, false),
        "^" | "**" => (9, true),
        "." | "!!" => (10, true),
        _ => (9, false),
    }
}

/// Merge type signatures and successive equations of the same function into
/// one `Decl::Function`, preserving first-seen order.
fn merge_functions(decls: &mut Vec<Decl>) {
    let mut out: Vec<Decl> = Vec::with_capacity(decls.len());
    for decl in decls.drain(..) {
        match decl {
            Decl::Function(f) => {
                let existing = out.iter_mut().find_map(|d| match d {
                    Decl::Function(g) if g.name == f.name => Some(g),
                    _ => None,
                });
                match existing {
                    Some(g) => {
                        if g.type_text.is_none() {
                            g.type_text = f.type_text.clone();
                        }
                        // The function's reported position is its first
                        // equation, not its type signature.
                        if g.equations.is_empty() && !f.equations.is_empty() {
                            g.pos = f.pos;
                        }
                        g.equations.extend(f.equations);
                        g.end_line = g.end_line.max(f.end_line);
                    }
                    None => out.push(Decl::Function(f)),
                }
            }
            other => out.push(other),
        }
    }
    *decls = out;
}

/// Render a token slice back to compact source-like text.
pub fn render_tokens(toks: &[Token]) -> String {
    let mut s = String::new();
    let mut prev_no_space_after = true;
    for t in toks {
        let (text, no_space_before, no_space_after): (String, bool, bool) = match &t.tok {
            Tok::LowerId { qualifier, name } | Tok::UpperId { qualifier, name } => (
                match qualifier {
                    Some(q) => format!("{}.{}", q, name),
                    None => name.clone(),
                },
                false,
                false,
            ),
            Tok::Op(o) => (o.clone(), false, false),
            Tok::IntLit(n) | Tok::DecimalLit(n) => (n.clone(), false, false),
            Tok::StringLit(v) => (format!("{:?}", v), false, false),
            Tok::CharLit(v) => (format!("'{}'", v), false, false),
            Tok::LParen => ("(".to_string(), false, true),
            Tok::RParen => (")".to_string(), true, false),
            Tok::LBracket => ("[".to_string(), false, true),
            Tok::RBracket => ("]".to_string(), true, false),
            Tok::LBrace => ("{".to_string(), false, true),
            Tok::RBrace => ("}".to_string(), true, false),
            Tok::Comma => (",".to_string(), true, false),
            Tok::Semi | Tok::VSemi => (";".to_string(), true, false),
            Tok::Backtick => ("`".to_string(), false, false),
            Tok::VLBrace | Tok::VRBrace => continue,
        };
        if !s.is_empty() && !no_space_before && !prev_no_space_after {
            s.push(' ');
        }
        s.push_str(&text);
        prev_no_space_after = no_space_after;
    }
    s
}
