//! Parsed, bounded, pure MATLAB function evaluation for simulation blocks.
use super::script::{validate_name, validate_value, validate_workspace};
use super::{
    ArrayBudget, BUILTINS, Expr, Function, Parser, Stmt, Target, Token, assigned, error,
    eval_with_calls, lex,
};
use crate::{
    Error,
    array_runtime::{Environment, Index, Value},
};
use std::collections::{BTreeMap, BTreeSet};
const MAX_VARIABLES: usize = 256;
const MAX_WORKSPACE_ELEMENTS: usize = 100_000;
const MAX_STEPS: usize = 100_000;

/// Declared primary function ports, in source order. Shapes remain runtime values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FunctionSignature {
    pub name: String,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
}
/// Validated function file; parse once and reuse across simulation samples.
pub struct FunctionProgram {
    signature: FunctionSignature,
    functions: BTreeMap<String, Function>,
    stack_costs: BTreeMap<String, usize>,
}
pub fn eval_function(source: &str, args: Vec<Value>) -> Result<Vec<Value>, Error> {
    FunctionProgram::parse(source)?.evaluate(args)
}
impl FunctionProgram {
    pub fn parse(source: &str) -> Result<Self, Error> {
        let mut parser = Parser {
            tokens: lex(source)?,
            pos: 0,
            depth: 0,
        };
        parser.separators();
        let single_function = parser
            .tokens
            .iter()
            .filter(|token| matches!(&token.token,Token::Name(name) if name=="function"))
            .count()
            == 1;
        let mut functions = BTreeMap::new();
        let mut signature = None;
        while parser.named("function") {
            let function = parser.function_with_implicit_end(single_function)?;
            if functions.len() >= 64 {
                return Err(error(1, "function file exceeds 64 definitions"));
            }
            validate_name(&function.name).map_err(|e| error(1, e))?;
            if BUILTINS.contains(&function.name.as_str()) || functions.contains_key(&function.name)
            {
                return Err(error(1, "duplicate function or builtin shadowing"));
            }
            if function.args.len() > 256 || function.outputs.len() > 256 {
                return Err(error(1, "function signature exceeds 256 inputs or outputs"));
            }
            for name in function.args.iter().chain(&function.outputs) {
                validate_name(name).map_err(|e| error(1, e))?;
            }
            if signature.is_none() {
                signature = Some(FunctionSignature {
                    name: function.name.clone(),
                    inputs: function.args.clone(),
                    outputs: function.outputs.clone(),
                });
            }
            functions.insert(function.name.clone(), function);
        }
        if !matches!(parser.token(), Token::Eof) {
            return Err(parser.fail("pure function files cannot contain script statements"));
        }
        let stack_costs = functions
            .iter()
            .map(|(name, f)| (name.clone(), 4 + body_stack(&f.body)))
            .collect();
        let program = Self {
            stack_costs,
            signature: signature.ok_or_else(|| error(1, "function definition required"))?,
            functions,
        };
        for function in program.functions.values() {
            let mut names = function.args.iter().cloned().collect();
            assigned(&function.body, &mut names);
            program
                .validate_body(&function.body, &names, 0)
                .map_err(|e| error(1, e))?;
        }
        Ok(program)
    }
    pub fn signature(&self) -> &FunctionSignature {
        &self.signature
    }
    pub fn evaluate(&self, args: Vec<Value>) -> Result<Vec<Value>, Error> {
        self.evaluate_with_budget(args, &mut ArrayBudget::default())
    }
    pub fn evaluate_with_budget(
        &self,
        args: Vec<Value>,
        budget: &mut ArrayBudget,
    ) -> Result<Vec<Value>, Error> {
        let mut steps = MAX_STEPS;
        self.call(&self.signature.name, args, budget, &mut steps, 0)
            .map_err(|e| error(1, e))
    }
    fn call(
        &self,
        name: &str,
        args: Vec<Value>,
        budget: &mut ArrayBudget,
        steps: &mut usize,
        depth: usize,
    ) -> Result<Vec<Value>, String> {
        budget.operations(1)?;
        if ["error", "assert", "sprintf"].contains(&name) {
            return crate::array_runtime::builtin(name, args, 1);
        }
        let stack = depth.saturating_add(*self.stack_costs.get(name).unwrap_or(&256));
        if stack > 256 {
            return Err(
                "function recursion or combined syntax nesting exceeds stack budget".into(),
            );
        }
        let function = self
            .functions
            .get(name)
            .ok_or_else(|| format!("unknown pure function '{name}'"))?;
        if args.len() != function.args.len() {
            return Err(format!(
                "function '{name}' expects {} inputs, got {}",
                function.args.len(),
                args.len()
            ));
        }
        let env: Environment = function.args.iter().cloned().zip(args).collect();
        let resident = validate_workspace(&env)?;
        budget.value(resident)?;
        let mut interpreter = Interpreter {
            env,
            resident,
            budget,
            steps,
            program: self,
            depth: stack,
        };
        interpreter.tick()?;
        interpreter.body(&function.body)?;
        function
            .outputs
            .iter()
            .map(|name| {
                let value = interpreter
                    .env
                    .get(name)
                    .ok_or_else(|| format!("function output '{name}' was not assigned"))?;
                interpreter.budget.shaped_value(value)?;
                Ok(value.clone())
            })
            .collect()
    }
    fn validate_expr(&self, expr: &Expr, names: &BTreeSet<String>) -> Result<(), String> {
        match expr {
            Expr::Apply(name, args) => {
                if ["disp", "fprintf"].contains(&name.as_str()) {
                    return Err(format!(
                        "I/O builtin '{name}' is forbidden in pure functions"
                    ));
                }
                if !names.contains(name)
                    && !BUILTINS.contains(&name.as_str())
                    && !self.functions.contains_key(name)
                {
                    return Err(format!("unsupported pure function '{name}'"));
                }
                for arg in args {
                    self.validate_expr(arg, names)?;
                }
            }
            Expr::Unary(_, a) => self.validate_expr(a, names)?,
            Expr::Binary(_, a, b) => {
                self.validate_expr(a, names)?;
                self.validate_expr(b, names)?;
            }
            Expr::Range(a, b, c) => {
                self.validate_expr(a, names)?;
                self.validate_expr(b, names)?;
                self.validate_expr(c, names)?;
            }
            Expr::Array(rows) => {
                for value in rows.iter().flatten() {
                    self.validate_expr(value, names)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn validate_body(
        &self,
        body: &[Stmt],
        names: &BTreeSet<String>,
        loops: usize,
    ) -> Result<(), String> {
        for statement in body {
            match statement {
                Stmt::Assign(target, value) => {
                    match target {
                        Target::Name(name) => validate_name(name)?,
                        Target::Many(names) => {
                            for name in names {
                                validate_name(name)?;
                            }
                        }
                        Target::Index(name, args) => {
                            validate_name(name)?;
                            for arg in args {
                                self.validate_expr(arg, names)?;
                            }
                        }
                    }
                    self.validate_expr(value, names)?;
                }
                Stmt::Call(expr) => self.validate_expr(expr, names)?,
                Stmt::If(branches, other) => {
                    for (condition, body) in branches {
                        self.validate_expr(condition, names)?;
                        self.validate_body(body, names, loops)?;
                    }
                    self.validate_body(other, names, loops)?;
                }
                Stmt::For(name, value, body) => {
                    validate_name(name)?;
                    self.validate_expr(value, names)?;
                    self.validate_body(body, names, loops + 1)?;
                }
                Stmt::While(value, body) => {
                    self.validate_expr(value, names)?;
                    self.validate_body(body, names, loops + 1)?;
                }
                Stmt::Break | Stmt::Continue if loops == 0 => {
                    return Err("break/continue outside a loop".into());
                }
                _ => {}
            }
        }
        Ok(())
    }
}

// Bound combined expression/statement/call nesting, not merely call count:
// deeply nested expressions in recursive functions must not multiply stack use.
fn body_stack(body: &[Stmt]) -> usize {
    use super::expression_depth;
    body.iter()
        .map(|stmt| {
            1 + match stmt {
                Stmt::Assign(Target::Index(_, args), value) => expression_depth(value)
                    .max(args.iter().map(expression_depth).max().unwrap_or(0)),
                Stmt::Assign(_, value) | Stmt::Call(value) => expression_depth(value),
                Stmt::If(branches, other) => branches
                    .iter()
                    .map(|(condition, body)| expression_depth(condition).max(body_stack(body)))
                    .max()
                    .unwrap_or(0)
                    .max(body_stack(other)),
                Stmt::For(_, value, body) | Stmt::While(value, body) => {
                    expression_depth(value).max(body_stack(body))
                }
                _ => 0,
            }
        })
        .max()
        .unwrap_or(0)
}

#[derive(PartialEq)]
enum Flow {
    Next,
    Break,
    Continue,
    Return,
}
struct Interpreter<'a> {
    env: Environment,
    budget: &'a mut ArrayBudget,
    resident: usize,
    steps: &'a mut usize,
    program: &'a FunctionProgram,
    depth: usize,
}
impl Interpreter<'_> {
    fn outputs(&mut self, expression: &Expr, count: usize) -> Result<Vec<Value>, String> {
        let Expr::Apply(name, args) = expression else {
            return Err("multiple outputs require a function call".into());
        };
        if self.env.contains_key(name) {
            return Err("indexed array cannot provide multiple outputs".into());
        }
        let args = args
            .iter()
            .map(|arg| self.expression(arg))
            .collect::<Result<Vec<_>, _>>()?;
        let values = if BUILTINS.contains(&name.as_str()) {
            crate::array_runtime::builtin(name, args, count)?
        } else {
            self.program
                .call(name, args, self.budget, self.steps, self.depth + 1)?
        };
        if count > values.len() {
            return Err("too many requested function outputs".into());
        }
        for value in &values {
            self.budget.shaped_value(value)?;
        }
        Ok(values.into_iter().take(count).collect())
    }

    fn tick(&mut self) -> Result<(), String> {
        *self.steps = self
            .steps
            .checked_sub(1)
            .ok_or("initialization script exceeds 100000 steps")?;
        self.budget.operations(1)
    }
    fn expression(&mut self, expression: &Expr) -> Result<Value, String> {
        eval_with_calls(
            expression,
            &self.env,
            None,
            self.budget,
            &mut |name, args, budget| {
                self.program
                    .call(name, args, budget, self.steps, self.depth + 1)
            },
        )
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
                            Index::Values(eval_with_calls(
                                arg,
                                &self.env,
                                Some(value.end_value(dimension, args.len()).number()?),
                                self.budget,
                                &mut |name, args, budget| {
                                    self.program.call(
                                        name,
                                        args,
                                        budget,
                                        self.steps,
                                        self.depth + 1,
                                    )
                                },
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
                            match self.body(body)? {
                                Flow::Break => break,
                                Flow::Return => return Ok(Flow::Return),
                                _ => {}
                            }
                        }
                    }
                }
                Stmt::While(condition, body) => loop {
                    self.tick()?;
                    if !self.expression(condition)?.truth()? {
                        break;
                    }
                    match self.body(body)? {
                        Flow::Break => break,
                        Flow::Return => return Ok(Flow::Return),
                        _ => {}
                    }
                },
                Stmt::Break => return Ok(Flow::Break),
                Stmt::Continue => return Ok(Flow::Continue),
                Stmt::Return => return Ok(Flow::Return),
                Stmt::Assign(Target::Many(names), expr) => {
                    let values = self.outputs(expr, names.len())?;
                    for (name, value) in names.iter().zip(values) {
                        self.store(name, value)?;
                    }
                }
                Stmt::Call(expr) => {
                    self.outputs(expr, 0)?;
                }
            }
        }
        Ok(Flow::Next)
    }
}
