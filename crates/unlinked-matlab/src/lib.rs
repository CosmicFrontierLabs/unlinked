//! MATLAB/Octave real scalar, matrix and character-array frontends targeting Rust.
//! Parsing and transpilation do not execute source code or launch a compiler.
pub mod array_runtime;
mod arrays;
pub use arrays::{eval_array_expr, transpile_arrays};
use std::collections::{BTreeMap, BTreeSet};
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
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
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
#[derive(Debug)]
enum Stmt {
    Assign(String, Expr),
    Display(Expr),
    For(String, Expr, Expr, Expr, Vec<Stmt>),
    If(Vec<(Expr, Vec<Stmt>)>, Vec<Stmt>),
}
struct Function {
    name: String,
    args: Vec<String>,
    output: String,
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
    fn error(&self, message: impl Into<String>) -> Error {
        Error {
            line: self.tokens[self.pos].line,
            message: message.into(),
        }
    }
    fn is_name(&self, name: &str) -> bool {
        self.token() == &Token::Name(name.into())
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
    fn name(&mut self) -> Result<String, Error> {
        if let Token::Name(name) = self.token() {
            let name = name.clone();
            self.pos += 1;
            Ok(name)
        } else {
            Err(self.error("expected identifier"))
        }
    }
    fn separators(&mut self) {
        while self.token() == &Token::Newline {
            self.pos += 1;
        }
    }
    fn end_statement(&mut self) -> Result<(), Error> {
        if matches!(self.token(), Token::Newline | Token::Eof) {
            self.separators();
            Ok(())
        } else {
            Err(self.error("expected newline or semicolon"))
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
    fn body(&mut self) -> Result<Vec<Stmt>, Error> {
        if self.depth >= 64 {
            return Err(self.error("statement nesting exceeds subset limit of 64"));
        }
        self.depth += 1;
        let result = self.body_inner();
        self.depth -= 1;
        result
    }
    fn body_inner(&mut self) -> Result<Vec<Stmt>, Error> {
        let mut body = Vec::new();
        self.separators();
        while !matches!(self.token(), Token::Eof)
            && !["end", "else", "elseif", "function"]
                .iter()
                .any(|n| self.is_name(n))
        {
            body.push(self.statement()?);
        }
        Ok(body)
    }
    fn end(&mut self) -> Result<(), Error> {
        if !self.is_name("end") {
            return Err(self.error("expected 'end'"));
        }
        self.pos += 1;
        self.end_statement()
    }
    fn statement(&mut self) -> Result<Stmt, Error> {
        let name = self.name()?;
        let stmt = match name.as_str() {
            "for" => {
                let var = self.name()?;
                self.expect_op("=")?;
                let start = self.expr(0)?;
                self.expect_op(":")?;
                let second = self.expr(0)?;
                let (step, stop) = if self.op(":") {
                    (second, self.expr(0)?)
                } else {
                    (Expr::Number(1.0), second)
                };
                self.end_statement()?;
                let body = self.body()?;
                self.end()?;
                return Ok(Stmt::For(var, start, step, stop, body));
            }
            "if" => {
                let condition = self.expr(0)?;
                self.end_statement()?;
                let mut branches = vec![(condition, self.body()?)];
                while self.is_name("elseif") {
                    self.pos += 1;
                    let condition = self.expr(0)?;
                    self.end_statement()?;
                    branches.push((condition, self.body()?));
                }
                let otherwise = if self.is_name("else") {
                    self.pos += 1;
                    self.end_statement()?;
                    self.body()?
                } else {
                    Vec::new()
                };
                self.end()?;
                return Ok(Stmt::If(branches, otherwise));
            }
            "disp" => {
                self.expect_op("(")?;
                let expr = self.expr(0)?;
                self.expect_op(")")?;
                Stmt::Display(expr)
            }
            "while" | "switch" | "global" | "persistent" | "classdef" | "parfor" | "try"
            | "return" | "break" | "continue" => {
                return Err(self.error(format!("unsupported statement '{name}'")));
            }
            _ => {
                self.expect_op("=")?;
                Stmt::Assign(name, self.expr(0)?)
            }
        };
        self.end_statement()?;
        Ok(stmt)
    }
    fn function(&mut self) -> Result<Function, Error> {
        self.pos += 1;
        let output = self.name()?;
        self.expect_op("=")?;
        let name = self.name()?;
        self.expect_op("(")?;
        let mut args = Vec::new();
        if !self.op(")") {
            loop {
                let arg = self.name()?;
                if args.contains(&arg) {
                    return Err(self.error("duplicate function parameter"));
                }
                args.push(arg);
                if self.op(")") {
                    break;
                }
                self.expect_op(",")?;
            }
        }
        self.end_statement()?;
        let body = self.body()?;
        self.end()?;
        Ok(Function {
            name,
            args,
            output,
            body,
        })
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
fn emit_expr(
    expr: &Expr,
    known: &BTreeSet<String>,
    functions: &BTreeMap<String, usize>,
) -> Result<String, Error> {
    Ok(match expr {
        Expr::Number(n) => format!("{n:?}_f64"),
        Expr::Var(name) => match name.as_str() {
            _ if known.contains(name) => format!("v_{name}"),
            "pi" => "std::f64::consts::PI".into(),
            "Inf" | "inf" => "f64::INFINITY".into(),
            "NaN" | "nan" => "f64::NAN".into(),
            "true" => "1.0_f64".into(),
            "false" => "0.0_f64".into(),
            _ => {
                return Err(semantic(format!(
                    "variable '{name}' is not definitely assigned"
                )));
            }
        },
        Expr::Unary(op, a) => {
            let a = emit_expr(a, known, functions)?;
            match op.as_str() {
                "+" => a,
                "-" => format!("(-({a}))"),
                _ => format!("((!matlab_bool({a})) as u8 as f64)"),
            }
        }
        Expr::Binary(op, a, b) => {
            let a = emit_expr(a, known, functions)?;
            let b = emit_expr(b, known, functions)?;
            match op.as_str() {
                "^" => format!("({a}).powf({b})"),
                "&&" | "||" => format!("((matlab_bool({a}) {op} matlab_bool({b})) as u8 as f64)"),
                "==" | "~=" | "<" | ">" | "<=" | ">=" => format!(
                    "((({a}) {} ({b})) as u8 as f64)",
                    if op == "~=" { "!=" } else { op }
                ),
                _ => format!("(({a}) {op} ({b}))"),
            }
        }
        Expr::Call(name, args) => {
            if known.contains(name) {
                return Err(semantic(format!(
                    "indexing or calling variable '{name}' is unsupported"
                )));
            }
            let arity = functions
                .get(name)
                .copied()
                .or_else(|| builtin(name))
                .ok_or_else(|| semantic(format!("unsupported function '{name}'")))?;
            if args.len() != arity {
                return Err(semantic(format!(
                    "'{name}' expects {arity} arguments, got {}",
                    args.len()
                )));
            }
            let args = args
                .iter()
                .map(|a| emit_expr(a, known, functions))
                .collect::<Result<Vec<_>, _>>()?;
            if functions.contains_key(name) {
                format!("f_{name}({})", args.join(", "))
            } else if name == "mod" {
                format!("matlab_mod({}, {})", args[0], args[1])
            } else if name == "sign" {
                format!("matlab_sign({})", args[0])
            } else {
                format!(
                    "({}).{}({})",
                    args[0],
                    if name == "log" { "ln" } else { name },
                    args[1..].join(", ")
                )
            }
        }
    })
}
fn assigned(body: &[Stmt], out: &mut BTreeSet<String>) {
    for stmt in body {
        match stmt {
            Stmt::Assign(name, _) => {
                out.insert(name.clone());
            }
            Stmt::For(name, _, _, _, body) => {
                out.insert(name.clone());
                assigned(body, out);
            }
            Stmt::If(branches, other) => {
                for (_, body) in branches {
                    assigned(body, out);
                }
                assigned(other, out);
            }
            _ => {}
        }
    }
}
fn emit_body(
    body: &[Stmt],
    known: &mut BTreeSet<String>,
    functions: &BTreeMap<String, usize>,
    serial: &mut usize,
) -> Result<String, Error> {
    let mut out = String::new();
    for stmt in body {
        match stmt {
            Stmt::Assign(name, expr) => {
                let expr = emit_expr(expr, known, functions)?;
                out.push_str(&format!("v_{name} = {expr};\n"));
                known.insert(name.clone());
            }
            Stmt::Display(expr) => out.push_str(&format!(
                "println!(\"{{}}\", {});\n",
                emit_expr(expr, known, functions)?
            )),
            Stmt::For(name, start, step, stop, body) => {
                let start = emit_expr(start, known, functions)?;
                let step = emit_expr(step, known, functions)?;
                let stop = emit_expr(stop, known, functions)?;
                *serial += 1;
                let id = *serial;
                let mut inner = known.clone();
                inner.insert(name.clone());
                let body = emit_body(body, &mut inner, functions, serial)?;
                out.push_str(&format!("for range_{id} in matlab_range({start}, {step}, {stop}) {{\nv_{name} = range_{id};\n{body}}}\n"));
                // A range may be empty: newly assigned variables cannot escape safely.
            }
            Stmt::If(branches, other) => {
                let mut paths = Vec::new();
                for (i, (condition, body)) in branches.iter().enumerate() {
                    let condition = emit_expr(condition, known, functions)?;
                    let mut inner = known.clone();
                    let body = emit_body(body, &mut inner, functions, serial)?;
                    paths.push(inner);
                    out.push_str(&format!(
                        "{}if matlab_bool({condition}) {{\n{body}}}",
                        if i == 0 { "" } else { " else " }
                    ));
                }
                let mut inner = known.clone();
                let body = emit_body(other, &mut inner, functions, serial)?;
                paths.push(inner);
                out.push_str(&format!(" else {{\n{body}}}\n"));
                if let Some(first) = paths.first() {
                    *known = first
                        .iter()
                        .filter(|name| paths.iter().all(|p| p.contains(*name)))
                        .cloned()
                        .collect();
                }
            }
        }
    }
    Ok(out)
}
fn declarations(body: &[Stmt], args: &[String]) -> String {
    let mut vars = BTreeSet::new();
    assigned(body, &mut vars);
    vars.into_iter()
        .filter(|n| !args.contains(n))
        .map(|n| format!("let mut v_{n}: f64;\n"))
        .collect()
}

/// Translate supported scripts and local functions into a standalone Rust program.
/// Selects the scalar or array frontend from the source features. Unsupported
/// syntax and statically unknown variables produce diagnostics.
/// This function does not execute code. Semantic diagnostics currently use line 1.
pub fn transpile(source: &str) -> Result<String, Error> {
    if arrays::selects_array_frontend(source) {
        arrays::transpile_arrays(source, false)
    } else {
        transpile_inner(source, false)
    }
}

/// Translate a function file into a Rust library or module; scripts are rejected.
/// Scalar-only functions export `f_name(f64, ...) -> f64`. Array-feature programs
/// export `f_name(Vec<Value>) -> ArrayResult<Vec<Value>>` for dynamic shapes and
/// multiple outputs. See [`transpile_arrays`] and [`array_runtime::Value`].
/// The output can be passed to `rustc --crate-type=lib --emit=llvm-ir,link`.
pub fn transpile_library(source: &str) -> Result<String, Error> {
    if arrays::selects_array_frontend(source) {
        arrays::transpile_arrays(source, true)
    } else {
        transpile_inner(source, true)
    }
}

fn transpile_inner(source: &str, library: bool) -> Result<String, Error> {
    let mut parser = Parser {
        tokens: lex(source)?,
        pos: 0,
        depth: 0,
    };
    let body = parser.body()?;
    let mut functions = Vec::new();
    while parser.is_name("function") {
        functions.push(parser.function()?);
    }
    if !matches!(parser.token(), Token::Eof) {
        return Err(parser.error("unexpected token after script; local functions must appear last"));
    }
    if library && (!body.is_empty() || functions.is_empty()) {
        return Err(semantic(
            "library mode requires one or more functions and no script statements",
        ));
    }
    let mut signatures = BTreeMap::new();
    for function in &functions {
        if builtin(&function.name).is_some() {
            return Err(semantic("shadowing built-in functions is unsupported"));
        }
        if signatures
            .insert(function.name.clone(), function.args.len())
            .is_some()
        {
            return Err(semantic("duplicate function declaration"));
        }
    }
    let mut out = String::from(RUNTIME);
    let mut serial = 0;
    for function in functions {
        let args = function
            .args
            .iter()
            .map(|a| format!("mut v_{a}: f64"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut known = function.args.iter().cloned().collect();
        let generated = emit_body(&function.body, &mut known, &signatures, &mut serial)?;
        if !known.contains(&function.output) {
            return Err(semantic(format!(
                "function '{}' output '{}' is not definitely assigned",
                function.name, function.output
            )));
        }
        out.push_str(&format!(
            "pub fn f_{}({args}) -> f64 {{\n{}{generated}v_{}\n}}\n",
            function.name,
            declarations(&function.body, &function.args),
            function.output
        ));
    }
    if !library {
        let generated = emit_body(&body, &mut BTreeSet::new(), &signatures, &mut serial)?;
        out.push_str(&format!(
            "fn main() {{\n{}{generated}}}\n",
            declarations(&body, &[])
        ));
    }
    Ok(out)
}
const RUNTIME: &str = r#"// Generated by unlinked-matlab. Scalar subset; see crate README.
#![allow(unused_mut, unused_variables, unused_assignments, dead_code, non_snake_case, unused_parens)]
fn matlab_sign(x: f64) -> f64 { if x == 0.0 { 0.0 } else { x.signum() } }
fn matlab_bool(x: f64) -> bool { assert!(!x.is_nan(), "NaN cannot be converted to logical"); x != 0.0 }
fn matlab_mod(x: f64, y: f64) -> f64 {
    if y == 0.0 { return x; }
    let q = x / y;
    if !q.is_finite() || !y.is_finite() { return f64::NAN; }
    if (q - q.round()).abs() <= 2.0 * f64::EPSILON * q.abs() { return 0.0_f64.copysign(y); }
    x - q.floor() * y
}
fn matlab_range(start: f64, step: f64, stop: f64) -> impl Iterator<Item = f64> {
    assert!(start.is_finite() && step.is_finite() && stop.is_finite(), "non-finite range");
    assert!(step != 0.0, "zero range step is unsupported");
    let intervals = (stop - start) / step;
    let count = if intervals < 0.0 { 0 } else {
        let tolerance = 4.0 * f64::EPSILON * intervals.abs().max(1.0);
        let count = (intervals + tolerance).floor() + 1.0;
        assert!(count <= 1_000_000.0, "range exceeds subset limit of 1000000 iterations");
        count as usize
    };
    assert!(count != 0, "empty for range requires an array-valued loop variable; unsupported scalar subset");
    (0..count).map(move |i| start + i as f64 * step)
}
"#;

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
