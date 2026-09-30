//! Native Rust code generation for the real matrix and character-array subset.
#[path = "functions.rs"]
mod functions;
#[path = "typed_codegen.rs"]
mod typed_codegen;
pub use typed_codegen::transpile_typed;
#[path = "script.rs"]
mod script;
use crate::Error;
pub use functions::{FunctionProgram, FunctionSignature, eval_function};
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
            crate::comments::skip(&chars, &mut i, &mut line)?;
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
struct Stmt {
    line: usize,
    kind: StmtKind,
}
#[derive(Debug)]
enum StmtKind {
    Assign(Target, Expr),
    Call(Expr),
    If(Vec<(Expr, Vec<Stmt>)>, Vec<Stmt>),
    For(String, Expr, Vec<Stmt>),
    While(Expr, Vec<Stmt>),
    Break,
    Continue,
    Return,
}
#[derive(Clone, Copy)]
enum ArgumentKind {
    Double,
    Logical,
    Character,
}
#[derive(Clone)]
struct ArgumentDeclaration {
    kind: ArgumentKind,
    dimensions: [Option<usize>; 2],
}
struct Function {
    name: String,
    args: Vec<String>,
    declarations: BTreeMap<String, ArgumentDeclaration>,
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
        let line = self.tokens[self.pos].line;
        let kind = self.statement_kind()?;
        Ok(Stmt { line, kind })
    }
    fn statement_kind(&mut self) -> Result<StmtKind, Error> {
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
            return Ok(StmtKind::If(branches, other));
        }
        if self.named("for") {
            self.pos += 1;
            let name = self.name()?;
            self.expect("=")?;
            let values = self.expr(0, false)?;
            self.finish()?;
            let body = self.body()?;
            self.end()?;
            return Ok(StmtKind::For(name, values, body));
        }
        if self.named("while") {
            self.pos += 1;
            let condition = self.expr(0, false)?;
            self.finish()?;
            let body = self.body()?;
            self.end()?;
            return Ok(StmtKind::While(condition, body));
        }
        for (name, stmt) in [
            ("break", StmtKind::Break),
            ("continue", StmtKind::Continue),
            ("return", StmtKind::Return),
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
                return Ok(StmtKind::Assign(Target::Many(names), value));
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
            StmtKind::Assign(target, self.expr(0, false)?)
        } else {
            if !matches!(lhs, Expr::Apply(..)) {
                return Err(
                    self.fail("bare expression display is unsupported; use disp(expression)")
                );
            }
            StmtKind::Call(lhs)
        };
        self.finish()?;
        Ok(result)
    }
    fn function_typed(&mut self) -> Result<Function, Error> {
        self.function_with_declarations(false, true)
    }
    fn function_with_implicit_end(&mut self, implicit: bool) -> Result<Function, Error> {
        self.function_with_declarations(implicit, false)
    }
    fn function_with_declarations(
        &mut self,
        implicit: bool,
        declarations_allowed: bool,
    ) -> Result<Function, Error> {
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
        let mut declarations = BTreeMap::new();
        if self.named("arguments") {
            if !declarations_allowed {
                return Err(self.fail("arguments blocks are supported by typed compilation, not by the bounded interpreter"));
            }
            self.pos += 1;
            self.finish()
                .map_err(|_| self.fail("arguments block attributes are unsupported"))?;
            while !self.named("end") {
                let argument = self.name()?;
                if !args.contains(&argument) {
                    return Err(self.fail("arguments declaration must name a function input"));
                }
                let mut dimensions = [None, None];
                if self.op("(") {
                    for (axis, dimension) in dimensions.iter_mut().enumerate() {
                        if !self.op(":") {
                            let Token::Number(value) = self.token() else {
                                return Err(self.fail(
                                    "argument dimensions require positive integer literals or ':'",
                                ));
                            };
                            if *value < 1.0 || *value > 1_000_000.0 || value.fract() != 0.0 {
                                return Err(self
                                    .fail("argument dimension must be an integer in 1..=1000000"));
                            }
                            *dimension = Some(*value as usize);
                            self.pos += 1;
                        }
                        if axis == 0 {
                            self.expect(",")?;
                        }
                    }
                    self.expect(")")?;
                }
                let kind = match self.name()?.as_str() {
                    "double" => ArgumentKind::Double,
                    "logical" => ArgumentKind::Logical,
                    "char" => ArgumentKind::Character,
                    _ => {
                        return Err(
                            self.fail("typed arguments support only double, logical, and char")
                        );
                    }
                };
                if declarations
                    .insert(argument, ArgumentDeclaration { kind, dimensions })
                    .is_some()
                {
                    return Err(self.fail("duplicate arguments declaration"));
                }
                self.finish()
                    .map_err(|_| self.fail("argument defaults and validators are unsupported"))?;
            }
            self.end()?;
        }
        let body = self.body()?;
        if !(implicit && matches!(self.token(), Token::Eof)) {
            self.end()?;
        }
        Ok(Function {
            name,
            args,
            declarations,
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
fn assigned(body: &[Stmt], vars: &mut BTreeSet<String>) {
    for statement in body {
        match &statement.kind {
            StmtKind::Assign(target, _) => match target {
                Target::Name(n) | Target::Index(n, _) => {
                    vars.insert(n.clone());
                }
                Target::Many(names) => vars.extend(names.iter().cloned()),
            },
            StmtKind::For(n, _, body) => {
                vars.insert(n.clone());
                assigned(body, vars);
            }
            StmtKind::While(_, body) => assigned(body, vars),
            StmtKind::If(branches, other) => {
                for (_, body) in branches {
                    assigned(body, vars);
                }
                assigned(other, vars);
            }
            _ => {}
        }
    }
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
    budget.operations(1).map_err(|e| error(1, e))?;
    for value in workspace.values() {
        budget
            .operations(
                value
                    .rows
                    .saturating_add(value.cols)
                    .saturating_add(value.data.len()),
            )
            .map_err(|e| error(1, e))?;
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
    cancellation: Option<std::sync::Arc<dyn Fn() -> bool + Send + Sync>>,
}
impl Default for ArrayBudget {
    fn default() -> Self {
        Self {
            values: 8_000_000,
            operations: 20_000_000,
            cancellation: None,
        }
    }
}
impl ArrayBudget {
    /// Install a cooperative interruption/deadline check. `true` interrupts.
    /// Checked at evaluation entry and every expression/statement/work charge.
    pub fn with_cancellation(mut self, check: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        self.cancellation = Some(std::sync::Arc::new(check));
        self
    }

    pub fn with_limits(intermediate_elements: usize, operations: usize) -> Self {
        Self {
            values: intermediate_elements,
            operations,
            cancellation: None,
        }
    }
    pub fn remaining_elements(&self) -> usize {
        self.values
    }
    pub fn remaining_operations(&self) -> usize {
        self.operations
    }
    fn operations(&mut self, count: usize) -> Result<(), String> {
        if self.cancellation.as_ref().is_some_and(|check| check()) {
            return Err("execution interrupted".into());
        }

        self.operations = self
            .operations
            .checked_sub(count)
            .ok_or("array expression exceeds aggregate operation budget")?;
        Ok(())
    }
    fn shaped_value(&mut self, value: &crate::array_runtime::Value) -> Result<(), String> {
        self.operations(value.rows.saturating_add(value.cols))?;
        self.value(value.data.len())
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
    eval_with_calls(expr, workspace, end, budget, &mut |name, _, _| {
        Err(format!("unsupported function '{name}'"))
    })
}

type FunctionEvaluator<'a> = dyn FnMut(
        &str,
        Vec<crate::array_runtime::Value>,
        &mut ArrayBudget,
    ) -> crate::array_runtime::ArrayResult<Vec<crate::array_runtime::Value>>
    + 'a;

fn eval_with_calls(
    expr: &Expr,
    workspace: &BTreeMap<String, crate::array_runtime::Value>,
    end: Option<f64>,
    budget: &mut ArrayBudget,
    calls: &mut FunctionEvaluator<'_>,
) -> crate::array_runtime::ArrayResult<crate::array_runtime::Value> {
    use crate::array_runtime::{self as rt, Index, Value};
    budget.operations(1)?;
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
        Expr::Unary(op, x) => rt::unary(op, &eval_with_calls(x, workspace, end, budget, calls)?)?,
        Expr::Binary(op, a, b) if op == "&&" => Value::logical(
            eval_with_calls(a, workspace, end, budget, calls)?.scalar_truth()?
                && eval_with_calls(b, workspace, end, budget, calls)?.scalar_truth()?,
        ),
        Expr::Binary(op, a, b) if op == "||" => Value::logical(
            eval_with_calls(a, workspace, end, budget, calls)?.scalar_truth()?
                || eval_with_calls(b, workspace, end, budget, calls)?.scalar_truth()?,
        ),
        Expr::Binary(op, a, b) => {
            let a = eval_with_calls(a, workspace, end, budget, calls)?;
            let b = eval_with_calls(b, workspace, end, budget, calls)?;
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
            &eval_with_calls(a, workspace, end, budget, calls)?,
            &eval_with_calls(b, workspace, end, budget, calls)?,
            &eval_with_calls(c, workspace, end, budget, calls)?,
        )?,
        Expr::Array(rows) => rt::concatenate(
            rows.iter()
                .map(|row| {
                    row.iter()
                        .map(|v| eval_with_calls(v, workspace, end, budget, calls))
                        .collect()
                })
                .collect::<Result<Vec<_>, _>>()?,
        )?,
        Expr::Apply(name, args) if workspace.contains_key(name) => {
            let value = &workspace[name];
            budget.operations(
                value
                    .rows
                    .saturating_add(value.cols)
                    .saturating_add(value.data.len()),
            )?;
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
                        Ok(Index::Values(eval_with_calls(
                            arg,
                            workspace,
                            Some(value.end_value(i, args.len()).number()?),
                            budget,
                            calls,
                        )?))
                    }
                })
                .collect::<Result<Vec<_>, String>>()?;
            value.index(&indices)?
        }
        Expr::Apply(name, args) => {
            let args = args
                .iter()
                .map(|a| eval_with_calls(a, workspace, end, budget, calls))
                .collect::<Result<Vec<_>, _>>()?;
            (if BUILTINS.contains(&name.as_str())
                && !["disp", "fprintf", "sprintf", "error", "assert"].contains(&name.as_str())
            {
                rt::builtin(name, args, 1)?
            } else {
                calls(name, args, budget)?
            })
            .into_iter()
            .next()
            .ok_or("builtin returned no value")?
        }
    };
    budget.shaped_value(&value)?;
    Ok(value)
}
