//! Typed parse tree produced by the recursive-descent parser (src/parse.rs).
//!
//! Every node carries a source position. This is the parser-internal tree;
//! src/parser.rs lowers it onto the rule-facing IR (src/ir.rs).

pub use crate::lexer::Pos;

#[derive(Debug, Clone, PartialEq)]
pub enum LitKind {
    Int,
    Decimal,
    Text,
    Char,
}

#[derive(Debug, Clone)]
pub struct FieldAssign {
    pub name: String,
    /// None for record puns (`Foo with owner` meaning `owner = owner`)
    /// and `..` wildcards.
    pub value: Option<Expr>,
    pub pos: Pos,
}

#[derive(Debug, Clone)]
pub struct Alt {
    pub pat: Pat,
    pub body: Expr,
    pub pos: Pos,
}

#[derive(Debug, Clone)]
pub struct Binding {
    /// Left-hand side: a variable with parameters, or a destructuring pattern.
    pub pat: Pat,
    /// Parameter patterns when the LHS is a function binding (`f x y = ...`).
    pub params: Vec<Pat>,
    pub expr: Expr,
    pub pos: Pos,
}

#[derive(Debug, Clone)]
pub enum Pat {
    Var { name: String, pos: Pos },
    Wild { pos: Pos },
    Con { qualifier: Option<String>, name: String, args: Vec<Pat>, pos: Pos },
    Tuple { items: Vec<Pat>, pos: Pos },
    List { items: Vec<Pat>, pos: Pos },
    Lit { kind: LitKind, text: String, pos: Pos },
    /// `name@pat`
    As { name: String, pat: Box<Pat>, pos: Pos },
    /// Anything the parser couldn't classify; raw text preserved.
    Other { raw: String, pos: Pos },
}

#[derive(Debug, Clone)]
pub enum Expr {
    /// Lowercase variable reference, possibly qualified.
    Var { qualifier: Option<String>, name: String, pos: Pos },
    /// Constructor / data-constructor reference, possibly qualified.
    Con { qualifier: Option<String>, name: String, pos: Pos },
    Lit { kind: LitKind, text: String, pos: Pos },
    /// Application, flattened: `f a b c` is one App with three args.
    App { func: Box<Expr>, args: Vec<Expr>, pos: Pos },
    /// Binary operator application with source-level operator text.
    BinOp { op: String, lhs: Box<Expr>, rhs: Box<Expr>, pos: Pos },
    /// Unary negation.
    Neg { expr: Box<Expr>, pos: Pos },
    Lambda { params: Vec<Pat>, body: Box<Expr>, pos: Pos },
    If { cond: Box<Expr>, then_branch: Box<Expr>, else_branch: Box<Expr>, pos: Pos },
    Case { scrutinee: Box<Expr>, alts: Vec<Alt>, pos: Pos },
    Do { stmts: Vec<DoStmt>, pos: Pos },
    LetIn { bindings: Vec<Binding>, body: Box<Expr>, pos: Pos },
    /// `base with f = e, ...` — record construction when base is a Con,
    /// record update otherwise.
    Record { base: Box<Expr>, fields: Vec<FieldAssign>, pos: Pos },
    Tuple { items: Vec<Expr>, pos: Pos },
    List { items: Vec<Expr>, pos: Pos },
    /// `try <body> catch <alts>`
    Try { body: Box<Expr>, handlers: Vec<Alt>, pos: Pos },
    /// Right operator section like `(+ 1)` / left section `(1 +)`.
    Section { op: String, operand: Option<Box<Expr>>, left: bool, pos: Pos },
    /// Expression the parser could not understand; raw text preserved so
    /// a parse failure degrades to the shim's behavior instead of dying.
    Error { raw: String, pos: Pos },
}

#[derive(Debug, Clone)]
pub enum DoStmt {
    /// `pat <- expr`
    Bind { pat: Pat, expr: Expr, pos: Pos },
    /// `let x = e` (no `in`) inside a do block.
    Let { bindings: Vec<Binding>, pos: Pos },
    /// Bare expression statement.
    Expr { expr: Expr, pos: Pos },
}

#[derive(Debug, Clone)]
pub struct FieldDecl {
    pub name: String,
    /// Type rendered back to source-ish text (`ContractId Foo`, `[Text]`).
    pub type_text: String,
    pub pos: Pos,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Consuming {
    Consuming,
    NonConsuming,
    PreConsuming,
    PostConsuming,
}

#[derive(Debug, Clone)]
pub struct ChoiceDecl {
    pub name: String,
    pub consuming: Consuming,
    pub return_type_text: String,
    pub params: Vec<FieldDecl>,
    /// Comma-separated controller expressions.
    pub controllers: Vec<Expr>,
    /// Choice observers, if any.
    pub observers: Vec<Expr>,
    pub body: Option<Expr>,
    pub pos: Pos,
    /// Line of the last token of the choice (body_raw slicing).
    pub end_line: usize,
}

#[derive(Debug, Clone)]
pub enum TemplateBodyDecl {
    Signatory { parties: Vec<Expr>, pos: Pos },
    Observer { parties: Vec<Expr>, pos: Pos },
    Ensure { expr: Expr, pos: Pos },
    Key { expr: Expr, type_text: String, pos: Pos },
    Maintainer { expr: Expr, pos: Pos },
    Choice(ChoiceDecl),
    InterfaceInstance(InterfaceInstanceDecl),
    /// `agreement`, `let` blocks, deprecated `controller ... can`, etc.
    Other { raw: String, pos: Pos },
}

#[derive(Debug, Clone)]
pub struct InterfaceInstanceDecl {
    /// Interface being implemented (`Disclosure.I`).
    pub interface_name: String,
    /// Template it is for (from `for Foo`); the enclosing template when
    /// declared inside one.
    pub for_template: String,
    /// Method implementations: name → bound expression.
    pub methods: Vec<Binding>,
    pub pos: Pos,
}

#[derive(Debug, Clone)]
pub struct TemplateDecl {
    pub name: String,
    pub fields: Vec<FieldDecl>,
    pub body: Vec<TemplateBodyDecl>,
    pub pos: Pos,
}

#[derive(Debug, Clone)]
pub struct InterfaceDecl {
    pub name: String,
    /// Interfaces this interface requires (`requires Lockable.I, ...`).
    pub requires: Vec<String>,
    pub viewtype: Option<String>,
    /// Method signatures: name and type text.
    pub methods: Vec<FieldDecl>,
    pub choices: Vec<ChoiceDecl>,
    pub pos: Pos,
}

#[derive(Debug, Clone)]
pub struct Equation {
    pub params: Vec<Pat>,
    pub body: Expr,
    /// Guarded equations keep their guards as (guard, body) pairs; `body`
    /// then holds the first guarded body for convenience.
    pub guards: Vec<(Expr, Expr)>,
    /// `where` helper bindings attached to this equation.
    pub where_bindings: Vec<Binding>,
    pub pos: Pos,
}

#[derive(Debug, Clone)]
pub struct FunctionDecl {
    pub name: String,
    pub type_text: Option<String>,
    pub equations: Vec<Equation>,
    pub pos: Pos,
    /// Line of the last token of the last equation (body_raw slicing).
    pub end_line: usize,
}

#[derive(Debug, Clone)]
pub struct ImportDecl {
    pub module_name: String,
    pub qualified: bool,
    pub alias: Option<String>,
    pub pos: Pos,
}

#[derive(Debug, Clone)]
pub enum Decl {
    Template(TemplateDecl),
    Interface(InterfaceDecl),
    Function(FunctionDecl),
    /// data/type/class/instance/exception — recorded with name + span.
    TypeDef { keyword: String, name: String, pos: Pos },
    /// Anything unparseable at the top level (diagnostic already emitted).
    Unknown { raw: String, pos: Pos },
}

#[derive(Debug, Clone)]
pub struct Module {
    pub name: String,
    pub pos: Pos,
    pub imports: Vec<ImportDecl>,
    pub decls: Vec<Decl>,
}

/// Parse diagnostic — never fatal; the scan continues.
#[derive(Debug, Clone)]
pub struct ParseDiagnostic {
    pub message: String,
    pub pos: Pos,
}

impl Expr {
    pub fn pos(&self) -> Pos {
        match self {
            Expr::Var { pos, .. }
            | Expr::Con { pos, .. }
            | Expr::Lit { pos, .. }
            | Expr::App { pos, .. }
            | Expr::BinOp { pos, .. }
            | Expr::Neg { pos, .. }
            | Expr::Lambda { pos, .. }
            | Expr::If { pos, .. }
            | Expr::Case { pos, .. }
            | Expr::Do { pos, .. }
            | Expr::LetIn { pos, .. }
            | Expr::Record { pos, .. }
            | Expr::Tuple { pos, .. }
            | Expr::List { pos, .. }
            | Expr::Try { pos, .. }
            | Expr::Section { pos, .. }
            | Expr::Error { pos, .. } => *pos,
        }
    }

    /// Render back to compact source-like text (raw-field compatibility).
    pub fn render(&self) -> String {
        match self {
            Expr::Var { qualifier, name, .. } | Expr::Con { qualifier, name, .. } => {
                match qualifier {
                    Some(q) => format!("{}.{}", q, name),
                    None => name.clone(),
                }
            }
            Expr::Lit { kind, text, .. } => match kind {
                LitKind::Text => format!("{:?}", text),
                LitKind::Char => format!("'{}'", text),
                _ => text.clone(),
            },
            Expr::App { func, args, .. } => {
                let mut s = func.render_atomic();
                for a in args {
                    s.push(' ');
                    s.push_str(&a.render_atomic());
                }
                s
            }
            Expr::BinOp { op, lhs, rhs, .. } => {
                if op == "." {
                    // Record projection / composition: `account.custodian`.
                    format!("{}.{}", lhs.render_atomic(), rhs.render_atomic())
                } else {
                    format!("{} {} {}", lhs.render_atomic(), op, rhs.render_atomic())
                }
            }
            Expr::Neg { expr, .. } => format!("-{}", expr.render_atomic()),
            Expr::Lambda { params, body, .. } => {
                let ps: Vec<String> = params.iter().map(|p| p.render()).collect();
                format!("\\{} -> {}", ps.join(" "), body.render())
            }
            Expr::If { cond, then_branch, else_branch, .. } => format!(
                "if {} then {} else {}",
                cond.render(),
                then_branch.render(),
                else_branch.render()
            ),
            Expr::Case { scrutinee, alts, .. } => {
                let arms: Vec<String> = alts
                    .iter()
                    .map(|a| format!("{} -> {}", a.pat.render(), a.body.render()))
                    .collect();
                format!("case {} of {}", scrutinee.render(), arms.join("; "))
            }
            Expr::Do { stmts, .. } => {
                let body: Vec<String> = stmts.iter().map(render_do_stmt).collect();
                format!("do {}", body.join("; "))
            }
            Expr::LetIn { bindings, body, .. } => {
                let bs: Vec<String> = bindings.iter().map(render_binding).collect();
                format!("let {} in {}", bs.join("; "), body.render())
            }
            Expr::Record { base, fields, .. } => {
                let fs: Vec<String> = fields
                    .iter()
                    .map(|f| match &f.value {
                        Some(v) => format!("{} = {}", f.name, v.render()),
                        None => f.name.clone(),
                    })
                    .collect();
                format!("{} with {}", base.render_atomic(), fs.join("; "))
            }
            Expr::Tuple { items, .. } => {
                let xs: Vec<String> = items.iter().map(|e| e.render()).collect();
                format!("({})", xs.join(", "))
            }
            Expr::List { items, .. } => {
                let xs: Vec<String> = items.iter().map(|e| e.render()).collect();
                format!("[{}]", xs.join(", "))
            }
            Expr::Try { body, handlers, .. } => {
                let hs: Vec<String> = handlers
                    .iter()
                    .map(|a| format!("{} -> {}", a.pat.render(), a.body.render()))
                    .collect();
                format!("try {} catch {}", body.render(), hs.join("; "))
            }
            Expr::Section { op, operand, left, .. } => match (operand, left) {
                (Some(e), true) => format!("({} {})", e.render(), op),
                (Some(e), false) => format!("({} {})", op, e.render()),
                (None, _) => format!("({})", op),
            },
            Expr::Error { raw, .. } => raw.clone(),
        }
    }

    /// Render with parentheses if this expression wouldn't survive as an
    /// application argument.
    fn render_atomic(&self) -> String {
        match self {
            Expr::Var { .. }
            | Expr::Con { .. }
            | Expr::Lit { .. }
            | Expr::Tuple { .. }
            | Expr::List { .. }
            | Expr::Section { .. }
            | Expr::Error { .. } => self.render(),
            _ => format!("({})", self.render()),
        }
    }

    /// The head of an application spine: for `Foo.exercise cid X`, the
    /// `Foo.exercise` Var. For non-apps, the expression itself.
    pub fn app_head(&self) -> &Expr {
        match self {
            Expr::App { func, .. } => func.app_head(),
            _ => self,
        }
    }

    /// Application arguments, empty for non-apps.
    pub fn app_args(&self) -> &[Expr] {
        match self {
            Expr::App { args, .. } => args,
            _ => &[],
        }
    }

    /// Is the head an unqualified variable with this exact name?
    pub fn head_is(&self, name: &str) -> bool {
        matches!(
            self.app_head(),
            Expr::Var { qualifier: None, name: n, .. } if n == name
        )
    }
}

pub fn render_do_stmt(s: &DoStmt) -> String {
    match s {
        DoStmt::Bind { pat, expr, .. } => format!("{} <- {}", pat.render(), expr.render()),
        DoStmt::Let { bindings, .. } => {
            let bs: Vec<String> = bindings.iter().map(render_binding).collect();
            format!("let {}", bs.join("; "))
        }
        DoStmt::Expr { expr, .. } => expr.render(),
    }
}

pub fn render_binding(b: &Binding) -> String {
    let mut s = b.pat.render();
    for p in &b.params {
        s.push(' ');
        s.push_str(&p.render());
    }
    format!("{} = {}", s, b.expr.render())
}

impl Pat {
    pub fn pos(&self) -> Pos {
        match self {
            Pat::Var { pos, .. }
            | Pat::Wild { pos }
            | Pat::Con { pos, .. }
            | Pat::Tuple { pos, .. }
            | Pat::List { pos, .. }
            | Pat::Lit { pos, .. }
            | Pat::As { pos, .. }
            | Pat::Other { pos, .. } => *pos,
        }
    }

    pub fn render(&self) -> String {
        match self {
            Pat::Var { name, .. } => name.clone(),
            Pat::Wild { .. } => "_".to_string(),
            Pat::Con { qualifier, name, args, .. } => {
                let head = match qualifier {
                    Some(q) => format!("{}.{}", q, name),
                    None => name.clone(),
                };
                if args.is_empty() {
                    head
                } else {
                    let parts: Vec<String> = args.iter().map(|p| p.render()).collect();
                    format!("({} {})", head, parts.join(" "))
                }
            }
            Pat::Tuple { items, .. } => {
                let xs: Vec<String> = items.iter().map(|p| p.render()).collect();
                format!("({})", xs.join(", "))
            }
            Pat::List { items, .. } => {
                let xs: Vec<String> = items.iter().map(|p| p.render()).collect();
                format!("[{}]", xs.join(", "))
            }
            Pat::Lit { kind, text, .. } => match kind {
                LitKind::Text => format!("{:?}", text),
                LitKind::Char => format!("'{}'", text),
                _ => text.clone(),
            },
            Pat::As { name, pat, .. } => format!("{}@{}", name, pat.render()),
            Pat::Other { raw, .. } => raw.clone(),
        }
    }
}
