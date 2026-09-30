//! In-process initialization scripts. This interpreter never invokes generated
//! code, user functions, I/O builtins, or external tools.
use super::{ArrayBudget, BUILTINS, Expr, Parser, Stmt, Target, Token, assigned, error, eval, lex};
use crate::{
    Error,
    array_runtime::{Environment, Index, Value},
};
use std::collections::BTreeSet;

const MAX_VARIABLES: usize = 256;
const MAX_VALUE_ELEMENTS: usize = 1024;
const MAX_WORKSPACE_ELEMENTS: usize = 100_000;
const MAX_STEPS: usize = 100_000;

/// Evaluate a bounded initialization script against a copy of `workspace`.
/// Errors never modify the caller's workspace. All expressions are pure.
pub fn eval_script(source: &str, workspace: &Environment) -> Result<Environment, Error> {
    eval_script_with_budget(source, workspace, &mut ArrayBudget::default())
}

/// As `eval_script`, sharing expression work with subsequent model parameters.
/// Caps: 256 variables, 1024 elements per stored value, 100000 stored elements,
/// and 100000 statement/loop-condition steps. Intermediate arrays retain the
/// expression evaluator's one-million-element cap and shared aggregate budget.
pub fn eval_script_with_budget(
    source: &str,
    workspace: &Environment,
    budget: &mut ArrayBudget,
) -> Result<Environment, Error> {
    budget.operations(1).map_err(|e| error(1, e))?;
    let resident = validate_workspace(workspace).map_err(|e| error(1, e))?;
    budget.value(resident).map_err(|e| error(1, e))?;
    let tokens = lex(source)?;
    budget.operations(tokens.len()).map_err(|e| error(1, e))?;
    let mut parser = Parser {
        tokens,
        pos: 0,
        depth: 0,
    };
    let body = parser.body()?;
    if !matches!(parser.token(), Token::Eof) {
        return Err(parser.fail(
            "functions and trailing control-flow tokens are unavailable in initialization scripts",
        ));
    }
    let mut names: BTreeSet<String> = workspace.keys().cloned().collect();
    assigned(&body, &mut names);
    validate_body(&body, &names, 0).map_err(|e| error(1, e))?;
    let mut interpreter = Interpreter {
        env: workspace.clone(),
        budget,
        resident,
        steps: MAX_STEPS,
    };
    interpreter.body(&body).map_err(|e| error(1, e))?;
    Ok(interpreter.env)
}

pub(super) fn validate_workspace(env: &Environment) -> Result<usize, String> {
    if env.len() > MAX_VARIABLES {
        return Err("initialization workspace exceeds 256 variables".into());
    }
    let mut count = 0usize;
    for (name, value) in env {
        validate_name(name)?;
        validate_value(value)?;
        count += value.data.len();
    }
    if count > MAX_WORKSPACE_ELEMENTS {
        return Err("initialization workspace exceeds 100000 elements".into());
    }
    Ok(count)
}
pub(super) fn validate_name(name: &str) -> Result<(), String> {
    if name.len() > 63
        || !name.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return Err("initialization variable names require an ASCII letter followed by letters, digits or underscores (maximum 63 characters)".into());
    }
    Ok(())
}
pub(super) fn validate_value(value: &Value) -> Result<(), String> {
    value.validate()?;
    if value.rows > MAX_VALUE_ELEMENTS || value.cols > MAX_VALUE_ELEMENTS {
        return Err("initialization variable dimensions exceed 1024".into());
    }
    if value.data.len() > MAX_VALUE_ELEMENTS {
        return Err("initialization variable exceeds 1024 elements".into());
    }
    Ok(())
}

// Inspect dead branches too: unsupported capabilities are rejected before any
// interpretation rather than depending on which branch happens to execute.
fn validate_expr(expr: &Expr, names: &BTreeSet<String>) -> Result<(), String> {
    match expr {
        Expr::Apply(name, args) => {
            if ["disp", "fprintf", "sprintf", "error", "assert"].contains(&name.as_str()) {
                return Err(format!("'{name}' is unavailable in initialization scripts"));
            }
            if !names.contains(name) && !BUILTINS.contains(&name.as_str()) {
                return Err(format!("unsupported initialization function '{name}'"));
            }
            for arg in args {
                validate_expr(arg, names)?;
            }
        }
        Expr::Unary(_, a) => validate_expr(a, names)?,
        Expr::Binary(_, a, b) => {
            validate_expr(a, names)?;
            validate_expr(b, names)?;
        }
        Expr::Range(a, b, c) => {
            validate_expr(a, names)?;
            validate_expr(b, names)?;
            validate_expr(c, names)?;
        }
        Expr::Array(rows) => {
            for item in rows.iter().flatten() {
                validate_expr(item, names)?;
            }
        }
        _ => {}
    }
    Ok(())
}
fn validate_body(body: &[Stmt], names: &BTreeSet<String>, loops: usize) -> Result<(), String> {
    for stmt in body {
        match stmt {
            Stmt::Assign(Target::Many(_), _) => {
                return Err(
                    "multiple-output assignment is unavailable in initialization scripts".into(),
                );
            }
            Stmt::Assign(target, value) => {
                if let Target::Index(_, args) = target {
                    for arg in args {
                        validate_expr(arg, names)?;
                    }
                }
                validate_expr(value, names)?;
            }
            Stmt::If(branches, other) => {
                for (condition, branch) in branches {
                    validate_expr(condition, names)?;
                    validate_body(branch, names, loops)?;
                }
                validate_body(other, names, loops)?;
            }
            Stmt::For(_, value, body) | Stmt::While(value, body) => {
                validate_expr(value, names)?;
                validate_body(body, names, loops + 1)?;
            }
            Stmt::Break | Stmt::Continue if loops == 0 => {
                return Err("break/continue outside a loop".into());
            }
            Stmt::Break | Stmt::Continue => {}
            Stmt::Call(_) => {
                return Err(
                    "call statements and printing are unavailable in initialization scripts".into(),
                );
            }
            Stmt::Return => return Err("return is unavailable in initialization scripts".into()),
        }
    }
    Ok(())
}

#[derive(PartialEq)]
enum Flow {
    Next,
    Break,
    Continue,
}
struct Interpreter<'a> {
    env: Environment,
    budget: &'a mut ArrayBudget,
    resident: usize,
    steps: usize,
}
impl Interpreter<'_> {
    fn tick(&mut self) -> Result<(), String> {
        self.steps = self
            .steps
            .checked_sub(1)
            .ok_or("initialization script exceeds 100000 steps")?;
        self.budget.operations(1)
    }
    fn expression(&mut self, expression: &Expr) -> Result<Value, String> {
        eval(expression, &self.env, None, self.budget)
    }
    fn store(&mut self, name: &str, value: Value) -> Result<(), String> {
        validate_name(name)?;
        validate_value(&value)?;
        let previous = self.env.get(name);
        if previous.is_none() && self.env.len() >= MAX_VARIABLES {
            return Err("initialization workspace exceeds 256 variables".into());
        }
        let resident = self.resident - previous.map_or(0, |v| v.data.len()) + value.data.len();
        if resident > MAX_WORKSPACE_ELEMENTS {
            return Err("initialization workspace exceeds 100000 elements".into());
        }
        self.resident = resident;
        self.env.insert(name.into(), value);
        Ok(())
    }
    fn body(&mut self, statements: &[Stmt]) -> Result<Flow, String> {
        for statement in statements {
            self.tick()?;
            match statement {
                Stmt::Assign(Target::Name(name), expression) => {
                    let value = self.expression(expression)?;
                    self.store(name, value)?;
                }
                Stmt::Assign(Target::Index(name, args), expression) => {
                    let rhs = self.expression(expression)?;
                    if args.is_empty() || args.len() > 2 {
                        return Err("one or two array indices required".into());
                    }
                    let mut value = self.env.get(name).cloned().unwrap_or_else(Value::empty);
                    self.budget.shaped_value(&value)?;
                    let mut indices = Vec::new();
                    for (dimension, arg) in args.iter().enumerate() {
                        indices.push(if matches!(arg, Expr::All) {
                            Index::All
                        } else {
                            Index::Values(eval(
                                arg,
                                &self.env,
                                Some(value.end_value(dimension, args.len()).number()?),
                                self.budget,
                            )?)
                        });
                    }
                    value.assign(&indices, &rhs)?;
                    self.budget.shaped_value(&value)?;
                    self.store(name, value)?;
                }
                Stmt::If(branches, other) => {
                    let mut chosen = other;
                    for (condition, body) in branches {
                        if self.expression(condition)?.truth()? {
                            chosen = body;
                            break;
                        }
                    }
                    let flow = self.body(chosen)?;
                    if flow != Flow::Next {
                        return Ok(flow);
                    }
                }
                Stmt::For(name, expression, body) => {
                    let values = self.expression(expression)?;
                    if values.rows == 0 || values.cols == 0 {
                        self.store(name, values)?;
                    } else {
                        for column in values.columns() {
                            self.tick()?;
                            self.budget.shaped_value(&column)?;
                            self.store(name, column)?;
                            if self.body(body)? == Flow::Break {
                                break;
                            }
                        }
                    }
                }
                Stmt::While(condition, body) => loop {
                    self.tick()?;
                    if !self.expression(condition)?.truth()? {
                        break;
                    }
                    if self.body(body)? == Flow::Break {
                        break;
                    }
                },
                Stmt::Break => return Ok(Flow::Break),
                Stmt::Continue => return Ok(Flow::Continue),
                _ => return Err("unsupported initialization statement".into()),
            }
        }
        Ok(Flow::Next)
    }
}
