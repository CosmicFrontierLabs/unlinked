//! Native Rust code generation for the real matrix and character-array subset.
#[path = "script.rs"]
mod script;
use crate::Error;
pub use script::{eval_script, eval_script_with_budget};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Number(f64),
    Name(String),
    Text(String),
    Op(String),
    Newline,
    Eof,
}
#[derive(Clone, Debug)]
struct Spanned {
    token: Token,
    line: usize,
    space: bool,
}
fn error(line: usize, message: impl Into<String>) -> Error {
    Error {
        line,
        message: message.into(),
    }
}
fn lex(source: &str) -> Result<Vec<Spanned>, Error> {
    if source.len() > 262_144 {
        return Err(error(1, "array source exceeds 256 KiB limit"));
    }
    let chars: Vec<char> = source.chars().collect();
    let (mut i, mut line, mut space) = (0, 1, false);
    let mut out: Vec<Spanned> = Vec::new();
    while i < chars.len() {
        if out.len() >= 16_384 {
            return Err(error(line, "array source exceeds 16384-token limit"));
        }
        let c = chars[i];
        if c == '\n' {
            out.push(Spanned {
                token: Token::Newline,
                line,
                space,
            });
            line += 1;
            i += 1;
            space = true;
            continue;
        }
        if c.is_whitespace() {
            i += 1;
            space = true;
            continue;
        }
        if c == '%' {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if chars.get(i..i + 3) == Some(&['.', '.', '.']) {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            if i < chars.len() {
                i += 1;
                line += 1;
            }
            space = true;
            continue;
        }
        let token = if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            i += 1;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            Token::Name(chars[start..i].iter().collect())
        } else if c.is_ascii_digit()
            || (c == '.' && chars.get(i + 1).is_some_and(char::is_ascii_digit))
        {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            if chars.get(i) == Some(&'.')
                && !chars.get(i + 1).is_some_and(|c| "*/^\\'".contains(*c))
            {
                i += 1;
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
            }
            if matches!(chars.get(i), Some('e' | 'E')) {
                i += 1;
                if matches!(chars.get(i), Some('+' | '-')) {
                    i += 1;
                }
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
            }
            let s: String = chars[start..i].iter().collect();
            let n = s
                .parse::<f64>()
                .map_err(|_| error(line, "invalid numeric literal"))?;
            if !n.is_finite() {
                return Err(error(line, "numeric literal must be finite"));
            }
            Token::Number(n)
        } else if c == '\'' {
            let after_expr = out.last().is_some_and(|s| {
                matches!(&s.token, Token::Number(_) | Token::Name(_) | Token::Text(_))
                    || matches!(&s.token,Token::Op(op) if [")","]","'",".'"].contains(&op.as_str()))
            });
            if after_expr && !space {
                i += 1;
                Token::Op("'".into())
            } else {
                i += 1;
                let mut text = String::new();
                loop {
                    let c = *chars
                        .get(i)
                        .ok_or_else(|| error(line, "unterminated character literal"))?;
                    i += 1;
                    if c == '\n' {
                        return Err(error(line, "newline in character literal"));
                    }
                    if c == '\'' {
                        if chars.get(i) == Some(&'\'') {
                            text.push('\'');
                            i += 1;
                        } else {
                            break;
                        }
                    } else {
                        if !c.is_ascii() {
                            return Err(error(line, "only ASCII character arrays are supported"));
                        }
                        text.push(c);
                    }
                }
                Token::Text(text)
            }
        } else if "+-*/\\^=<>~&|():,;[].".contains(c) {
            i += 1;
            let mut op = c.to_string();
            if let Some(&next) = chars.get(i)
                && matches!(
                    (c, next),
                    ('=', '=')
                        | ('~', '=')
                        | ('<', '=')
                        | ('>', '=')
                        | ('&', '&')
                        | ('|', '|')
                        | ('.', '*')
                        | ('.', '/')
                        | ('.', '\\')
                        | ('.', '^')
                        | ('.', '\'')
                )
            {
                op.push(next);
                i += 1;
            }
            Token::Op(op)
        } else {
            return Err(error(line, format!("unsupported character {c:?}")));
        };
        out.push(Spanned { token, line, space });
        space = false;
    }
    out.push(Spanned {
        token: Token::Eof,
        line,
        space,
    });
    Ok(out)
}
#[derive(Debug)]
enum Expr {
    Number(f64),
    Text(String),
    Var(String),
    End,
    All,
    Unary(String, Box<Expr>),
    Binary(String, Box<Expr>, Box<Expr>),
    Range(Box<Expr>, Box<Expr>, Box<Expr>),
    Array(Vec<Vec<Expr>>),
    Apply(String, Vec<Expr>),
}
fn expression_depth(expr: &Expr) -> usize {
    match expr {
        Expr::Unary(_, value) => 1 + expression_depth(value),
        Expr::Binary(_, left, right) => 1 + expression_depth(left).max(expression_depth(right)),
        Expr::Range(a, b, c) => {
            1 + expression_depth(a)
                .max(expression_depth(b))
                .max(expression_depth(c))
        }
        Expr::Array(rows) => {
            1 + rows
                .iter()
                .flatten()
                .map(expression_depth)
                .max()
                .unwrap_or(0)
        }
        Expr::Apply(_, args) => 1 + args.iter().map(expression_depth).max().unwrap_or(0),
        _ => 1,
    }
}

#[derive(Debug)]
enum Target {
    Name(String),
    Index(String, Vec<Expr>),
    Many(Vec<String>),
}
#[derive(Debug)]
enum Stmt {
    Assign(Target, Expr),
    Call(Expr),
    If(Vec<(Expr, Vec<Stmt>)>, Vec<Stmt>),
    For(String, Expr, Vec<Stmt>),
    While(Expr, Vec<Stmt>),
    Break,
    Continue,
    Return,
}
struct Function {
    name: String,
    args: Vec<String>,
    outputs: Vec<String>,
    body: Vec<Stmt>,
}
struct Parser {
    tokens: Vec<Spanned>,
    pos: usize,
    depth: usize,
}
impl Parser {
    fn token(&self) -> &Token {
        &self.tokens[self.pos].token
    }
    fn fail(&self, message: impl Into<String>) -> Error {
        error(self.tokens[self.pos].line, message)
    }
    fn named(&self, name: &str) -> bool {
        matches!(self.token(),Token::Name(n) if n==name)
    }
    fn at(&self, op: &str) -> bool {
        matches!(self.token(),Token::Op(o) if o==op)
    }
    fn op(&mut self, op: &str) -> bool {
        if self.at(op) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn expect(&mut self, op: &str) -> Result<(), Error> {
        if self.op(op) {
            Ok(())
        } else {
            Err(self.fail(format!("expected '{op}'")))
        }
    }
    fn name(&mut self) -> Result<String, Error> {
        if let Token::Name(n) = self.token() {
            let n = n.clone();
            self.pos += 1;
            Ok(n)
        } else {
            Err(self.fail("expected identifier"))
        }
    }
    fn separators(&mut self) {
        while matches!(self.token(), Token::Newline) || self.at(";") || self.at(",") {
            self.pos += 1;
        }
    }
    fn newline(&mut self) {
        while matches!(self.token(), Token::Newline) {
            self.pos += 1;
        }
    }
    fn finish(&mut self) -> Result<(), Error> {
        if matches!(self.token(), Token::Newline | Token::Eof) || self.at(";") || self.at(",") {
            self.separators();
            Ok(())
        } else {
            Err(self.fail("expected statement separator"))
        }
    }
    fn expr(&mut self, min: u8, in_array: bool) -> Result<Expr, Error> {
        if self.depth >= 64 {
            return Err(self.fail("expression nesting exceeds 64 levels"));
        }
        self.depth += 1;
        let result = self.expr_inner(min, in_array);
        self.depth -= 1;
        result
    }
    fn expr_inner(&mut self, min: u8, in_array: bool) -> Result<Expr, Error> {
        let mut lhs = match self.token().clone() {
            Token::Number(n) => {
                self.pos += 1;
                Expr::Number(n)
            }
            Token::Text(s) => {
                self.pos += 1;
                Expr::Text(s)
            }
            Token::Name(n) => {
                self.pos += 1;
                if n == "end" { Expr::End } else { Expr::Var(n) }
            }
            Token::Op(op) if ["+", "-", "~"].contains(&op.as_str()) => {
                self.pos += 1;
                Expr::Unary(op, Box::new(self.expr(17, in_array)?))
            }
            Token::Op(op) if op == "(" => {
                self.pos += 1;
                let e = self.expr(0, false)?;
                self.expect(")")?;
                e
            }
            Token::Op(op) if op == "[" => {
                self.pos += 1;
                let mut rows = vec![];
                let mut row = vec![];
                self.newline();
                if !self.op("]") {
                    loop {
                        row.push(self.expr(0, true)?);
                        if self.op("]") {
                            rows.push(row);
                            break;
                        }
                        if self.op(";") || matches!(self.token(), Token::Newline) {
                            self.newline();
                            rows.push(std::mem::take(&mut row));
                            if self.op("]") {
                                break;
                            }
                        } else if self.op(",") {
                            self.newline();
                        } else if !self.tokens[self.pos].space {
                            return Err(self.fail("expected matrix element or row separator"));
                        }
                    }
                }
                Expr::Array(rows)
            }
            Token::Op(op) if op == ":" => {
                self.pos += 1;
                Expr::All
            }
            _ => return Err(self.fail("expected matrix or scalar expression")),
        };
        let mut operators = 0;
        loop {
            if expression_depth(&lhs) > 256 {
                return Err(self.fail("expression tree exceeds 256 levels"));
            }
            if operators >= 256 {
                return Err(self.fail("expression exceeds 256 operators"));
            }
            if min <= 21 && self.at("(") {
                self.pos += 1;
                let Expr::Var(name) = lhs else {
                    return Err(
                        self.fail("indexing a temporary result is unsupported; assign it first")
                    );
                };
                let mut args = Vec::new();
                self.newline();
                if !self.op(")") {
                    loop {
                        args.push(self.expr(0, false)?);
                        self.newline();
                        if self.op(")") {
                            break;
                        }
                        self.expect(",")?;
                        self.newline();
                    }
                }
                lhs = Expr::Apply(name, args);
                operators += 1;
                continue;
            }
            if min <= 21 && (self.at("'") || self.at(".'")) {
                let Token::Op(op) = self.token().clone() else {
                    unreachable!()
                };
                self.pos += 1;
                lhs = Expr::Unary(op, Box::new(lhs));
                operators += 1;
                continue;
            }
            let Token::Op(op) = self.token() else {
                break;
            };
            let (left, right) = match op.as_str() {
                "||" => (1, 2),
                "&&" => (3, 4),
                "|" => (5, 6),
                "&" => (7, 8),
                "==" | "~=" | "<" | ">" | "<=" | ">=" => (9, 10),
                ":" => (11, 12),
                "+" | "-" => (13, 14),
                "*" | "/" | "\\" | ".*" | "./" | ".\\" => (15, 16),
                "^" | ".^" => (19, 20),
                _ => break,
            };
            if left < min {
                break;
            }
            if in_array
                && (op == "+" || op == "-")
                && self.tokens[self.pos].space
                && !self.tokens.get(self.pos + 1).is_none_or(|t| t.space)
            {
                break;
            }
            let op = op.clone();
            self.pos += 1;
            let rhs = self.expr(right, in_array)?;
            lhs = if op == ":" {
                let (step, stop) = if self.op(":") {
                    (rhs, self.expr(right, in_array)?)
                } else {
                    (Expr::Number(1.0), rhs)
                };
                Expr::Range(Box::new(lhs), Box::new(step), Box::new(stop))
            } else {
                Expr::Binary(op, Box::new(lhs), Box::new(rhs))
            };
            operators += 1;
        }
        Ok(lhs)
    }
    fn body(&mut self) -> Result<Vec<Stmt>, Error> {
        if self.depth >= 64 {
            return Err(self.fail("statement nesting exceeds 64 levels"));
        }
        self.depth += 1;
        let result = self.body_inner();
        self.depth -= 1;
        result
    }
    fn body_inner(&mut self) -> Result<Vec<Stmt>, Error> {
        let mut result = Vec::new();
        self.separators();
        while !matches!(self.token(), Token::Eof)
            && !["end", "else", "elseif", "function"]
                .iter()
                .any(|n| self.named(n))
        {
            result.push(self.statement()?);
        }
        Ok(result)
    }
    fn end(&mut self) -> Result<(), Error> {
        if !self.named("end") {
            return Err(self.fail("expected 'end'"));
        }
        self.pos += 1;
        self.finish()
    }
    fn statement(&mut self) -> Result<Stmt, Error> {
        if self.named("if") {
            self.pos += 1;
            let condition = self.expr(0, false)?;
            self.finish()?;
            let mut branches = vec![(condition, self.body()?)];
            while self.named("elseif") {
                self.pos += 1;
                let condition = self.expr(0, false)?;
                self.finish()?;
                branches.push((condition, self.body()?));
            }
            let other = if self.named("else") {
                self.pos += 1;
                if !self.named("if") {
                    self.finish()?;
                }
                self.body()?
            } else {
                vec![]
            };
            self.end()?;
            return Ok(Stmt::If(branches, other));
        }
        if self.named("for") {
            self.pos += 1;
            let name = self.name()?;
            self.expect("=")?;
            let values = self.expr(0, false)?;
            self.finish()?;
            let body = self.body()?;
            self.end()?;
            return Ok(Stmt::For(name, values, body));
        }
        if self.named("while") {
            self.pos += 1;
            let condition = self.expr(0, false)?;
            self.finish()?;
            let body = self.body()?;
            self.end()?;
            return Ok(Stmt::While(condition, body));
        }
        for (name, stmt) in [
            ("break", Stmt::Break),
            ("continue", Stmt::Continue),
            ("return", Stmt::Return),
        ] {
            if self.named(name) {
                self.pos += 1;
                self.finish()?;
                return Ok(stmt);
            }
        }
        if self.at("[") {
            let checkpoint = self.pos;
            self.pos += 1;
            let mut names = Vec::new();
            let mut valid = true;
            while !self.op("]") {
                if let Token::Name(name) = self.token() {
                    names.push(name.clone());
                    self.pos += 1;
                    self.op(",");
                } else {
                    valid = false;
                    break;
                }
            }
            if valid && self.op("=") {
                if names.is_empty() {
                    return Err(self.fail("empty output list unsupported"));
                }
                let value = self.expr(0, false)?;
                self.finish()?;
                return Ok(Stmt::Assign(Target::Many(names), value));
            }
            self.pos = checkpoint;
        }
        let lhs = self.expr(0, false)?;
        let result = if self.op("=") {
            let target = match lhs {
                Expr::Var(n) => Target::Name(n),
                Expr::Apply(n, args) => Target::Index(n, args),
                _ => return Err(self.fail("invalid assignment target")),
            };
            Stmt::Assign(target, self.expr(0, false)?)
        } else {
            if !matches!(lhs, Expr::Apply(..)) {
                return Err(
                    self.fail("bare expression display is unsupported; use disp(expression)")
                );
            }
            Stmt::Call(lhs)
        };
        self.finish()?;
        Ok(result)
    }
    fn function(&mut self) -> Result<Function, Error> {
        self.pos += 1;
        let mut outputs = Vec::new();
        let name;
        if self.op("[") {
            while !self.op("]") {
                let n = self.name()?;
                if outputs.contains(&n) {
                    return Err(self.fail("duplicate output variable"));
                }
                outputs.push(n);
                self.op(",");
            }
            self.expect("=")?;
            name = self.name()?;
        } else {
            let first = self.name()?;
            if self.op("=") {
                outputs.push(first);
                name = self.name()?;
            } else {
                name = first;
            }
        }
        self.expect("(")?;
        let mut args = Vec::new();
        if !self.op(")") {
            loop {
                let name = self.name()?;
                if args.contains(&name) {
                    return Err(self.fail("duplicate function parameter"));
                }
                args.push(name);
                if self.op(")") {
                    break;
                }
                self.expect(",")?;
            }
        }
        self.finish()?;
        let body = self.body()?;
        self.end()?;
        Ok(Function {
            name,
            args,
            outputs,
            body,
        })
    }
}
const BUILTINS: &[&str] = &[
    "disp",
    "fprintf",
    "sprintf",
    "error",
    "assert",
    "numel",
    "length",
    "isempty",
    "size",
    "zeros",
    "ones",
    "eye",
    "reshape",
    "transpose",
    "num2str",
    "strcmp",
    "linspace",
    "diag",
    "sum",
    "prod",
    "all",
    "any",
    "min",
    "max",
    "mod",
    "rem",
    "atan2",
    "norm",
    "dot",
    "sort",
    "find",
    "abs",
    "sin",
    "cos",
    "tan",
    "asin",
    "acos",
    "atan",
    "sqrt",
    "exp",
    "log",
    "log2",
    "log10",
    "floor",
    "ceil",
    "round",
    "sign",
    "isnan",
    "isinf",
    "isfinite",
];
const ARRAY_BUILTINS: &[&str] = &[
    "fprintf",
    "sprintf",
    "error",
    "assert",
    "numel",
    "length",
    "isempty",
    "size",
    "zeros",
    "ones",
    "eye",
    "reshape",
    "transpose",
    "num2str",
    "strcmp",
    "linspace",
    "diag",
    "sum",
    "prod",
    "all",
    "any",
    "norm",
    "dot",
    "sort",
    "find",
    "isnan",
    "isinf",
    "isfinite",
    "log2",
];
/// Select the array frontend only for explicit array/character/builtin syntax.
/// Scalar-only sources keep the original scalar API and diagnostics.
pub(crate) fn selects_array_frontend(source: &str) -> bool {
    let Ok(tokens) = lex(source) else {
        return false;
    };
    if tokens.iter().enumerate().any(|(i,s)| {
        matches!(&s.token,Token::Text(_))
        || matches!(&s.token,Token::Op(op) if ["[",".*","./",".\\",".^","\\","'",".'"].contains(&op.as_str()))
        || matches!(&s.token,Token::Name(n) if ["while","break","continue","return"].contains(&n.as_str()))
        || matches!(&s.token,Token::Name(n) if ARRAY_BUILTINS.contains(&n.as_str()) && tokens.get(i+1).is_some_and(|s|s.token==Token::Op("(".into())))
    }) { return true; }
    // Indexing parameters need not contain a matrix literal (e.g. y=x(1)).
    // Inspect scope names without changing the legacy scalar function ABI.
    let mut parser = Parser {
        tokens,
        pos: 0,
        depth: 0,
    };
    let Ok(script) = parser.body() else {
        return false;
    };
    let mut vars = BTreeSet::new();
    assigned(&script, &mut vars);
    if body_needs_arrays(&script, &vars) {
        return true;
    }
    while parser.named("function") {
        let Ok(function) = parser.function() else {
            return false;
        };
        let mut vars = function.args.into_iter().collect();
        assigned(&function.body, &mut vars);
        if body_needs_arrays(&function.body, &vars) {
            return true;
        }
    }
    false
}
fn expr_needs_arrays(expr: &Expr, vars: &BTreeSet<String>) -> bool {
    match expr {
        Expr::Apply(name, args) => {
            vars.contains(name) || args.iter().any(|e| expr_needs_arrays(e, vars))
        }
        Expr::Range(..) | Expr::Array(..) | Expr::All | Expr::End | Expr::Text(_) => true,
        Expr::Unary(_, e) => expr_needs_arrays(e, vars),
        Expr::Binary(_, a, b) => expr_needs_arrays(a, vars) || expr_needs_arrays(b, vars),
        _ => false,
    }
}
fn body_needs_arrays(body: &[Stmt], vars: &BTreeSet<String>) -> bool {
    body.iter().any(|s| match s {
        Stmt::Assign(Target::Index(..) | Target::Many(..), _) => true,
        Stmt::Assign(_, e) | Stmt::Call(e) => expr_needs_arrays(e, vars),
        Stmt::If(branches, other) => {
            branches
                .iter()
                .any(|(c, b)| expr_needs_arrays(c, vars) || body_needs_arrays(b, vars))
                || body_needs_arrays(other, vars)
        }
        Stmt::For(_, Expr::Range(a, b, c), body) => {
            expr_needs_arrays(a, vars)
                || expr_needs_arrays(b, vars)
                || expr_needs_arrays(c, vars)
                || body_needs_arrays(body, vars)
        }
        Stmt::For(..) | Stmt::While(..) | Stmt::Break | Stmt::Continue | Stmt::Return => true,
    })
}

fn assigned(body: &[Stmt], vars: &mut BTreeSet<String>) {
    for statement in body {
        match statement {
            Stmt::Assign(target, _) => match target {
                Target::Name(n) | Target::Index(n, _) => {
                    vars.insert(n.clone());
                }
                Target::Many(names) => vars.extend(names.iter().cloned()),
            },
            Stmt::For(n, _, body) => {
                vars.insert(n.clone());
                assigned(body, vars);
            }
            Stmt::While(_, body) => assigned(body, vars),
            Stmt::If(branches, other) => {
                for (_, body) in branches {
                    assigned(body, vars);
                }
                assigned(other, vars);
            }
            _ => {}
        }
    }
}
struct Generator<'a> {
    vars: BTreeSet<String>,
    functions: &'a BTreeMap<String, (usize, usize)>,
    outputs: &'a [String],
    serial: usize,
    in_function: bool,
}
impl Generator<'_> {
    fn expr(&mut self, expr: &Expr, end: Option<&str>) -> Result<String, Error> {
        Ok(match expr {
            Expr::Number(n) => format!("Value::scalar({n:?}_f64)"),
            Expr::Text(s) => format!("Value::string({s:?})?"),
            Expr::Var(n) => {
                if self.vars.contains(n) {
                    format!("get(&env,{n:?})?")
                } else {
                    match n.as_str() {
                        "pi" => "Value::scalar(std::f64::consts::PI)".into(),
                        "Inf" | "inf" => "Value::scalar(f64::INFINITY)".into(),
                        "NaN" | "nan" => "Value::scalar(f64::NAN)".into(),
                        "true" => "Value::logical(true)".into(),
                        "false" => "Value::logical(false)".into(),
                        _ => return Err(error(1, format!("undefined variable '{n}'"))),
                    }
                }
            }
            Expr::End => end
                .ok_or_else(|| error(1, "'end' is only supported inside array indices"))?
                .into(),
            Expr::All => return Err(error(1, "bare ':' is only supported as an array index")),
            Expr::Unary(op, a) => format!("unary({op:?},&({}))?", self.expr(a, end)?),
            Expr::Binary(op, a, b) if op == "&&" || op == "||" => format!(
                "Value::logical(({}).scalar_truth()? {op} ({}).scalar_truth()?)",
                self.expr(a, end)?,
                self.expr(b, end)?
            ),
            Expr::Binary(op, a, b) => format!(
                "binary({op:?},&({}),&({}))?",
                self.expr(a, end)?,
                self.expr(b, end)?
            ),
            Expr::Range(a, b, c) => format!(
                "range(&({}),&({}),&({}))?",
                self.expr(a, end)?,
                self.expr(b, end)?,
                self.expr(c, end)?
            ),
            Expr::Array(rows) => {
                let rows = rows
                    .iter()
                    .map(|row| {
                        let values = row
                            .iter()
                            .map(|v| self.expr(v, end))
                            .collect::<Result<Vec<_>, _>>()?;
                        Ok(format!("vec![{}]", values.join(",")))
                    })
                    .collect::<Result<Vec<_>, Error>>()?;
                format!("concatenate(vec![{}])?", rows.join(","))
            }
            Expr::Apply(name, args) if self.vars.contains(name) => {
                let index = self.indices(args, "indexed")?;
                format!("{{let indexed=get(&env,{name:?})?; indexed.index(&[{index}])?}}")
            }
            Expr::Apply(name, args) => {
                let call = self.call(name, args, 1, end)?;
                format!(
                    "{{let result={call}; result.into_iter().next().ok_or_else(||\"function has no output\".to_string())?}}"
                )
            }
        })
    }
    fn indices(&mut self, args: &[Expr], base: &str) -> Result<String, Error> {
        if args.is_empty() || args.len() > 2 {
            return Err(error(1, "one or two array indices required"));
        }
        args.iter()
            .enumerate()
            .map(|(i, arg)| {
                if matches!(arg, Expr::All) {
                    Ok("Index::All".into())
                } else {
                    Ok(format!(
                        "Index::Values({})",
                        self.expr(arg, Some(&format!("{base}.end_value({i},{})", args.len())))?
                    ))
                }
            })
            .collect::<Result<Vec<_>, _>>()
            .map(|v| v.join(","))
    }
    fn call(
        &mut self,
        name: &str,
        args: &[Expr],
        outputs: usize,
        end: Option<&str>,
    ) -> Result<String, Error> {
        if self.vars.contains(name) {
            return Err(error(
                1,
                "multiple outputs cannot be taken from array indexing",
            ));
        }
        let values = args
            .iter()
            .map(|a| self.expr(a, end))
            .collect::<Result<Vec<_>, _>>()?
            .join(",");
        if let Some((inputs, available)) = self.functions.get(name) {
            if *inputs != args.len() {
                return Err(error(1, format!("{name} expects {inputs} arguments")));
            }
            if outputs > *available {
                return Err(error(
                    1,
                    format!("{name} supplies only {available} outputs"),
                ));
            }
            Ok(format!("{{let args=vec![{values}];call_{name}(rt,args)?}}"))
        } else if BUILTINS.contains(&name) {
            Ok(format!("builtin({name:?},vec![{values}],{outputs})?"))
        } else {
            Err(error(1, format!("unsupported function '{name}'")))
        }
    }
    fn returns(&self) -> String {
        format!(
            "Ok(vec![{}])",
            self.outputs
                .iter()
                .map(|n| format!("get(&env,{n:?})?"))
                .collect::<Vec<_>>()
                .join(",")
        )
    }
    fn body(&mut self, body: &[Stmt], loop_depth: usize) -> Result<String, Error> {
        let mut out = String::new();
        for statement in body {
            out.push_str("rt.tick()?;\n");
            match statement {
                Stmt::Assign(Target::Name(name), expr) => {
                    let value = self.expr(expr, None)?;
                    out.push_str(&format!(
                        "{{let value={value};env.insert({name:?}.into(),value);}}\n"
                    ));
                }
                Stmt::Assign(Target::Index(name, args), expr) => {
                    let indices = self.indices(args, "indexed")?;
                    let value = self.expr(expr, None)?;
                    out.push_str(&format!("{{let value={value};let mut indexed=env.get({name:?}).cloned().unwrap_or_else(Value::empty);let indices=vec![{indices}];indexed.assign(&indices,&value)?;env.insert({name:?}.into(),indexed);}}\n"));
                }
                Stmt::Assign(Target::Many(names), expr) => {
                    let Expr::Apply(name, args) = expr else {
                        return Err(error(1, "multiple assignment requires a function call"));
                    };
                    let call = self.call(name, args, names.len(), None)?;
                    out.push_str(&format!("{{let mut values=({call}).into_iter();"));
                    for name in names {
                        out.push_str(&format!("env.insert({name:?}.into(),values.next().ok_or_else(||\"missing function output\".to_string())?);"));
                    }
                    out.push_str("}\n");
                }
                Stmt::Call(Expr::Apply(name, args)) => {
                    let call = self.call(name, args, 0, None)?;
                    out.push_str(&format!("let _={call};\n"));
                }
                Stmt::Call(_) => unreachable!(),
                Stmt::If(branches, other) => {
                    for (i, (condition, body)) in branches.iter().enumerate() {
                        let condition = self.expr(condition, None)?;
                        out.push_str(&format!(
                            "{}if ({condition}).truth()? {{\n{}}}",
                            if i == 0 { "" } else { "else " },
                            self.body(body, loop_depth)?
                        ));
                    }
                    out.push_str(&format!("else{{\n{}}}\n", self.body(other, loop_depth)?));
                }
                Stmt::For(name, expr, body) => {
                    self.serial += 1;
                    let id = self.serial;
                    let value = self.expr(expr, None)?;
                    let body = self.body(body, loop_depth + 1)?;
                    out.push_str(&format!("{{let values_{id}={value}; if values_{id}.cols==0||values_{id}.rows==0 {{env.insert({name:?}.into(),values_{id}.clone());}} else {{for column in values_{id}.columns() {{rt.tick()?;env.insert({name:?}.into(),column);{body}}}}}}}\n"));
                }
                Stmt::While(expr, body) => {
                    let condition = self.expr(expr, None)?;
                    let body = self.body(body, loop_depth + 1)?;
                    out.push_str(&format!(
                        "while ({condition}).truth()? {{rt.tick()?;{body}}}\n"
                    ));
                }
                Stmt::Break | Stmt::Continue => {
                    if loop_depth == 0 {
                        return Err(error(1, "break/continue outside a loop"));
                    }
                    out.push_str(if matches!(statement, Stmt::Break) {
                        "break;\n"
                    } else {
                        "continue;\n"
                    });
                }
                Stmt::Return => {
                    if !self.in_function {
                        return Err(error(1, "return outside a function"));
                    }
                    out.push_str(&format!("return {};\n", self.returns()));
                }
            }
        }
        Ok(out)
    }
}
/// Generate native Rust for real numeric/logical matrices and ASCII character arrays.
/// The generated code contains the dependency-free runtime and executes no external code.
pub fn transpile_arrays(source: &str, library: bool) -> Result<String, Error> {
    let mut parser = Parser {
        tokens: lex(source)?,
        pos: 0,
        depth: 0,
    };
    let script = parser.body()?;
    let mut functions = Vec::new();
    while parser.named("function") {
        functions.push(parser.function()?);
    }
    if !matches!(parser.token(), Token::Eof) {
        return Err(parser.fail("unexpected token after script/function definitions"));
    }
    if library && (!script.is_empty() || functions.is_empty()) {
        return Err(error(
            1,
            "library mode requires functions and no script statements",
        ));
    }
    let mut signatures = BTreeMap::new();
    for function in &functions {
        if BUILTINS.contains(&function.name.as_str()) {
            return Err(error(1, "built-in function shadowing is unsupported"));
        }
        if signatures
            .insert(
                function.name.clone(),
                (function.args.len(), function.outputs.len()),
            )
            .is_some()
        {
            return Err(error(1, "duplicate function definition"));
        }
    }
    let mut out = String::from(
        "// Generated by unlinked-matlab array frontend. Real 2D bounded subset.\n#![allow(dead_code,unused_variables,unused_mut,unused_imports,non_snake_case,unreachable_code)]\nmod unlinked_array_runtime {\n",
    );
    out.push_str(include_str!("array_runtime.rs"));
    out.push_str("\n}\npub use unlinked_array_runtime::{Value,ValueKind,ArrayResult};\nuse unlinked_array_runtime::*;\n");
    for function in &functions {
        let mut vars = function.args.iter().cloned().collect();
        assigned(&function.body, &mut vars);
        for output in &function.outputs {
            if !vars.contains(output) {
                return Err(error(
                    1,
                    format!("function output '{output}' is never assigned"),
                ));
            }
        }
        let mut generator = Generator {
            vars,
            functions: &signatures,
            outputs: &function.outputs,
            serial: 0,
            in_function: true,
        };
        let body = generator.body(&function.body, 0)?;
        let returns = generator.returns();
        let name = &function.name;
        let arity = function.args.len();
        out.push_str(&format!("pub fn f_{name}(args:Vec<Value>)->ArrayResult<Vec<Value>> {{call_{name}(&mut Runtime::default(),args)}}\nfn call_{name}(rt:&mut Runtime,args:Vec<Value>)->ArrayResult<Vec<Value>> {{rt.enter()?;let result=(||->ArrayResult<Vec<Value>>{{if args.len()!={arity} {{return Err(\"function argument count mismatch\".into());}}for arg in &args{{arg.validate()?;}}let mut env=Environment::new();let mut args=args.into_iter();\n"));
        for arg in &function.args {
            out.push_str(&format!(
                "env.insert({arg:?}.into(),args.next().unwrap());\n"
            ));
        }
        out.push_str(&format!("{body}{returns}\n}})();rt.leave();result}}\n"));
    }
    if !library {
        let mut vars = BTreeSet::new();
        assigned(&script, &mut vars);
        let mut generator = Generator {
            vars,
            functions: &signatures,
            outputs: &[],
            serial: 0,
            in_function: false,
        };
        let body = generator.body(&script, 0)?;
        out.push_str(&format!("pub fn run_script()->ArrayResult<Environment>{{let mut runtime=Runtime::default();let rt=&mut runtime;let mut env=Environment::new();{body}Ok(env)}}\nfn main(){{if let Err(error)=run_script(){{eprintln!(\"MATLAB runtime error: {{error}}\");std::process::exit(1);}}}}\n"));
    }
    Ok(out)
}

/// Evaluate a pure scalar/array parameter expression against explicit values.
/// File/process access, printing, script execution and user functions are unavailable.
pub fn eval_array_expr(
    source: &str,
    workspace: &BTreeMap<String, crate::array_runtime::Value>,
) -> Result<crate::array_runtime::Value, Error> {
    eval_array_expr_with_budget(source, workspace, &mut ArrayBudget::default())
}

/// Evaluate against a shared budget to bound a complete model's parameter work.
pub fn eval_array_expr_with_budget(
    source: &str,
    workspace: &BTreeMap<String, crate::array_runtime::Value>,
    budget: &mut ArrayBudget,
) -> Result<crate::array_runtime::Value, Error> {
    for value in workspace.values() {
        value.validate().map_err(|e| error(1, e))?;
    }
    let mut parser = Parser {
        tokens: lex(source)?,
        pos: 0,
        depth: 0,
    };
    budget
        .operations(parser.tokens.len())
        .map_err(|e| error(1, e))?;
    let expr = parser.expr(0, false)?;
    parser.separators();
    if !matches!(parser.token(), Token::Eof) {
        return Err(parser.fail("expected end of array expression"));
    }
    eval(&expr, workspace, None, budget).map_err(|e| error(1, e))
}

/// Aggregate intermediate-element and estimated-operation budget. Reuse across
/// calls when evaluating all workspace and block parameters for one model.
pub struct ArrayBudget {
    values: usize,
    operations: usize,
}
impl Default for ArrayBudget {
    fn default() -> Self {
        Self {
            values: 8_000_000,
            operations: 20_000_000,
        }
    }
}
impl ArrayBudget {
    pub fn with_limits(intermediate_elements: usize, operations: usize) -> Self {
        Self {
            values: intermediate_elements,
            operations,
        }
    }
    pub fn remaining_elements(&self) -> usize {
        self.values
    }
    pub fn remaining_operations(&self) -> usize {
        self.operations
    }
    fn operations(&mut self, count: usize) -> Result<(), String> {
        self.operations = self
            .operations
            .checked_sub(count)
            .ok_or("array expression exceeds aggregate operation budget")?;
        Ok(())
    }
    fn value(&mut self, count: usize) -> Result<(), String> {
        self.values = self
            .values
            .checked_sub(count)
            .ok_or("array expression exceeds aggregate value budget")?;
        self.operations(count.max(1))
    }
}

fn eval(
    expr: &Expr,
    workspace: &BTreeMap<String, crate::array_runtime::Value>,
    end: Option<f64>,
    budget: &mut ArrayBudget,
) -> crate::array_runtime::ArrayResult<crate::array_runtime::Value> {
    use crate::array_runtime::{self as rt, Index, Value};
    let value = match expr {
        Expr::Number(n) => Value::scalar(*n),
        Expr::Text(s) => Value::string(s)?,
        Expr::Var(name) => {
            if let Some(value) = workspace.get(name) {
                value.clone()
            } else {
                match name.as_str() {
                    "pi" => Value::scalar(std::f64::consts::PI),
                    "Inf" | "inf" => Value::scalar(f64::INFINITY),
                    "NaN" | "nan" => Value::scalar(f64::NAN),
                    "true" => Value::logical(true),
                    "false" => Value::logical(false),
                    _ => return Err(format!("undefined variable '{name}'")),
                }
            }
        }
        Expr::End => Value::scalar(end.ok_or("'end' is only supported inside array indices")?),
        Expr::All => return Err("bare ':' requires an array index".into()),
        Expr::Unary(op, x) => rt::unary(op, &eval(x, workspace, end, budget)?)?,
        Expr::Binary(op, a, b) if op == "&&" => Value::logical(
            eval(a, workspace, end, budget)?.scalar_truth()?
                && eval(b, workspace, end, budget)?.scalar_truth()?,
        ),
        Expr::Binary(op, a, b) if op == "||" => Value::logical(
            eval(a, workspace, end, budget)?.scalar_truth()?
                || eval(b, workspace, end, budget)?.scalar_truth()?,
        ),
        Expr::Binary(op, a, b) => {
            let a = eval(a, workspace, end, budget)?;
            let b = eval(b, workspace, end, budget)?;
            let cost = match op.as_str() {
                "*" if a.data.len() != 1 && b.data.len() != 1 => {
                    a.rows.saturating_mul(a.cols).saturating_mul(b.cols)
                }
                "\\" if a.data.len() != 1 => a
                    .rows
                    .saturating_mul(a.rows)
                    .saturating_mul(a.rows.saturating_add(b.cols)),
                "/" if b.data.len() != 1 => b
                    .rows
                    .saturating_mul(b.rows)
                    .saturating_mul(b.rows.saturating_add(a.rows)),
                "^" if a.data.len() != 1 => 32usize
                    .saturating_mul(a.rows)
                    .saturating_mul(a.rows)
                    .saturating_mul(a.rows),
                _ => a.rows.max(b.rows).saturating_mul(a.cols.max(b.cols)),
            };
            budget.operations(cost)?;
            rt::binary(op, &a, &b)?
        }
        Expr::Range(a, b, c) => rt::range(
            &eval(a, workspace, end, budget)?,
            &eval(b, workspace, end, budget)?,
            &eval(c, workspace, end, budget)?,
        )?,
        Expr::Array(rows) => rt::concatenate(
            rows.iter()
                .map(|row| {
                    row.iter()
                        .map(|v| eval(v, workspace, end, budget))
                        .collect()
                })
                .collect::<Result<Vec<_>, _>>()?,
        )?,
        Expr::Apply(name, args) if workspace.contains_key(name) => {
            let value = &workspace[name];
            if args.is_empty() || args.len() > 2 {
                return Err("one or two array indices required".into());
            }
            let indices = args
                .iter()
                .enumerate()
                .map(|(i, arg)| {
                    if matches!(arg, Expr::All) {
                        Ok(Index::All)
                    } else {
                        Ok(Index::Values(eval(
                            arg,
                            workspace,
                            Some(value.end_value(i, args.len()).number()?),
                            budget,
                        )?))
                    }
                })
                .collect::<Result<Vec<_>, String>>()?;
            value.index(&indices)?
        }
        Expr::Apply(name, args) => {
            if ["disp", "fprintf", "sprintf", "error", "assert"].contains(&name.as_str()) {
                return Err(format!(
                    "'{name}' is unavailable in pure parameter expressions"
                ));
            }
            let args = args
                .iter()
                .map(|a| eval(a, workspace, end, budget))
                .collect::<Result<Vec<_>, _>>()?;
            rt::builtin(name, args, 1)?
                .into_iter()
                .next()
                .ok_or("builtin returned no value")?
        }
    };
    budget.value(value.data.len())?;
    Ok(value)
}
