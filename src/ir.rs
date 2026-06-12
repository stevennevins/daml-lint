use serde::Serialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize)]
pub struct Span {
    pub file: PathBuf,
    pub line: usize,
    pub column: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub enum DamlType {
    Party,
    Text,
    Decimal,
    Int,
    Bool,
    Date,
    Time,
    ContractId(Box<DamlType>),
    List(Box<DamlType>),
    Optional(Box<DamlType>),
    TextMap(Box<DamlType>),
    Map(Box<DamlType>, Box<DamlType>),
    Named(String),
    Unit,
    Unknown,
}

impl DamlType {
    pub fn from_str(s: &str) -> DamlType {
        let s = s.trim();
        if s == "Party" {
            DamlType::Party
        } else if s == "Text" {
            DamlType::Text
        } else if s == "Decimal" {
            DamlType::Decimal
        } else if s == "Int" {
            DamlType::Int
        } else if s == "Bool" {
            DamlType::Bool
        } else if s == "Date" {
            DamlType::Date
        } else if s == "Time" {
            DamlType::Time
        } else if s == "()" {
            DamlType::Unit
        } else if let Some(inner) = s.strip_prefix("ContractId ") {
            DamlType::ContractId(Box::new(DamlType::from_str(inner)))
        } else if let Some(inner) = s.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            DamlType::List(Box::new(DamlType::from_str(inner)))
        } else if let Some(inner) = s.strip_prefix("Optional ") {
            DamlType::Optional(Box::new(DamlType::from_str(inner)))
        } else if let Some(inner) = s.strip_prefix("TextMap ") {
            DamlType::TextMap(Box::new(DamlType::from_str(inner)))
        } else if s.starts_with(char::is_uppercase) {
            DamlType::Named(s.to_string())
        } else {
            DamlType::Unknown
        }
    }

    pub fn is_decimal(&self) -> bool {
        matches!(self, DamlType::Decimal)
    }

    pub fn is_text(&self) -> bool {
        matches!(self, DamlType::Text)
    }

    pub fn is_textmap(&self) -> bool {
        matches!(self, DamlType::TextMap(_))
    }

    pub fn is_list(&self) -> bool {
        matches!(self, DamlType::List(_))
    }

    pub fn is_unbounded(&self) -> bool {
        self.is_text() || self.is_textmap() || self.is_list()
    }
}

/// Lightweight source position for expression-level nodes (1-based). The
/// enclosing module fixes the file; repeating the path on every node would
/// bloat the JSON handed to rule scripts.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct SrcPos {
    pub line: usize,
    pub column: usize,
}

/// Expression AST exposed to rule scripts. Serialized as tagged unions:
/// `{ "App": {...} }`, `{ "Lit": {...} }`, ... mirrored by daml-lint.d.ts.
#[derive(Debug, Clone, Serialize)]
pub enum Expr {
    /// Variable reference: `amount`, `Map.lookup` (qualifier "Map").
    Var {
        name: String,
        qualifier: Option<String>,
        span: SrcPos,
    },
    /// Constructor or type reference in expression position: `Some`, `Iou`.
    Con {
        name: String,
        qualifier: Option<String>,
        span: SrcPos,
    },
    /// Literal; kind is "Int" | "Decimal" | "Text" | "Char".
    Lit {
        kind: String,
        value: String,
        span: SrcPos,
    },
    /// Application, flattened: `f a b` has two args.
    App {
        func: Box<Expr>,
        args: Vec<Expr>,
        span: SrcPos,
    },
    /// Binary operator with source-level operator text (`+`, `/`, `&&`,
    /// `` `div` `` for backtick application, `..` for ranges).
    BinOp {
        op: String,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
        span: SrcPos,
    },
    Neg {
        expr: Box<Expr>,
        span: SrcPos,
    },
    Lambda {
        params: Vec<String>,
        body: Box<Expr>,
        span: SrcPos,
    },
    If {
        cond: Box<Expr>,
        then_branch: Box<Expr>,
        else_branch: Box<Expr>,
        span: SrcPos,
    },
    Case {
        scrutinee: Box<Expr>,
        alts: Vec<CaseAlt>,
        span: SrcPos,
    },
    /// Nested do block, lowered to statements like a choice body.
    DoBlock {
        statements: Vec<Statement>,
        span: SrcPos,
    },
    LetIn {
        bindings: Vec<LetBinding>,
        body: Box<Expr>,
        span: SrcPos,
    },
    /// Record construction or update: `Foo with x = 1`, `this with owner`.
    Record {
        base: Box<Expr>,
        fields: Vec<RecordField>,
        span: SrcPos,
    },
    Tuple {
        items: Vec<Expr>,
        span: SrcPos,
    },
    List {
        items: Vec<Expr>,
        span: SrcPos,
    },
    /// Anything without a structured encoding (operator sections,
    /// comprehension qualifiers, recovered parse errors). `raw` preserves
    /// the source text.
    Unknown {
        raw: String,
        span: SrcPos,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct CaseAlt {
    /// Pattern rendered to source text (`Some x`, `[]`, `_`).
    pub pattern: String,
    pub body: Expr,
}

#[derive(Debug, Clone, Serialize)]
pub struct LetBinding {
    pub name: String,
    pub value: Expr,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecordField {
    pub name: String,
    /// None for punned fields (`Foo with owner`) and `..` spreads.
    pub value: Option<Expr>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Field {
    pub name: String,
    pub type_: DamlType,
    pub span: Span,
}

#[derive(Debug, Clone, Serialize)]
pub struct Template {
    pub name: String,
    pub fields: Vec<Field>,
    pub signatories: Vec<String>,
    pub observers: Vec<String>,
    /// Structured party expressions behind `signatories`/`observers`.
    pub signatory_exprs: Vec<Expr>,
    pub observer_exprs: Vec<Expr>,
    pub ensure_clause: Option<EnsureClause>,
    /// `key <expr> : <Type>` — expression and type text, if declared.
    pub key_expr: Option<Expr>,
    pub key_type: Option<String>,
    pub maintainer_exprs: Vec<Expr>,
    pub choices: Vec<Choice>,
    /// Interfaces this template implements (`interface instance I for T`).
    pub interface_instances: Vec<InterfaceInstance>,
    pub span: Span,
}

#[derive(Debug, Clone, Serialize)]
pub struct InterfaceInstance {
    pub interface_name: String,
    /// Implemented method names, in declaration order.
    pub methods: Vec<String>,
    pub span: Span,
}

#[derive(Debug, Clone, Serialize)]
pub struct EnsureClause {
    pub raw_text: String,
    /// Structured ensure condition.
    pub expr: Expr,
    pub span: Span,
}

impl EnsureClause {
    pub fn references_field_with_bound(&self, field_name: &str, bound: &str) -> bool {
        let text = &self.raw_text;
        text.contains(field_name) && text.contains(bound)
    }

    pub fn references_field(&self, field_name: &str) -> bool {
        self.raw_text.contains(field_name)
    }

    pub fn has_positive_bound(&self, field_name: &str) -> bool {
        let text = &self.raw_text;
        if !text.contains(field_name) {
            return false;
        }
        // Check for common positive bound patterns
        text.contains(&format!("{} > 0", field_name))
            || text.contains(&format!("{} > 0.0", field_name))
            || text.contains(&format!("{} >= 0", field_name))
            || text.contains(&format!("{} >= 0.0", field_name))
            || text.contains(&format!("0 < {}", field_name))
            || text.contains(&format!("0.0 < {}", field_name))
            || text.contains(&format!("0 <= {}", field_name))
            || text.contains(&format!("0.0 <= {}", field_name))
    }

    pub fn has_size_bound(&self, field_name: &str) -> bool {
        let text = &self.raw_text;
        if !text.contains(field_name) {
            return false;
        }
        text.contains(&format!("T.length {}", field_name))
            || text.contains(&format!("Text.length {}", field_name))
            || text.contains(&format!("length {}", field_name))
            || text.contains(&format!("Map.size {}", field_name))
            || text.contains(&format!("size {}", field_name))
            || text.contains(&format!("DA.Text.length {}", field_name))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Choice {
    pub name: String,
    pub consuming: bool,
    pub controllers: Vec<String>,
    /// Structured controller expressions behind `controllers`.
    pub controller_exprs: Vec<Expr>,
    /// Choice observers, if declared.
    pub observer_exprs: Vec<Expr>,
    pub parameters: Vec<Field>,
    pub return_type: DamlType,
    pub body: Vec<Statement>,
    pub body_raw: String,
    pub span: Span,
}

/// Do-statement classification. Raw-text fields (`expr`, `condition`,
/// `raw`) are kept for compatibility; the structured payloads (`value`,
/// `cid`, `argument`, ...) are the real parse tree.
#[derive(Debug, Clone, Serialize)]
pub enum Statement {
    Let {
        name: String,
        expr: String,
        value: Expr,
        span: SrcPos,
    },
    Assert {
        condition: String,
        condition_expr: Expr,
        span: SrcPos,
    },
    Fetch {
        cid_expr: String,
        cid: Expr,
        /// Pattern bound by `x <- fetch cid`, if any.
        binder: Option<String>,
        span: SrcPos,
    },
    Archive {
        cid_expr: String,
        cid: Expr,
        span: SrcPos,
    },
    Create {
        template_name: String,
        raw: String,
        /// The created payload (usually a Record expression).
        argument: Expr,
        binder: Option<String>,
        span: SrcPos,
    },
    Exercise {
        cid_expr: String,
        choice_name: String,
        raw: String,
        cid: Expr,
        /// The choice argument (usually a Record expression), if present.
        argument: Option<Expr>,
        binder: Option<String>,
        span: SrcPos,
    },
    TryCatch {
        try_body: Vec<Statement>,
        catch_body: Vec<Statement>,
        span: SrcPos,
    },
    Other {
        raw: String,
        /// Structured form of the statement expression.
        expr: Expr,
        binder: Option<String>,
        span: SrcPos,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct Function {
    pub name: String,
    /// Declared type signature text, if present.
    pub type_signature: Option<String>,
    pub body: Vec<Statement>,
    pub body_raw: String,
    pub span: Span,
}

#[derive(Debug, Clone, Serialize)]
pub struct Import {
    pub module_name: String,
    pub qualified: bool,
    pub alias: Option<String>,
    pub span: Span,
}

#[derive(Debug, Clone, Serialize)]
pub struct InterfaceMethod {
    pub name: String,
    pub type_text: String,
    pub span: Span,
}

#[derive(Debug, Clone, Serialize)]
pub struct Interface {
    pub name: String,
    /// Interfaces this interface requires.
    pub requires: Vec<String>,
    pub viewtype: Option<String>,
    pub methods: Vec<InterfaceMethod>,
    pub choices: Vec<Choice>,
    pub span: Span,
}

#[derive(Debug, Clone, Serialize)]
pub struct DamlModule {
    pub name: String,
    pub file: PathBuf,
    pub source: String,
    pub imports: Vec<Import>,
    pub templates: Vec<Template>,
    pub interfaces: Vec<Interface>,
    pub functions: Vec<Function>,
}
