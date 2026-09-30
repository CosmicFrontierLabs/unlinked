//! MATLAB/Octave real scalar, matrix and character-array frontends targeting Rust.
//! Parsing and transpilation do not execute source code or launch a compiler.
pub mod array_runtime;
mod arrays;
mod comments;
pub mod project;
pub use arrays::{
    ArrayBudget, FunctionProgram, FunctionSignature, eval_array_expr, eval_array_expr_with_budget,
    eval_function, eval_script, eval_script_with_budget, transpile_typed,
};
pub use project::{GeneratedProject, generate_project};
use std::collections::BTreeMap;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub line: usize,
    pub message: String,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}
impl std::error::Error for Error {}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Number(f64),
    Name(String),
    Op(String),
    Newline,
    Eof,
}
#[derive(Debug, Clone)]
struct Spanned {
    token: Token,
    line: usize,
}
fn lex(source: &str) -> Result<Vec<Spanned>, Error> {
    if source.len() > 65_536 {
        return Err(Error {
            line: 1,
            message: "source exceeds 65536-byte subset limit".into(),
        });
    }
    let chars: Vec<char> = source.chars().collect();
    let (mut i, mut line, mut out) = (0, 1, Vec::new());
    while i < chars.len() {
        if out.len() >= 1024 {
            return Err(Error {
                line,
                message: "source exceeds 1024-token subset limit".into(),
            });
        }
        let c = chars[i];
        let token = if c == '\n' || c == ';' {
            i += 1;
            let old = line;
            if c == '\n' {
                line += 1;
            }
            out.push(Spanned {
                token: Token::Newline,
                line: old,
            });
            continue;
        } else if c.is_whitespace() {
            i += 1;
            continue;
        } else if c == '%' {
            comments::skip(&chars, &mut i, &mut line)?;
            continue;
        } else if c.is_ascii_alphabetic() || c == '_' {
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
            if chars.get(i) == Some(&'.') {
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
            let text: String = chars[start..i].iter().collect();
            let value = text.parse::<f64>().map_err(|_| Error {
                line,
                message: format!("invalid numeric literal {text}"),
            })?;
            if !value.is_finite() {
                return Err(Error {
                    line,
                    message: "numeric literal must be finite".into(),
                });
            }
            Token::Number(value)
        } else if "+-*/^=<>~&|():,".contains(c) {
            i += 1;
            let mut op = c.to_string();
            if let Some(&next) = chars.get(i)
                && matches!(
                    (c, next),
                    ('=', '=') | ('~', '=') | ('<', '=') | ('>', '=') | ('&', '&') | ('|', '|')
                )
            {
                op.push(next);
                i += 1;
            }
            Token::Op(op)
        } else {
            return Err(Error {
                line,
                message: format!(
                    "unsupported character {c:?}; matrices, strings and indexing are not supported"
                ),
            });
        };
        out.push(Spanned { token, line });
        if out.len() > 1024 {
            return Err(Error {
                line,
                message: "source exceeds 1024-token subset limit".into(),
            });
        }
    }
    out.push(Spanned {
        token: Token::Eof,
        line,
    });
    Ok(out)
}

#[derive(Debug)]
enum Expr {
    Number(f64),
    Var(String),
    Unary(String, Box<Expr>),
    Binary(String, Box<Expr>, Box<Expr>),
    Call(String, Vec<Expr>),
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
    fn error(&self, message: impl Into<String>) -> Error {
        Error {
            line: self.tokens[self.pos].line,
            message: message.into(),
        }
    }
    fn op(&mut self, op: &str) -> bool {
        if self.token() == &Token::Op(op.into()) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn expect_op(&mut self, op: &str) -> Result<(), Error> {
        if self.op(op) {
            Ok(())
        } else {
            Err(self.error(format!("expected '{op}'")))
        }
    }
    fn separators(&mut self) {
        while self.token() == &Token::Newline {
            self.pos += 1;
        }
    }
    fn expr(&mut self, min: u8) -> Result<Expr, Error> {
        if self.depth >= 64 {
            return Err(self.error("expression nesting exceeds subset limit of 64"));
        }
        self.depth += 1;
        let result = self.expr_inner(min);
        self.depth -= 1;
        result
    }
    fn expr_inner(&mut self, min: u8) -> Result<Expr, Error> {
        let mut lhs = match self.token().clone() {
            Token::Number(n) => {
                self.pos += 1;
                Expr::Number(n)
            }
            Token::Name(name) => {
                self.pos += 1;
                if self.op("(") {
                    let mut args = Vec::new();
                    if !self.op(")") {
                        loop {
                            args.push(self.expr(0)?);
                            if self.op(")") {
                                break;
                            }
                            self.expect_op(",")?;
                        }
                    }
                    Expr::Call(name, args)
                } else {
                    Expr::Var(name)
                }
            }
            Token::Op(op) if matches!(op.as_str(), "+" | "-" | "~") => {
                self.pos += 1;
                Expr::Unary(op, Box::new(self.expr(11)?))
            }
            Token::Op(op) if op == "(" => {
                self.pos += 1;
                let expr = self.expr(0)?;
                self.expect_op(")")?;
                expr
            }
            _ => return Err(self.error("expected scalar expression")),
        };
        while let Token::Op(op) = self.token() {
            let (left, right) = match op.as_str() {
                "||" => (1, 2),
                "&&" => (3, 4),
                "==" | "~=" | "<" | ">" | "<=" | ">=" => (5, 6),
                "+" | "-" => (7, 8),
                "*" | "/" => (9, 10),
                "^" => (12, 13),
                _ => break,
            };
            if left < min {
                break;
            }
            let op = op.clone();
            self.pos += 1;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(self.expr(right)?));
        }
        Ok(lhs)
    }
}

fn semantic(message: impl Into<String>) -> Error {
    Error {
        line: 1,
        message: message.into(),
    }
}
fn builtin(name: &str) -> Option<usize> {
    match name {
        "sin" | "cos" | "tan" | "asin" | "acos" | "atan" | "sqrt" | "abs" | "exp" | "log"
        | "log10" | "floor" | "ceil" | "round" | "sign" => Some(1),
        "min" | "max" | "atan2" | "mod" => Some(2),
        _ => None,
    }
}
/// Evaluate one scalar parameter expression using an explicit variable workspace.
/// No assignments, user-defined functions, file access or process execution occur.
pub fn eval_expr(source: &str, workspace: &BTreeMap<String, f64>) -> Result<f64, Error> {
    let mut parser = Parser {
        tokens: lex(source)?,
        pos: 0,
        depth: 0,
    };
    let expr = parser.expr(0)?;
    parser.separators();
    if !matches!(parser.token(), Token::Eof) {
        return Err(parser.error("expected end of scalar expression"));
    }
    evaluate(&expr, workspace)
}
fn evaluate(expr: &Expr, vars: &BTreeMap<String, f64>) -> Result<f64, Error> {
    Ok(match expr {
        Expr::Number(n) => *n,
        Expr::Var(name) => match vars.get(name) {
            Some(n) => *n,
            None => match name.as_str() {
                "pi" => std::f64::consts::PI,
                "Inf" | "inf" => f64::INFINITY,
                "NaN" | "nan" => f64::NAN,
                "true" => 1.0,
                "false" => 0.0,
                _ => return Err(semantic(format!("undefined variable '{name}'"))),
            },
        },
        Expr::Unary(op, value) => {
            let x = evaluate(value, vars)?;
            match op.as_str() {
                "+" => x,
                "-" => -x,
                _ => (!scalar_bool(x)?) as u8 as f64,
            }
        }
        Expr::Binary(op, left, right) => {
            let a = evaluate(left, vars)?;
            if op == "&&" && !scalar_bool(a)? {
                return Ok(0.0);
            }
            if op == "||" && scalar_bool(a)? {
                return Ok(1.0);
            }
            let b = evaluate(right, vars)?;
            match op.as_str() {
                "+" => a + b,
                "-" => a - b,
                "*" => a * b,
                "/" => a / b,
                "^" => a.powf(b),
                "==" => (a == b) as u8 as f64,
                "~=" => (a != b) as u8 as f64,
                "<" => (a < b) as u8 as f64,
                ">" => (a > b) as u8 as f64,
                "<=" => (a <= b) as u8 as f64,
                ">=" => (a >= b) as u8 as f64,
                "&&" | "||" => scalar_bool(b)? as u8 as f64,
                _ => unreachable!(),
            }
        }
        Expr::Call(name, args) => {
            if vars.contains_key(name) {
                return Err(semantic(format!(
                    "indexing or calling variable '{name}' is unsupported"
                )));
            }
            let arity =
                builtin(name).ok_or_else(|| semantic(format!("unsupported function '{name}'")))?;
            if args.len() != arity {
                return Err(semantic(format!("'{name}' expects {arity} arguments")));
            }
            let a = evaluate(&args[0], vars)?;
            let b = if arity == 2 {
                evaluate(&args[1], vars)?
            } else {
                0.0
            };
            match name.as_str() {
                "sin" => a.sin(),
                "cos" => a.cos(),
                "tan" => a.tan(),
                "asin" => a.asin(),
                "acos" => a.acos(),
                "atan" => a.atan(),
                "sqrt" => a.sqrt(),
                "abs" => a.abs(),
                "exp" => a.exp(),
                "log" => a.ln(),
                "log10" => a.log10(),
                "floor" => a.floor(),
                "ceil" => a.ceil(),
                "round" => a.round(),
                "sign" => {
                    if a == 0.0 {
                        0.0
                    } else {
                        a.signum()
                    }
                }
                "min" => a.min(b),
                "max" => a.max(b),
                "atan2" => a.atan2(b),
                "mod" => scalar_mod(a, b),
                _ => unreachable!(),
            }
        }
    })
}

fn scalar_bool(x: f64) -> Result<bool, Error> {
    if x.is_nan() {
        Err(semantic("NaN cannot be converted to logical"))
    } else {
        Ok(x != 0.0)
    }
}
fn scalar_mod(x: f64, y: f64) -> f64 {
    if y == 0.0 {
        return x;
    }
    let q = x / y;
    if !q.is_finite() || !y.is_finite() {
        return f64::NAN;
    }
    if (q - q.round()).abs() <= 2.0 * f64::EPSILON * q.abs() {
        return 0.0_f64.copysign(y);
    }
    x - q.floor() * y
}

/// Format generated source without requiring rustfmt or launching a process.
fn format_generated(source: &str) -> Result<String, Error> {
    let mut syntax = syn::parse_file(source).map_err(|e| Error {
        line: 1,
        message: format!("generated Rust syntax error: {e}"),
    })?;
    struct CleanAtoms;
    impl syn::visit_mut::VisitMut for CleanAtoms {
        fn visit_expr_mut(&mut self, expression: &mut syn::Expr) {
            syn::visit_mut::visit_expr_mut(self, expression);
            let condition = match expression {
                syn::Expr::If(value) => Some(&mut value.cond),
                syn::Expr::While(value) => Some(&mut value.cond),
                _ => None,
            };
            if let Some(condition) = condition {
                while let syn::Expr::Paren(parenthesized) = condition.as_ref() {
                    *condition = parenthesized.expr.clone();
                }
            }
            if let syn::Expr::Paren(parenthesized) = expression
                && matches!(
                    *parenthesized.expr,
                    syn::Expr::Lit(_)
                        | syn::Expr::Path(_)
                        | syn::Expr::Call(_)
                        | syn::Expr::MethodCall(_)
                        | syn::Expr::Index(_)
                        | syn::Expr::Field(_)
                )
            {
                *expression = (*parenthesized.expr).clone();
            }
        }
    }
    syn::visit_mut::VisitMut::visit_file_mut(&mut CleanAtoms, &mut syntax);
    let formatted = prettyplease::unparse(&syntax);
    if formatted.len() > 4 * 1024 * 1024 {
        return Err(Error {
            line: 1,
            message: "generated typed Rust exceeds 4 MiB limit".into(),
        });
    }
    Ok(formatted)
}
