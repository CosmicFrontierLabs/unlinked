//! Conservative typed Rust lowering. The interpreter remains the semantic reference.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ty {
    Number,
    Bool,
    Text,
    Numbers,
    Bools,
    Chars,
}
impl Ty {
    fn rust(self) -> &'static str {
        match self {
            Self::Number => "f64",
            Self::Bool => "bool",
            Self::Text => "String",
            Self::Numbers => "ArrayD<f64>",
            Self::Bools => "ArrayD<bool>",
            Self::Chars => "ArrayD<u8>",
        }
    }
    fn scalar(self) -> bool {
        matches!(self, Self::Number | Self::Bool)
    }
    fn character(self) -> bool {
        matches!(self, Self::Text | Self::Chars)
    }
    fn array(self) -> Self {
        match self {
            Self::Bool | Self::Bools => Self::Bools,
            Self::Text | Self::Chars => Self::Chars,
            _ => Self::Numbers,
        }
    }
    fn numeric(self) -> Self {
        if self.scalar() {
            Self::Number
        } else {
            Self::Numbers
        }
    }
    fn logical(self) -> Self {
        if self.scalar() {
            Self::Bool
        } else {
            Self::Bools
        }
    }
}
fn join(a: Ty, b: Ty) -> Result<Ty, Error> {
    if a == b {
        return Ok(a);
    }
    if a.character() != b.character() {
        return Err(error(
            1,
            "typed code cannot join character and numeric assignments to the same variable",
        ));
    }
    if a.character() {
        return Ok(Ty::Chars);
    }
    if matches!(a, Ty::Bool | Ty::Bools) != matches!(b, Ty::Bool | Ty::Bools) {
        return Err(error(
            1,
            "typed code cannot join logical and numeric assignments: logical masks and numeric indices have different semantics",
        ));
    }
    Ok(if a.scalar() && b.scalar() {
        Ty::Number
    } else if matches!(a, Ty::Bool | Ty::Bools) && matches!(b, Ty::Bool | Ty::Bools) {
        Ty::Bools
    } else {
        Ty::Numbers
    })
}
#[derive(Clone)]
struct Signature {
    args: Vec<Ty>,
    outputs: Vec<Option<Ty>>,
}
type Types = BTreeMap<String, Ty>;
fn builtin_ty(name: &str, args: &[Ty]) -> Result<Ty, Error> {
    let first = args.first().copied().unwrap_or(Ty::Numbers);
    Ok(match name {
        "sprintf" | "num2str" => Ty::Text,
        "strcmp" | "isempty" => Ty::Bool,
        "numel" | "length" | "norm" | "dot" | "fprintf" => Ty::Number,
        "det" => Ty::Number,
        "inv" => Ty::Numbers,
        "size" if args.len() == 2 => Ty::Number,
        "size" | "zeros" | "ones" | "eye" | "linspace" | "find" => Ty::Numbers,
        "reshape" | "diag" => first.array(),
        "transpose" => {
            if first == Ty::Text {
                Ty::Chars
            } else {
                first
            }
        }
        "sort" => first,
        "all" | "any" | "isnan" | "isinf" | "isfinite" => first.logical(),
        "sum" | "prod" => first.numeric(),
        "min" | "max" | "mod" | "rem" | "atan2" => {
            if args.iter().all(|t| t.scalar()) {
                Ty::Number
            } else {
                Ty::Numbers
            }
        }
        "abs" | "sin" | "cos" | "tan" | "asin" | "acos" | "atan" | "sqrt" | "exp" | "log"
        | "log2" | "log10" | "floor" | "ceil" | "round" | "sign" => first.numeric(),
        "disp" | "assert" | "error" => {
            return Err(error(1, format!("{name} does not produce a typed value")));
        }
        _ => return Err(error(1, format!("unsupported function '{name}'"))),
    })
}
fn expr_ty(
    expr: &Expr,
    types: &Types,
    vars: &BTreeSet<String>,
    functions: &BTreeMap<String, Signature>,
) -> Result<Option<Ty>, Error> {
    Ok(match expr {
        Expr::Number(_) | Expr::End => Some(Ty::Number),
        Expr::Text(_) => Some(Ty::Text),
        Expr::All => None,
        Expr::Var(n) => {
            if vars.contains(n) {
                types.get(n).copied()
            } else {
                Some(match n.as_str() {
                    "pi" | "Inf" | "inf" | "NaN" | "nan" => Ty::Number,
                    "true" | "false" => Ty::Bool,
                    _ => return Err(error(1, format!("undefined variable '{n}'"))),
                })
            }
        }
        Expr::Unary(op, a) => expr_ty(a, types, vars, functions)?.map(|t| match op.as_str() {
            "~" => t.logical(),
            "'" | ".'" => {
                if t == Ty::Text {
                    Ty::Chars
                } else {
                    t
                }
            }
            _ => t.numeric(),
        }),
        Expr::Binary(op, a, b) => {
            let (Some(a), Some(b)) = (
                expr_ty(a, types, vars, functions)?,
                expr_ty(b, types, vars, functions)?,
            ) else {
                return Ok(None);
            };
            let scalar = a.scalar() && b.scalar();
            Some(if ["&&", "||"].contains(&op.as_str()) {
                Ty::Bool
            } else if ["==", "~=", "<", "<=", ">", ">=", "&", "|"].contains(&op.as_str()) {
                if scalar { Ty::Bool } else { Ty::Bools }
            } else if scalar {
                Ty::Number
            } else {
                Ty::Numbers
            })
        }
        Expr::Range(..) => Some(Ty::Numbers),
        Expr::Array(rows) => {
            let mut result = None;
            for e in rows.iter().flatten() {
                let Some(t) = expr_ty(e, types, vars, functions)? else {
                    return Ok(None);
                };
                result = Some(match result {
                    None => t,
                    Some(previous) => join(previous, t)?,
                });
            }
            Some(result.unwrap_or(Ty::Numbers).array())
        }
        Expr::Apply(n, args) if vars.contains(n) => {
            let Some(base) = types.get(n).copied() else {
                return Ok(None);
            };
            let scalar = args
                .iter()
                .map(|e| expr_ty(e, types, vars, functions))
                .collect::<Result<Vec<_>, _>>()?
                .iter()
                .all(|t| *t == Some(Ty::Number));
            Some(if scalar {
                match base {
                    Ty::Bool | Ty::Bools => Ty::Bool,
                    Ty::Text | Ty::Chars => Ty::Text,
                    _ => Ty::Number,
                }
            } else {
                base.array()
            })
        }
        Expr::Apply(n, args) => {
            if let Some(sig) = functions.get(n) {
                if sig.args.len() != args.len() {
                    return Err(error(
                        1,
                        format!("{n} expects {} arguments", sig.args.len()),
                    ));
                }
                sig.outputs.first().copied().flatten()
            } else {
                let Some(args) = args
                    .iter()
                    .map(|e| expr_ty(e, types, vars, functions))
                    .collect::<Result<Option<Vec<_>>, _>>()?
                else {
                    return Ok(None);
                };
                Some(builtin_ty(n, &args)?)
            }
        }
    })
}
fn assign_type(types: &mut Types, name: &str, value: Ty) -> Result<(), Error> {
    let value = if let Some(old) = types.get(name) {
        join(*old, value)?
    } else {
        value
    };
    types.insert(name.into(), value);
    Ok(())
}
fn expression_work(e: &Expr) -> usize {
    1 + match e {
        Expr::Unary(_, a) => expression_work(a),
        Expr::Binary(_, a, b) => expression_work(a) + expression_work(b),
        Expr::Range(a, b, c) => expression_work(a) + expression_work(b) + expression_work(c),
        Expr::Apply(_, args) => args.iter().map(expression_work).sum(),
        Expr::Array(rows) => rows.iter().flatten().map(expression_work).sum(),
        _ => 0,
    }
}
fn statement_work(s: &Stmt) -> usize {
    1 + match s {
        Stmt::Assign(Target::Index(_, args), e) => {
            expression_work(e) + args.iter().map(expression_work).sum::<usize>()
        }
        Stmt::Assign(_, e) | Stmt::Call(e) | Stmt::For(_, e, _) | Stmt::While(e, _) => {
            expression_work(e)
        }
        Stmt::If(branches, _) => branches.iter().map(|(e, _)| expression_work(e)).sum(),
        _ => 0,
    }
}
fn infer_body(
    body: &[Stmt],
    types: &mut Types,
    vars: &BTreeSet<String>,
    functions: &BTreeMap<String, Signature>,
    budget: &mut usize,
) -> Result<(), Error> {
    for s in body {
        *budget = budget
            .checked_sub(statement_work(s))
            .ok_or_else(|| error(1, "typed inference work budget exceeded"))?;
        match s {
            Stmt::Assign(Target::Name(n), e) => {
                if let Some(t) = expr_ty(e, types, vars, functions)? {
                    assign_type(types, n, t)?
                }
            }
            Stmt::Assign(Target::Index(n, _), e) => {
                if let Some(t) = expr_ty(e, types, vars, functions)? {
                    let target = types.get(n).copied().unwrap_or(t).array();
                    assign_type(types, n, target)?
                }
            }
            Stmt::Assign(Target::Many(names), e) if names.len() == 1 => {
                if let Some(t) = expr_ty(e, types, vars, functions)? {
                    assign_type(types, &names[0], t)?;
                }
            }
            Stmt::Assign(Target::Many(names), Expr::Apply(n, args)) => {
                if let Some(sig) = functions.get(n) {
                    if names.len() > sig.outputs.len() {
                        return Err(error(
                            1,
                            format!("{n} supplies only {} outputs", sig.outputs.len()),
                        ));
                    }
                    for (name, t) in names.iter().zip(&sig.outputs) {
                        if let Some(t) = t {
                            assign_type(types, name, *t)?
                        }
                    }
                } else {
                    let Some(argtypes) = args
                        .iter()
                        .map(|e| expr_ty(e, types, vars, functions))
                        .collect::<Result<Option<Vec<_>>, _>>()?
                    else {
                        continue;
                    };
                    if !["size", "min", "max", "sort", "find"].contains(&n.as_str()) {
                        return Err(error(1, format!("multiple outputs unsupported for {n}")));
                    }
                    for (i, name) in names.iter().enumerate() {
                        let t = if n == "size" {
                            Ty::Number
                        } else if i == 0 {
                            builtin_ty(n, &argtypes)?.numeric()
                        } else {
                            Ty::Numbers
                        };
                        assign_type(types, name, t)?
                    }
                }
            }
            Stmt::Assign(Target::Many(_), _) => {
                return Err(error(1, "multiple assignment requires a function call"));
            }
            Stmt::For(n, e, b) => {
                if let Some(t) = expr_ty(e, types, vars, functions)? {
                    assign_type(types, n, t.array())?
                }
                infer_body(b, types, vars, functions, budget)?
            }
            Stmt::While(_, b) => infer_body(b, types, vars, functions, budget)?,
            Stmt::If(branches, other) => {
                for (_, b) in branches {
                    infer_body(b, types, vars, functions, budget)?
                }
                infer_body(other, types, vars, functions, budget)?
            }
            _ => {}
        }
    }
    Ok(())
}
fn scope_types(
    body: &[Stmt],
    args: &[(String, Ty)],
    functions: &BTreeMap<String, Signature>,
    budget: &mut usize,
) -> Result<(Types, BTreeSet<String>), Error> {
    let mut vars = args.iter().map(|(n, _)| n.clone()).collect();
    assigned(body, &mut vars);
    let mut types: Types = args.iter().cloned().collect();
    for _ in 0..(vars.len().saturating_mul(3) + 1).min(50_000) {
        let before = types.clone();
        infer_body(body, &mut types, &vars, functions, budget)?;
        if before == types {
            break;
        }
    }
    Ok((types, vars))
}
fn call_evidence_expr(
    expr: &Expr,
    types: &Types,
    vars: &BTreeSet<String>,
    functions: &BTreeMap<String, Signature>,
    evidence: &mut BTreeMap<String, Vec<Option<Ty>>>,
) -> Result<(), Error> {
    match expr {
        Expr::Apply(name, args) => {
            if functions.contains_key(name) && !vars.contains(name) {
                let slots = evidence
                    .entry(name.clone())
                    .or_insert_with(|| vec![None; args.len()]);
                if slots.len() != args.len() {
                    return Err(error(1, "function argument count mismatch"));
                }
                for (slot, arg) in slots.iter_mut().zip(args) {
                    if let Some(t) = expr_ty(arg, types, vars, functions)? {
                        let t = if t == Ty::Text { Ty::Text } else { t.array() };
                        *slot = Some(if let Some(old) = *slot {
                            join(old, t)?
                        } else {
                            t
                        });
                    }
                }
            }
            for arg in args {
                call_evidence_expr(arg, types, vars, functions, evidence)?;
            }
        }
        Expr::Unary(_, a) => call_evidence_expr(a, types, vars, functions, evidence)?,
        Expr::Binary(_, a, b) => {
            call_evidence_expr(a, types, vars, functions, evidence)?;
            call_evidence_expr(b, types, vars, functions, evidence)?;
        }
        Expr::Range(a, b, c) => {
            for e in [a, b, c] {
                call_evidence_expr(e, types, vars, functions, evidence)?;
            }
        }
        Expr::Array(rows) => {
            for e in rows.iter().flatten() {
                call_evidence_expr(e, types, vars, functions, evidence)?;
            }
        }
        _ => {}
    }
    Ok(())
}
fn call_evidence(
    body: &[Stmt],
    types: &Types,
    vars: &BTreeSet<String>,
    functions: &BTreeMap<String, Signature>,
    evidence: &mut BTreeMap<String, Vec<Option<Ty>>>,
) -> Result<(), Error> {
    for s in body {
        match s {
            Stmt::Assign(target, e) => {
                call_evidence_expr(e, types, vars, functions, evidence)?;
                if let Target::Index(_, args) = target {
                    for e in args {
                        call_evidence_expr(e, types, vars, functions, evidence)?;
                    }
                }
            }
            Stmt::Call(e) => call_evidence_expr(e, types, vars, functions, evidence)?,
            Stmt::If(branches, other) => {
                for (c, b) in branches {
                    call_evidence_expr(c, types, vars, functions, evidence)?;
                    call_evidence(b, types, vars, functions, evidence)?;
                }
                call_evidence(other, types, vars, functions, evidence)?;
            }
            Stmt::While(e, b) | Stmt::For(_, e, b) => {
                call_evidence_expr(e, types, vars, functions, evidence)?;
                call_evidence(b, types, vars, functions, evidence)?;
            }
            _ => {}
        }
    }
    Ok(())
}
fn reads(
    expr: &Expr,
    vars: &BTreeSet<String>,
    defined: &BTreeSet<String>,
    optional: &mut BTreeSet<String>,
) {
    match expr {
        Expr::Var(n) => {
            if vars.contains(n) && !defined.contains(n) {
                optional.insert(n.clone());
            }
        }
        Expr::Apply(n, args) => {
            if vars.contains(n) && !defined.contains(n) {
                optional.insert(n.clone());
            }
            for a in args {
                reads(a, vars, defined, optional)
            }
        }
        Expr::Unary(_, a) => reads(a, vars, defined, optional),
        Expr::Binary(_, a, b) => {
            reads(a, vars, defined, optional);
            reads(b, vars, defined, optional)
        }
        Expr::Range(a, b, c) => {
            reads(a, vars, defined, optional);
            reads(b, vars, defined, optional);
            reads(c, vars, defined, optional)
        }
        Expr::Array(rows) => {
            for e in rows.iter().flatten() {
                reads(e, vars, defined, optional)
            }
        }
        _ => {}
    }
}
fn definite(
    body: &[Stmt],
    vars: &BTreeSet<String>,
    mut defined: BTreeSet<String>,
    optional: &mut BTreeSet<String>,
    outputs: &[String],
) -> BTreeSet<String> {
    for s in body {
        match s {
            Stmt::Assign(target, e) => {
                reads(e, vars, &defined, optional);
                match target {
                    Target::Name(n) => {
                        defined.insert(n.clone());
                    }
                    Target::Many(ns) => defined.extend(ns.iter().cloned()),
                    Target::Index(n, args) => {
                        if !defined.contains(n) {
                            optional.insert(n.clone());
                        }
                        for e in args {
                            reads(e, vars, &defined, optional)
                        }
                        defined.insert(n.clone());
                    }
                }
            }
            Stmt::Call(e) => reads(e, vars, &defined, optional),
            Stmt::If(branches, other) => {
                let mut endings = vec![];
                for (c, b) in branches {
                    reads(c, vars, &defined, optional);
                    endings.push(definite(b, vars, defined.clone(), optional, outputs));
                }
                endings.push(definite(other, vars, defined.clone(), optional, outputs));
                defined = endings
                    .into_iter()
                    .reduce(|a, b| a.intersection(&b).cloned().collect())
                    .unwrap_or(defined);
            }
            Stmt::While(e, b) => {
                reads(e, vars, &defined, optional);
                definite(b, vars, defined.clone(), optional, outputs);
            }
            Stmt::For(n, e, b) => {
                reads(e, vars, &defined, optional);
                defined.insert(n.clone());
                definite(b, vars, defined.clone(), optional, outputs);
            }
            Stmt::Return => {
                for n in outputs {
                    if !defined.contains(n) {
                        optional.insert(n.clone());
                    }
                }
            }
            _ => {}
        }
    }
    defined
}
struct TypedGenerator<'a> {
    types: Types,
    vars: BTreeSet<String>,
    optional: BTreeSet<String>,
    functions: &'a BTreeMap<String, Signature>,
    outputs: Vec<String>,
    serial: usize,
    in_function: bool,
}
impl TypedGenerator<'_> {
    fn ty(&self, e: &Expr) -> Result<Ty, Error> {
        expr_ty(e,&self.types,&self.vars,self.functions)?.ok_or_else(||error(1,"cannot determine expression type; recursive functions require a resolvable output type"))
    }
    fn variable(&self, n: &str) -> String {
        if self.optional.contains(n) {
            format!(
                "v_{n}.as_ref().ok_or_else(|| {:?}.to_string())?.clone()",
                format!("undefined variable '{n}'")
            )
        } else {
            format!("v_{n}.clone()")
        }
    }
    fn cast(&self, code: String, from: Ty, to: Ty) -> String {
        if from == to {
            code
        } else {
            format!("rt::convert::<{}>(&({code}))?", to.rust())
        }
    }
    fn scalar(&mut self, e: &Expr, end: Option<&str>) -> Result<String, Error> {
        let t = self.ty(e)?;
        let c = self.expr(e, end)?;
        Ok(self.cast(c, t, Ty::Number))
    }
    fn args(&mut self, args: &[Expr], end: Option<&str>) -> Result<String, Error> {
        args.iter()
            .map(|e| Ok(format!("&({}) as &dyn rt::Matlab", self.expr(e, end)?)))
            .collect::<Result<Vec<_>, Error>>()
            .map(|v| v.join(", "))
    }
    fn expr(&mut self, e: &Expr, end: Option<&str>) -> Result<String, Error> {
        let ty = self.ty(e)?;
        Ok(match e {
            Expr::Number(n) => format!("{n:?}_f64"),
            Expr::Text(s) => format!("String::from({s:?})"),
            Expr::Var(n) => {
                if self.vars.contains(n) {
                    self.variable(n)
                } else {
                    match n.as_str() {
                        "pi" => "std::f64::consts::PI".into(),
                        "Inf" | "inf" => "f64::INFINITY".into(),
                        "NaN" | "nan" => "f64::NAN".into(),
                        "true" | "false" => n.clone(),
                        _ => return Err(error(1, format!("undefined variable '{n}'"))),
                    }
                }
            }
            Expr::End => end
                .ok_or_else(|| error(1, "'end' is only valid within indexing"))?
                .into(),
            Expr::All => return Err(error(1, "':' is only valid as an array index")),
            Expr::Unary(op, a) => {
                let a_ty = self.ty(a)?;
                let v = self.expr(a, end)?;
                match op.as_str() {
                    "+" if a_ty.scalar() => self.cast(v, a_ty, Ty::Number),
                    "-" if a_ty.scalar() => format!("(-({}))", self.cast(v, a_ty, Ty::Number)),
                    "~" if a_ty.scalar() => format!("!rt::scalar_truth(&({v}))?"),
                    "'" | ".'" if a_ty.scalar() => v,
                    _ => format!("rt::unary::<{}>({op:?}, &({v}))?", ty.rust()),
                }
            }
            Expr::Binary(op, a, b) => {
                let (a_ty, b_ty) = (self.ty(a)?, self.ty(b)?);
                let (a, b) = (self.expr(a, end)?, self.expr(b, end)?);
                if op == "&&" || op == "||" {
                    format!("(rt::scalar_truth(&({a}))? {op} rt::scalar_truth(&({b}))?)")
                } else if a_ty.scalar() && b_ty.scalar() {
                    let (a, b) = (
                        self.cast(a, a_ty, Ty::Number),
                        self.cast(b, b_ty, Ty::Number),
                    );
                    match op.as_str() {
                        "+" | "-" | "*" | "/" | "<" | "<=" | ">" | ">=" | "==" => {
                            format!("(({a}) {op} ({b}))")
                        }
                        ".*" => format!("(({a}) * ({b}))"),
                        "./" => format!("(({a}) / ({b}))"),
                        "\\" | ".\\" => format!("(({b}) / ({a}))"),
                        "^" | ".^" => format!("rt::binary::<f64>({op:?}, &({a}), &({b}))?"),
                        "~=" => format!("(({a}) != ({b}))"),
                        "&" | "|" => format!("rt::binary::<bool>({op:?}, &({a}), &({b}))?"),
                        _ => return Err(error(1, format!("unsupported scalar operator {op}"))),
                    }
                } else {
                    format!("rt::binary::<{}>({op:?}, &({a}), &({b}))?", ty.rust())
                }
            }
            Expr::Range(a, b, c) => format!(
                "rt::range({}, {}, {})?",
                self.scalar(a, end)?,
                self.scalar(b, end)?,
                self.scalar(c, end)?
            ),
            Expr::Array(rows) => {
                let rows = rows
                    .iter()
                    .map(|r| Ok(format!("&[{}]", self.args(r, end)?)))
                    .collect::<Result<Vec<_>, Error>>()?;
                format!("rt::concatenate::<{}>(&[{}])?", ty.rust(), rows.join(", "))
            }
            Expr::Apply(n, args) if self.vars.contains(n) => {
                let base = self.variable(n);
                let indices = self.indices(args, "indexed")?;
                format!(
                    "{{ let indexed = {base}; rt::index::<{}>(&indexed, &[{indices}])? }}",
                    ty.rust()
                )
            }
            Expr::Apply(n, args) => self.call(n, args, 1, end)?.0,
        })
    }
    fn indices(&mut self, args: &[Expr], base: &str) -> Result<String, Error> {
        if args.is_empty() || args.len() > 2 {
            return Err(error(1, "one or two array indices required"));
        }
        args.iter()
            .enumerate()
            .map(|(i, e)| {
                if matches!(e, Expr::All) {
                    Ok("rt::Index::All".into())
                } else {
                    Ok(format!(
                        "rt::Index::values(&({}))?",
                        self.expr(e, Some(&format!("rt::end(&{base}, {i}, {})?", args.len())))?
                    ))
                }
            })
            .collect::<Result<Vec<_>, Error>>()
            .map(|v| v.join(", "))
    }
    fn call(
        &mut self,
        n: &str,
        args: &[Expr],
        count: usize,
        end: Option<&str>,
    ) -> Result<(String, Vec<Ty>), Error> {
        if self.vars.contains(n) {
            return Err(error(1, "multiple outputs cannot be taken from indexing"));
        }
        if let Some(sig) = self.functions.get(n) {
            if args.len() != sig.args.len() {
                return Err(error(
                    1,
                    format!("{n} expects {} arguments", sig.args.len()),
                ));
            }
            if count > sig.outputs.len() {
                return Err(error(
                    1,
                    format!("{n} supplies only {} outputs", sig.outputs.len()),
                ));
            }
            let outputs = sig
                .outputs
                .iter()
                .map(|o| o.ok_or_else(|| error(1, format!("unresolved output type for {n}"))))
                .collect::<Result<Vec<_>, _>>()?;
            let mut values = vec![];
            for (e, t) in args.iter().zip(&sig.args) {
                let from = self.ty(e)?;
                let code = self.expr(e, end)?;
                values.push(self.cast(code, from, *t));
            }
            let mut code = format!("f_{n}({})?", values.join(", "));
            if count == 1 && outputs.len() > 1 {
                code = format!("({code}).0")
            }
            Ok((code, outputs))
        } else {
            if !BUILTINS.contains(&n) && !["inv", "det"].contains(&n) {
                return Err(error(1, format!("unsupported function '{n}'")));
            }
            let values = self.args(args, end)?;
            if count == 0 {
                return Ok((format!("rt::call_void({n:?}, &[{values}])?"), vec![]));
            }
            let types = args
                .iter()
                .map(|e| self.ty(e))
                .collect::<Result<Vec<_>, _>>()?;
            let ty = builtin_ty(n, &types)?;
            if count == 1 {
                Ok((
                    format!("rt::call::<{}>({n:?}, &[{values}])?", ty.rust()),
                    vec![ty],
                ))
            } else {
                if !["size", "min", "max", "sort", "find"].contains(&n) {
                    return Err(error(1, format!("multiple outputs unsupported for {n}")));
                }
                Ok((
                    format!("rt::call_outputs::<ArrayD<f64>>({n:?}, &[{values}], {count})?"),
                    vec![Ty::Numbers; count],
                ))
            }
        }
    }
    fn store(&self, n: &str, code: String, from: Ty) -> Result<String, Error> {
        let to = *self
            .types
            .get(n)
            .ok_or_else(|| error(1, format!("cannot infer type of '{n}'")))?;
        let code = self.cast(code, from, to);
        Ok(format!(
            "v_{n} = {};\n",
            if self.optional.contains(n) {
                format!("Some({code})")
            } else {
                code
            }
        ))
    }
    fn returns(&self) -> String {
        let values = self
            .outputs
            .iter()
            .map(|n| self.variable(n))
            .collect::<Vec<_>>();
        format!("Ok({})", tuple(&values))
    }
    fn body(&mut self, body: &[Stmt], depth: usize) -> Result<String, Error> {
        let mut out = String::new();
        for s in body {
            match s {
                Stmt::Assign(Target::Name(n), e) => {
                    let t = self.ty(e)?;
                    let c = self.expr(e, None)?;
                    out.push_str(&self.store(n, c, t)?);
                }
                Stmt::Assign(Target::Index(n, args), e) => {
                    let ty = *self
                        .types
                        .get(n)
                        .ok_or_else(|| error(1, format!("cannot infer indexed variable '{n}'")))?;
                    let value = self.expr(e, None)?;
                    let indices = self.indices(args, "indexed")?;
                    let base = if self.optional.contains(n) {
                        format!(
                            "v_{n}.clone().unwrap_or_else(|| ArrayD::from_shape_vec(ndarray::IxDyn(&[0, 0]), vec![]).expect(\"empty array\"))"
                        )
                    } else {
                        self.variable(n)
                    };
                    out.push_str(&format!("{{ let assigned_value = {value}; let mut indexed: {} = {base}; let indices = [{indices}]; rt::assign(&mut indexed, &indices, &assigned_value)?; {} }}\n",ty.rust(),self.store(n,"indexed".into(),ty)?));
                }
                Stmt::Assign(Target::Many(names), e) if names.len() == 1 => {
                    let t = self.ty(e)?;
                    let c = self.expr(e, None)?;
                    out.push_str(&self.store(&names[0], c, t)?);
                }
                Stmt::Assign(Target::Many(names), Expr::Apply(n, args)) => {
                    self.serial += 1;
                    let id = self.serial;
                    let (code, types) = self.call(n, args, names.len(), None)?;
                    out.push_str(&format!("{{ let result_{id} = {code};\n"));
                    for (i, name) in names.iter().enumerate() {
                        let c = if self.functions.contains_key(n) {
                            if types.len() == 1 {
                                format!("result_{id}.clone()")
                            } else {
                                format!("result_{id}.{i}.clone()")
                            }
                        } else {
                            format!("result_{id}[{i}].clone()")
                        };
                        out.push_str(&self.store(name, c, types[i])?)
                    }
                    out.push_str("}\n");
                }
                Stmt::Assign(Target::Many(_), _) => {
                    return Err(error(1, "multiple assignment requires a call"));
                }
                Stmt::Call(Expr::Apply(n, args)) => {
                    let (code, _) = self.call(n, args, 0, None)?;
                    out.push_str(&format!("let _ = {code};\n"));
                }
                Stmt::Call(_) => return Err(error(1, "bare expression display is unsupported")),
                Stmt::If(branches, other) => {
                    for (i, (c, b)) in branches.iter().enumerate() {
                        let cond = self.expr(c, None)?;
                        out.push_str(&format!(
                            "{}if rt::truth(&({cond}))? {{\n{} }}",
                            if i == 0 { "" } else { " else " },
                            self.body(b, depth)?
                        ));
                    }
                    out.push_str(&format!(" else {{\n{} }}\n", self.body(other, depth)?));
                }
                Stmt::While(c, b) => {
                    let cond = self.expr(c, None)?;
                    out.push_str(&format!(
                        "while rt::truth(&({cond}))? {{\n{} }}\n",
                        self.body(b, depth + 1)?
                    ));
                }
                Stmt::For(n, e, b) => {
                    self.serial += 1;
                    let id = self.serial;
                    let from = self.ty(e)?;
                    let ty = *self
                        .types
                        .get(n)
                        .ok_or_else(|| error(1, "cannot infer loop variable type"))?;
                    let value = self.expr(e, None)?;
                    let converted = self.cast(format!("range_{id}.clone()"), from, ty);
                    out.push_str(&format!("{{ let range_{id} = {value}; {} for column in rt::columns::<{}>(&range_{id})? {{ {} {} }} }}\n",self.store(n,converted,ty)?,ty.rust(),self.store(n,"column".into(),ty)?,self.body(b,depth+1)?));
                }
                Stmt::Break | Stmt::Continue => {
                    if depth == 0 {
                        return Err(error(1, "break/continue outside loop"));
                    }
                    out.push_str(if matches!(s, Stmt::Break) {
                        "break;\n"
                    } else {
                        "continue;\n"
                    });
                }
                Stmt::Return => {
                    if !self.in_function {
                        return Err(error(1, "return outside function"));
                    }
                    out.push_str(&format!("return {};\n", self.returns()));
                }
            }
        }
        Ok(out)
    }
}
fn tuple(values: &[String]) -> String {
    match values.len() {
        0 => "()".into(),
        1 => values[0].clone(),
        _ => format!("({})", values.join(", ")),
    }
}
fn field_name(name: &str) -> String {
    // Always prefix path-reserved names; reserve the entire prefix for an injective mapping.
    if ["_", "self", "Self", "crate", "super"].contains(&name) || name.starts_with("matlab_field_")
    {
        return format!("matlab_field_{name}");
    }
    format!("r#{name}")
}
fn declarations(generator: &TypedGenerator<'_>, args: &BTreeSet<String>) -> String {
    generator
        .types
        .iter()
        .filter(|(n, _)| !args.contains(*n))
        .map(|(n, t)| {
            if generator.optional.contains(n) {
                format!("let mut v_{n}: Option<{}> = None;\n", t.rust())
            } else {
                format!("let mut v_{n}: {};\n", t.rust())
            }
        })
        .collect()
}
/// Generate compact typed Rust using the separately packaged ndarray-based runtime.
/// Undeclared function parameters use `ArrayD<f64>`; unsupported type joins are diagnostics.
pub fn transpile_typed(source: &str, library: bool) -> Result<String, Error> {
    let mut parser = Parser {
        tokens: lex(source)?,
        pos: 0,
        depth: 0,
    };
    if parser
        .tokens
        .iter()
        .any(|s| matches!(&s.token,Token::Name(n) if n=="arguments"))
    {
        return Err(error(
            1,
            "MATLAB arguments blocks are not yet supported by typed code generation",
        ));
    }
    let script = parser.body()?;
    let mut functions = vec![];
    while parser.named("function") {
        if functions.len() >= 64 {
            return Err(error(1, "typed source exceeds 64-function limit"));
        }
        let function = parser.function()?;
        if function.args.len() > 256 || function.outputs.len() > 256 {
            return Err(error(1, "typed function exceeds 256-argument/output limit"));
        }
        functions.push(function);
    }
    if !matches!(parser.token(), Token::Eof) {
        return Err(parser.fail("unexpected token after definitions"));
    }
    if library && (!script.is_empty() || functions.is_empty()) {
        return Err(error(
            1,
            "library mode requires functions and no script statements",
        ));
    }
    let mut inference_budget = 2_000_000usize;
    let mut signatures = BTreeMap::new();
    for f in &functions {
        if BUILTINS.contains(&f.name.as_str()) || ["inv", "det"].contains(&f.name.as_str()) {
            return Err(error(1, "built-in function shadowing is unsupported"));
        }
        if signatures
            .insert(
                f.name.clone(),
                Signature {
                    args: vec![Ty::Numbers; f.args.len()],
                    outputs: vec![None; f.outputs.len()],
                },
            )
            .is_some()
        {
            return Err(error(1, "duplicate function definition"));
        }
    }
    for _ in 0..functions.len().saturating_mul(4).saturating_add(1) {
        let mut changed = false;
        let mut evidence = BTreeMap::new();
        let (script_types, script_vars) =
            scope_types(&script, &[], &signatures, &mut inference_budget)?;
        call_evidence(
            &script,
            &script_types,
            &script_vars,
            &signatures,
            &mut evidence,
        )?;
        // Apply direct script-call evidence before checking function assignments;
        // otherwise a legitimate logical/character parameter is mistaken for fallback numeric.
        for (name, observed) in &evidence {
            let sig = signatures.get_mut(name).unwrap();
            for (parameter, observed) in sig.args.iter_mut().zip(observed) {
                if let Some(t) = observed
                    && *parameter != *t
                {
                    *parameter = *t;
                    changed = true;
                }
            }
        }
        for f in &functions {
            let args = f
                .args
                .iter()
                .cloned()
                .zip(signatures[&f.name].args.iter().copied())
                .collect::<Vec<_>>();
            let (types, vars) = scope_types(&f.body, &args, &signatures, &mut inference_budget)?;
            call_evidence(&f.body, &types, &vars, &signatures, &mut evidence)?;
        }
        for (name, observed) in evidence {
            let sig = signatures.get_mut(&name).unwrap();
            for (parameter, observed) in sig.args.iter_mut().zip(observed) {
                if let Some(t) = observed
                    && *parameter != t
                {
                    *parameter = t;
                    changed = true;
                }
            }
        }
        for f in &functions {
            let args = f
                .args
                .iter()
                .cloned()
                .zip(signatures[&f.name].args.iter().copied())
                .collect::<Vec<_>>();
            let (types, _) = scope_types(&f.body, &args, &signatures, &mut inference_budget)?;
            let sig = signatures.get_mut(&f.name).unwrap();
            for (n, t) in f.outputs.iter().zip(&mut sig.outputs) {
                if let Some(next) = types.get(n) {
                    let next = *next;
                    if *t != Some(next) {
                        *t = Some(next);
                        changed = true
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
    let mut out = String::from(
        "// Generated typed Rust. MATLAB indexing remains one-based in runtime helpers.\n#![allow(dead_code, unused_imports, unused_variables, unused_mut, unused_assignments, non_snake_case, unreachable_code, unused_parens)]\nuse ndarray::ArrayD;\nuse unlinked_matlab_rt as rt;\n",
    );
    for f in &functions {
        let sig = &signatures[&f.name];
        let args = f
            .args
            .iter()
            .cloned()
            .zip(sig.args.iter().copied())
            .collect::<Vec<_>>();
        let (types, vars) = scope_types(&f.body, &args, &signatures, &mut inference_budget)?;
        for n in &vars {
            if !types.contains_key(n) {
                return Err(error(1, format!("cannot infer type of '{n}'")));
            }
        }
        let mut optional = BTreeSet::new();
        let argnames = f.args.iter().cloned().collect();
        let final_defs = definite(&f.body, &vars, argnames, &mut optional, &f.outputs);
        for n in &f.outputs {
            if !final_defs.contains(n) {
                optional.insert(n.clone());
            }
        }
        for n in &f.outputs {
            if !types.contains_key(n) {
                return Err(error(1, format!("function output '{n}' is never assigned")));
            }
        }
        let mut generator = TypedGenerator {
            types,
            vars,
            optional,
            functions: &signatures,
            outputs: f.outputs.clone(),
            serial: 0,
            in_function: true,
        };
        let params = args
            .iter()
            .map(|(n, t)| format!("mut v_{n}: {}", t.rust()))
            .collect::<Vec<_>>()
            .join(", ");
        let output_types = f
            .outputs
            .iter()
            .map(|n| generator.types[n].rust().to_string())
            .collect::<Vec<_>>();
        out.push_str(&format!(
            "pub fn f_{}({params}) -> Result<{}, String> {{\n",
            f.name,
            tuple(&output_types)
        ));
        // Argument assignments can promote types; preserve incoming ABI and convert once.
        for (n, t) in &args {
            if generator.types[n] != *t {
                out.push_str(&format!(
                    "let mut v_{n}: {} = rt::convert(&v_{n})?;\n",
                    generator.types[n].rust()
                ));
            }
        }
        out.push_str(&declarations(&generator, &f.args.iter().cloned().collect()));
        out.push_str(&generator.body(&f.body, 0)?);
        out.push_str(&format!("{}\n}}\n", generator.returns()));
    }
    if !library {
        let (types, vars) = scope_types(&script, &[], &signatures, &mut inference_budget)?;
        for n in &vars {
            if !types.contains_key(n) {
                return Err(error(1, format!("cannot infer type of '{n}'")));
            }
        }
        let mut optional = BTreeSet::new();
        let defined = definite(&script, &vars, BTreeSet::new(), &mut optional, &[]);
        for n in &vars {
            if !defined.contains(n) {
                optional.insert(n.clone());
            }
        }
        let mut generator = TypedGenerator {
            types,
            vars,
            optional,
            functions: &signatures,
            outputs: vec![],
            serial: 0,
            in_function: false,
        };
        out.push_str("#[derive(Debug)]\npub struct ScriptOutput {\n");
        for (n, t) in &generator.types {
            out.push_str(&format!(
                "pub {}: {},\n",
                field_name(n),
                if defined.contains(n) {
                    t.rust().into()
                } else {
                    format!("Option<{}>", t.rust())
                }
            ));
        }
        out.push_str("}\npub fn run_script() -> Result<ScriptOutput, String> {\n");
        out.push_str(&declarations(&generator, &BTreeSet::new()));
        out.push_str(&generator.body(&script, 0)?);
        out.push_str("Ok(ScriptOutput {\n");
        for n in generator.types.keys() {
            let value = if defined.contains(n) {
                generator.variable(n)
            } else {
                format!("v_{n}")
            };
            out.push_str(&format!("{}: {value},\n", field_name(n)));
        }
        out.push_str("})\n}\nfn main() { if let Err(error) = run_script() { eprintln!(\"MATLAB runtime error: {error}\"); std::process::exit(1); } }\n");
    }
    if out.len() > 4 * 1024 * 1024 {
        return Err(error(1, "generated typed Rust exceeds 4 MiB limit"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::transpile_typed;
    #[test]
    fn scalar_locals_are_typed_and_only_uncertain_variables_are_optional() {
        let code = transpile_typed("x=2; y=x*x+3; if y>5; z=4; end; disp(z);", false).unwrap();
        assert!(code.contains("let mut v_x: f64;"));
        assert!(code.contains("let mut v_y: f64;"));
        assert!(code.contains("let mut v_z: Option<f64> = None;"));
        assert!(!code.contains("Environment"));
        assert!(!code.contains("Value"));
        assert!(!code.contains("tick"));
    }
    #[test]
    fn joins_and_logical_indices_preserve_element_kind() {
        assert!(
            transpile_typed("x=1; x='abc';", false)
                .unwrap_err()
                .message
                .contains("character")
        );
        assert!(
            transpile_typed("mask=false; mask=0;", false)
                .unwrap_err()
                .message
                .contains("logical")
        );
        let code = transpile_typed("a=[1 2]; b=a(false);", false).unwrap();
        assert!(code.contains("let mut v_b: ArrayD<f64>;"));
        let code = transpile_typed("x=1; x(2)=true;", false).unwrap();
        assert!(code.contains("let mut v_x: ArrayD<f64>;"));
    }
    #[test]
    fn char_calls_specialize_while_unknown_inputs_remain_numeric_arrays() {
        let code =
            transpile_typed("y=f('abc'); function y=f(x); y=strcmp(x,'abc'); end", false).unwrap();
        assert!(code.contains("f_f(mut v_x: String) -> Result<bool, String>"));
        let code = transpile_typed("function y=f(x); y=x*2; end", true).unwrap();
        assert!(code.contains("v_x: ArrayD<f64>"));
    }
    #[test]
    fn one_output_brackets_and_rust_reserved_names_emit_valid_forms() {
        let code = transpile_typed(
            "[a]=zeros(1,2); [b]=sort([2 1]); [c]=f(); _=3; function [x,y]=f(); x=1;y=2;end",
            false,
        )
        .unwrap();
        assert!(code.contains("let mut v_a: ArrayD<f64>;"));
        assert!(code.contains("v_c = (f_f()?).0;"));
        assert!(code.contains("pub matlab_field__: f64,"));
        assert!(!code.contains("result_1.0"));
        let code =
            transpile_typed("disp(f(true)); function y=f(x); x=false;y=x;end", false).unwrap();
        assert!(code.contains("v_x: ArrayD<bool>"));
    }
    #[test]
    fn recursive_types_and_inference_work_have_bounded_diagnostics() {
        assert!(
            transpile_typed("function y=f(x); y=f(x); end", true)
                .unwrap_err()
                .message
                .contains("infer")
        );
        let many = (0..65)
            .map(|i| format!("function y=f{i}(); y=1; end\n"))
            .collect::<String>();
        assert!(
            transpile_typed(&many, true)
                .unwrap_err()
                .message
                .contains("64-function")
        );
        let mut reverse = (0..900)
            .map(|i| format!("a{i}=a{};\n", i + 1))
            .collect::<String>();
        reverse.push_str("a900=1;");
        assert!(
            transpile_typed(&reverse, false)
                .unwrap_err()
                .message
                .contains("budget")
        );
    }
}
