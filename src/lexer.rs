//! DAML lexer: source text → tokens with spans.
//!
//! First stage of the real parser pipeline (lexer → layout → parse). Comments
//! (line `--`, nested block `{- -}`) and string/char literals are resolved
//! here, so no later stage can ever mistake `-- exercise the option` for a
//! ledger action.

/// 1-based source position of a token's first character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pos {
    pub line: usize,
    pub column: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    /// Lowercase-initial identifier, possibly qualified: `foo`, `Map.lookup`.
    LowerId { qualifier: Option<String>, name: String },
    /// Uppercase-initial identifier, possibly qualified: `Foo`, `DA.Set.Set`.
    UpperId { qualifier: Option<String>, name: String },
    /// Symbolic operator: `+`, `<-`, `->`, `=`, `=>`, `::`, `.`, `\`, ...
    Op(String),
    IntLit(String),
    DecimalLit(String),
    StringLit(String),
    CharLit(String),
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Comma,
    Semi,
    Backtick,
    /// Layout-inserted virtual open brace (block start).
    VLBrace,
    /// Layout-inserted virtual close brace (block end).
    VRBrace,
    /// Layout-inserted virtual semicolon (new item at block indentation).
    VSemi,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub tok: Tok,
    pub pos: Pos,
}

/// A lexical error. The scan must survive these: the caller reports the
/// diagnostic and works with the tokens produced so far.
#[derive(Debug, Clone)]
pub struct LexError {
    pub message: String,
    pub pos: Pos,
}

fn is_symbol_char(c: char) -> bool {
    matches!(
        c,
        '!' | '#' | '$' | '%' | '&' | '*' | '+' | '.' | '/' | '<' | '=' | '>' | '?' | '@'
            | '\\' | '^' | '|' | '-' | '~' | ':'
    )
}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '\''
}

const TAB_STOP: usize = 8;

struct Lexer<'a> {
    chars: Vec<char>,
    src: &'a str,
    i: usize,
    line: usize,
    column: usize,
    tokens: Vec<Token>,
    errors: Vec<LexError>,
}

pub fn lex(source: &str) -> (Vec<Token>, Vec<LexError>) {
    let mut lx = Lexer {
        chars: source.chars().collect(),
        src: source,
        i: 0,
        line: 1,
        column: 1,
        tokens: Vec::new(),
        errors: Vec::new(),
    };
    lx.run();
    (lx.tokens, lx.errors)
}

impl<'a> Lexer<'a> {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.i).copied()
    }

    fn peek_at(&self, n: usize) -> Option<char> {
        self.chars.get(self.i + n).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.chars.get(self.i).copied()?;
        self.i += 1;
        match c {
            '\n' => {
                self.line += 1;
                self.column = 1;
            }
            '\t' => {
                // Tab advances to the next multiple-of-8 stop, matching GHC,
                // so mixed tabs/spaces don't silently corrupt layout.
                self.column = ((self.column - 1) / TAB_STOP + 1) * TAB_STOP + 1;
            }
            _ => self.column += 1,
        }
        Some(c)
    }

    fn pos(&self) -> Pos {
        Pos {
            line: self.line,
            column: self.column,
        }
    }

    fn push(&mut self, tok: Tok, pos: Pos) {
        self.tokens.push(Token { tok, pos });
    }

    fn error(&mut self, message: impl Into<String>, pos: Pos) {
        self.errors.push(LexError {
            message: message.into(),
            pos,
        });
    }

    fn run(&mut self) {
        while let Some(c) = self.peek() {
            let pos = self.pos();
            match c {
                ' ' | '\t' | '\n' | '\r' => {
                    self.bump();
                }
                '(' => {
                    self.bump();
                    self.push(Tok::LParen, pos);
                }
                ')' => {
                    self.bump();
                    self.push(Tok::RParen, pos);
                }
                '[' => {
                    self.bump();
                    self.push(Tok::LBracket, pos);
                }
                ']' => {
                    self.bump();
                    self.push(Tok::RBracket, pos);
                }
                ',' => {
                    self.bump();
                    self.push(Tok::Comma, pos);
                }
                ';' => {
                    self.bump();
                    self.push(Tok::Semi, pos);
                }
                '`' => {
                    self.bump();
                    self.push(Tok::Backtick, pos);
                }
                '{' => {
                    if self.peek_at(1) == Some('-') {
                        self.block_comment(pos);
                    } else {
                        self.bump();
                        self.push(Tok::LBrace, pos);
                    }
                }
                '}' => {
                    self.bump();
                    self.push(Tok::RBrace, pos);
                }
                '"' => self.string_lit(pos),
                '\'' => self.char_lit(pos),
                c if c.is_ascii_digit() => self.number(pos),
                c if is_ident_start(c) => self.identifier(pos),
                c if is_symbol_char(c) => self.operator(pos),
                _ => {
                    self.bump();
                    self.error(format!("unexpected character '{}'", c), pos);
                }
            }
        }
    }

    /// `{- ... -}`, nested as in Haskell. Unterminated comment is an error
    /// but consumes to EOF (no hang, no panic).
    fn block_comment(&mut self, pos: Pos) {
        self.bump(); // {
        self.bump(); // -
        let mut depth = 1usize;
        while depth > 0 {
            match self.peek() {
                None => {
                    self.error("unterminated block comment", pos);
                    return;
                }
                Some('{') if self.peek_at(1) == Some('-') => {
                    self.bump();
                    self.bump();
                    depth += 1;
                }
                Some('-') if self.peek_at(1) == Some('}') => {
                    self.bump();
                    self.bump();
                    depth -= 1;
                }
                Some(_) => {
                    self.bump();
                }
            }
        }
    }

    fn string_lit(&mut self, pos: Pos) {
        self.bump(); // opening "
        let mut value = String::new();
        loop {
            match self.peek() {
                None | Some('\n') => {
                    self.error("unterminated string literal", pos);
                    break;
                }
                Some('"') => {
                    self.bump();
                    break;
                }
                Some('\\') => {
                    self.bump();
                    match self.peek() {
                        // String gap: backslash, whitespace, backslash.
                        Some(w) if w.is_whitespace() => {
                            while self.peek().is_some_and(|c| c.is_whitespace()) {
                                self.bump();
                            }
                            if self.peek() == Some('\\') {
                                self.bump();
                            } else {
                                self.error("unterminated string gap", pos);
                                break;
                            }
                        }
                        Some(e) => {
                            self.bump();
                            value.push(unescape(e));
                        }
                        None => {
                            self.error("unterminated string literal", pos);
                            break;
                        }
                    }
                }
                Some(c) => {
                    self.bump();
                    value.push(c);
                }
            }
        }
        self.push(Tok::StringLit(value), pos);
    }

    /// `'a'`, `'\n'`, `'\x41'`. A lone `'` that doesn't close within a few
    /// chars is not a char literal (identifiers consume their own primes, so
    /// this only triggers at expression positions).
    fn char_lit(&mut self, pos: Pos) {
        // Lookahead: find closing quote within a short window.
        let mut j = self.i + 1;
        let mut escaped = false;
        let mut ok = false;
        let window_end = (self.i + 12).min(self.chars.len());
        while j < window_end {
            match self.chars[j] {
                '\\' if !escaped => escaped = true,
                '\'' if !escaped => {
                    ok = j > self.i + 1;
                    break;
                }
                '\n' => break,
                _ => escaped = false,
            }
            j += 1;
        }
        if !ok {
            self.bump();
            self.error("stray single quote", pos);
            return;
        }
        self.bump(); // opening '
        let mut value = String::new();
        while self.peek() != Some('\'') {
            let c = self.bump().unwrap();
            if c == '\\' {
                if let Some(e) = self.bump() {
                    value.push(unescape(e));
                }
            } else {
                value.push(c);
            }
        }
        self.bump(); // closing '
        self.push(Tok::CharLit(value), pos);
    }

    fn number(&mut self, pos: Pos) {
        let mut text = String::new();
        if self.peek() == Some('0')
            && matches!(self.peek_at(1), Some('x') | Some('X'))
        {
            text.push(self.bump().unwrap());
            text.push(self.bump().unwrap());
            while self.peek().is_some_and(|c| c.is_ascii_hexdigit() || c == '_') {
                text.push(self.bump().unwrap());
            }
            self.push(Tok::IntLit(text), pos);
            return;
        }
        while self.peek().is_some_and(|c| c.is_ascii_digit() || c == '_') {
            text.push(self.bump().unwrap());
        }
        let mut decimal = false;
        // `1.5` is a decimal but `1..5` or `1.foo` is not.
        if self.peek() == Some('.') && self.peek_at(1).is_some_and(|c| c.is_ascii_digit()) {
            decimal = true;
            text.push(self.bump().unwrap());
            while self.peek().is_some_and(|c| c.is_ascii_digit() || c == '_') {
                text.push(self.bump().unwrap());
            }
        }
        if matches!(self.peek(), Some('e') | Some('E'))
            && (self.peek_at(1).is_some_and(|c| c.is_ascii_digit())
                || (matches!(self.peek_at(1), Some('+') | Some('-'))
                    && self.peek_at(2).is_some_and(|c| c.is_ascii_digit())))
        {
            decimal = true;
            text.push(self.bump().unwrap());
            if matches!(self.peek(), Some('+') | Some('-')) {
                text.push(self.bump().unwrap());
            }
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                text.push(self.bump().unwrap());
            }
        }
        if decimal {
            self.push(Tok::DecimalLit(text), pos);
        } else {
            self.push(Tok::IntLit(text), pos);
        }
    }

    /// Identifiers, with greedy qualification: `DA.Set.fromList` is one
    /// token (qualifier "DA.Set", name "fromList").
    fn identifier(&mut self, pos: Pos) {
        let mut segments: Vec<String> = Vec::new();
        loop {
            let mut seg = String::new();
            while self.peek().is_some_and(is_ident_char) {
                seg.push(self.bump().unwrap());
            }
            let seg_is_upper = seg.chars().next().is_some_and(|c| c.is_uppercase());
            segments.push(seg);
            // Continue qualification only after an Upper segment: `Foo.bar`
            // is qualified, `foo.bar` is composition/projection.
            if seg_is_upper
                && self.peek() == Some('.')
                && self.peek_at(1).is_some_and(is_ident_start)
            {
                self.bump(); // .
                continue;
            }
            break;
        }
        let name = segments.pop().unwrap();
        let qualifier = if segments.is_empty() {
            None
        } else {
            Some(segments.join("."))
        };
        let tok = if name.chars().next().is_some_and(|c| c.is_uppercase()) {
            Tok::UpperId { qualifier, name }
        } else {
            Tok::LowerId { qualifier, name }
        };
        self.push(tok, pos);
    }

    fn operator(&mut self, pos: Pos) {
        let start = self.i;
        while self.peek().is_some_and(is_symbol_char) {
            // `{-` inside an operator run can't happen ({ isn't a symbol
            // char), but `--` comment detection needs the full run first.
            self.i += 1;
            self.column += 1;
        }
        let text: String = self.chars[start..self.i].iter().collect();
        // A run of 2+ dashes and nothing else is a line comment (Haskell
        // rule: `-->` is an operator, `--` and `---` start comments).
        if text.len() >= 2 && text.chars().all(|c| c == '-') {
            while self.peek().is_some_and(|c| c != '\n') {
                self.bump();
            }
            return;
        }
        self.push(Tok::Op(text), pos);
    }
}

fn unescape(c: char) -> char {
    match c {
        'n' => '\n',
        't' => '\t',
        'r' => '\r',
        '0' => '\0',
        other => other,
    }
}

impl Tok {
    /// The identifier text if this is an unqualified lowercase identifier —
    /// how the parser checks for (contextual) keywords.
    pub fn keyword(&self) -> Option<&str> {
        match self {
            Tok::LowerId {
                qualifier: None,
                name,
            } => Some(name.as_str()),
            _ => None,
        }
    }

    pub fn is_keyword(&self, kw: &str) -> bool {
        self.keyword() == Some(kw)
    }

    pub fn is_op(&self, op: &str) -> bool {
        matches!(self, Tok::Op(o) if o == op)
    }
}

// `src` field keeps the lexer borrow-tied to the source for future
// span-slicing; silence the lint until a consumer lands.
impl<'a> Lexer<'a> {
    #[allow(dead_code)]
    fn source(&self) -> &'a str {
        self.src
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(src: &str) -> Vec<Tok> {
        let (tokens, errors) = lex(src);
        assert!(errors.is_empty(), "lex errors: {:?}", errors);
        tokens.into_iter().map(|t| t.tok).collect()
    }

    fn lower(name: &str) -> Tok {
        Tok::LowerId {
            qualifier: None,
            name: name.to_string(),
        }
    }

    fn upper(name: &str) -> Tok {
        Tok::UpperId {
            qualifier: None,
            name: name.to_string(),
        }
    }

    #[test]
    fn line_comment_with_keywords_produces_no_tokens() {
        assert_eq!(toks("-- electing to exercise the option"), vec![]);
        assert_eq!(toks("--- template Foo"), vec![]);
    }

    #[test]
    fn arrow_like_operator_is_not_comment() {
        assert_eq!(toks("a --> b"), vec![lower("a"), Tok::Op("-->".into()), lower("b")]);
    }

    #[test]
    fn nested_block_comment() {
        assert_eq!(toks("{- outer {- inner -} still -} x"), vec![lower("x")]);
    }

    #[test]
    fn string_with_keyword_and_escapes() {
        assert_eq!(
            toks(r#""template \"Foo\" \n""#),
            vec![Tok::StringLit("template \"Foo\" \n".into())]
        );
    }

    #[test]
    fn qualified_identifiers() {
        assert_eq!(
            toks("DA.Set.fromList Map.Map foo"),
            vec![
                Tok::LowerId {
                    qualifier: Some("DA.Set".into()),
                    name: "fromList".into()
                },
                Tok::UpperId {
                    qualifier: Some("Map".into()),
                    name: "Map".into()
                },
                lower("foo"),
            ]
        );
    }

    #[test]
    fn numbers() {
        assert_eq!(
            toks("42 1.5 0x1F 2e3 1_000"),
            vec![
                Tok::IntLit("42".into()),
                Tok::DecimalLit("1.5".into()),
                Tok::IntLit("0x1F".into()),
                Tok::DecimalLit("2e3".into()),
                Tok::IntLit("1_000".into()),
            ]
        );
    }

    #[test]
    fn enum_from_to_is_not_decimal() {
        assert_eq!(
            toks("[1..5]"),
            vec![
                Tok::LBracket,
                Tok::IntLit("1".into()),
                Tok::Op("..".into()),
                Tok::IntLit("5".into()),
                Tok::RBracket,
            ]
        );
    }

    #[test]
    fn primes_stay_in_identifier_and_char_lit_works() {
        assert_eq!(
            toks(r"foo' 'a' '\n'"),
            vec![lower("foo'"), Tok::CharLit("a".into()), Tok::CharLit("\n".into())]
        );
    }

    #[test]
    fn operators_and_punctuation() {
        assert_eq!(
            toks("x <- f (y, z) `div` 2"),
            vec![
                lower("x"),
                Tok::Op("<-".into()),
                lower("f"),
                Tok::LParen,
                lower("y"),
                Tok::Comma,
                lower("z"),
                Tok::RParen,
                Tok::Backtick,
                lower("div"),
                Tok::Backtick,
                Tok::IntLit("2".into()),
            ]
        );
    }

    #[test]
    fn spans_are_one_based() {
        let (tokens, _) = lex("ab\n  cd");
        assert_eq!(tokens[0].pos, Pos { line: 1, column: 1 });
        assert_eq!(tokens[1].pos, Pos { line: 2, column: 3 });
    }

    #[test]
    fn tab_advances_to_stop() {
        let (tokens, _) = lex("\tx");
        assert_eq!(tokens[0].pos, Pos { line: 1, column: 9 });
    }

    #[test]
    fn unterminated_string_is_error_not_hang() {
        let (_, errors) = lex("x = \"oops\ny");
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn unterminated_block_comment_is_error_not_hang() {
        let (_, errors) = lex("{- never closed");
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn unicode_identifier() {
        assert_eq!(toks("ärger = 1"), vec![lower("ärger"), Tok::Op("=".into()), Tok::IntLit("1".into())]);
        let _ = upper("Ülf"); // helper used
    }
}
