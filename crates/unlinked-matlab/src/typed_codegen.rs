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
    declared: Vec<bool>,
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
fn flattened_reduction(name: &str, args: &[Expr], vars: &BTreeSet<String>) -> Option<Ty> {
    if args.len() == 1
        && matches!(&args[0],Expr::Apply(n,indices) if vars.contains(n)&&matches!(indices.as_slice(),[Expr::All]))
    {
        match name {
            "sum" | "prod" => Some(Ty::Number),
            "all" | "any" => Some(Ty::Bool),
            _ => None,
        }
    } else {
        None
    }
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
            if let Some(ty) = flattened_reduction(n, args, vars) {
                return Ok(Some(ty));
            }
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
    1 + match &s.kind {
        StmtKind::Assign(Target::Index(_, args), e) => {
            expression_work(e) + args.iter().map(expression_work).sum::<usize>()
        }
        StmtKind::Assign(_, e)
        | StmtKind::Call(e)
        | StmtKind::For(_, e, _)
        | StmtKind::While(e, _) => expression_work(e),
        StmtKind::If(branches, _) => branches.iter().map(|(e, _)| expression_work(e)).sum(),
        _ => 0,
    }
}
fn loop_types(body: &[Stmt], outputs: &[String]) -> (BTreeSet<String>, BTreeSet<String>) {
    fn number(e: &Expr) -> Option<f64> {
        match e {
            Expr::Number(n) => Some(*n),
            Expr::Unary(op, e) if op == "-" => Some(-number(e)?),
            _ => None,
        }
    }
    fn gather(body: &[Stmt], candidates: &mut BTreeMap<String, bool>) {
        for s in body {
            match &s.kind {
                StmtKind::For(n, Expr::Range(a, b, c), body) => {
                    let nonempty = match (number(a), number(b), number(c)) {
                        (Some(a), Some(b), Some(c)) => {
                            b != 0.0 && ((b > 0.0 && a <= c) || (b < 0.0 && a >= c))
                        }
                        _ => false,
                    };
                    candidates
                        .entry(n.clone())
                        .and_modify(|old| *old &= nonempty)
                        .or_insert(nonempty);
                    gather(body, candidates);
                }
                StmtKind::For(n, _, body) => {
                    candidates.insert(n.clone(), false);
                    gather(body, candidates);
                }
                StmtKind::While(_, body) => gather(body, candidates),
                StmtKind::If(branches, other) => {
                    for (_, b) in branches {
                        gather(b, candidates);
                    }
                    gather(other, candidates);
                }
                _ => {}
            }
        }
    }
    fn reads_name(e: &Expr, n: &str) -> bool {
        match e {
            Expr::Var(name) => name == n,
            Expr::Apply(name, args) => name == n || args.iter().any(|e| reads_name(e, n)),
            Expr::Unary(_, a) => reads_name(a, n),
            Expr::Binary(_, a, b) => reads_name(a, n) || reads_name(b, n),
            Expr::Range(a, b, c) => reads_name(a, n) || reads_name(b, n) || reads_name(c, n),
            Expr::Array(rows) => rows.iter().flatten().any(|e| reads_name(e, n)),
            _ => false,
        }
    }
    fn escapes(body: &[Stmt], n: &str) -> bool {
        body.iter().any(|s| match &s.kind {
            StmtKind::For(name, range, _) if name == n => reads_name(range, n),
            StmtKind::For(_, e, b) | StmtKind::While(e, b) => reads_name(e, n) || escapes(b, n),
            StmtKind::Assign(t, e) => {
                reads_name(e, n)
                    || match t {
                        Target::Name(name) => name == n,
                        Target::Index(name, args) => {
                            name == n || args.iter().any(|e| reads_name(e, n))
                        }
                        Target::Many(names) => names.iter().any(|name| name == n),
                    }
            }
            StmtKind::Call(e) => reads_name(e, n),
            StmtKind::If(branches, other) => {
                branches
                    .iter()
                    .any(|(e, b)| reads_name(e, n) || escapes(b, n))
                    || escapes(other, n)
            }
            _ => false,
        })
    }
    fn nonrange_loops(body: &[Stmt], names: &mut BTreeSet<String>) {
        for s in body {
            match &s.kind {
                StmtKind::For(n, e, b) => {
                    if !matches!(e, Expr::Range(..)) {
                        names.insert(n.clone());
                    }
                    nonrange_loops(b, names);
                }
                StmtKind::While(_, b) => nonrange_loops(b, names),
                StmtKind::If(branches, other) => {
                    for (_, b) in branches {
                        nonrange_loops(b, names);
                    }
                    nonrange_loops(other, names);
                }
                _ => {}
            }
        }
    }
    let mut nonrange = BTreeSet::new();
    nonrange_loops(body, &mut nonrange);
    let mut candidates = BTreeMap::new();
    gather(body, &mut candidates);
    let mut scalar = BTreeSet::new();
    let mut hidden = BTreeSet::new();
    for (n, nonempty) in candidates {
        if nonrange.contains(&n) {
            continue;
        }
        let local = !outputs.contains(&n) && !escapes(body, &n);
        if nonempty || local {
            scalar.insert(n.clone());
        }
        if local {
            hidden.insert(n);
        }
    }
    (scalar, hidden)
}
fn at_statement(mut error: Error, line: usize) -> Error {
    if error.line == 1 {
        error.line = line;
    }
    error
}
fn infer_body(
    body: &[Stmt],
    types: &mut Types,
    vars: &BTreeSet<String>,
    functions: &BTreeMap<String, Signature>,
    budget: &mut usize,
    scalar_loops: &BTreeSet<String>,
) -> Result<(), Error> {
    for s in body {
        *budget = budget
            .checked_sub(statement_work(s))
            .ok_or_else(|| error(1, "typed inference work budget exceeded"))?;
        let result = (|| -> Result<(), Error> {
            match &s.kind {
                StmtKind::Assign(Target::Name(n), e) => {
                    if let Some(t) = expr_ty(e, types, vars, functions)? {
                        assign_type(types, n, t)?
                    }
                }
                StmtKind::Assign(Target::Index(n, _), e) => {
                    if let Some(t) = expr_ty(e, types, vars, functions)? {
                        let target = types.get(n).copied().unwrap_or(t).array();
                        assign_type(types, n, target)?
                    }
                }
                StmtKind::Assign(Target::Many(names), e) if names.len() == 1 => {
                    if let Some(t) = expr_ty(e, types, vars, functions)? {
                        assign_type(types, &names[0], t)?;
                    }
                }
                StmtKind::Assign(Target::Many(names), Expr::Apply(n, args)) => {
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
                            return Ok(());
                        };
                        if n != "size" || names.len() != 2 {
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
                StmtKind::Assign(Target::Many(_), _) => {
                    return Err(error(1, "multiple assignment requires a function call"));
                }
                StmtKind::For(n, e, b) => {
                    if let Some(t) = expr_ty(e, types, vars, functions)? {
                        assign_type(
                            types,
                            n,
                            if matches!(e, Expr::Range(..)) && scalar_loops.contains(n) {
                                Ty::Number
                            } else {
                                t.array()
                            },
                        )?
                    }
                    infer_body(b, types, vars, functions, budget, scalar_loops)?
                }
                StmtKind::While(_, b) => {
                    infer_body(b, types, vars, functions, budget, scalar_loops)?
                }
                StmtKind::If(branches, other) => {
                    for (_, b) in branches {
                        infer_body(b, types, vars, functions, budget, scalar_loops)?
                    }
                    infer_body(other, types, vars, functions, budget, scalar_loops)?
                }
                _ => {}
            }
            Ok(())
        })();
        result.map_err(|error| at_statement(error, s.line))?;
    }
    Ok(())
}
fn scope_types(
    body: &[Stmt],
    args: &[(String, Ty)],
    functions: &BTreeMap<String, Signature>,
    budget: &mut usize,
    outputs: &[String],
) -> Result<(Types, BTreeSet<String>), Error> {
    let (scalar_loops, _) = loop_types(body, outputs);
    let mut vars = args.iter().map(|(n, _)| n.clone()).collect();
    assigned(body, &mut vars);
    let mut types: Types = args.iter().cloned().collect();
    for _ in 0..(vars.len().saturating_mul(3) + 1).min(50_000) {
        let before = types.clone();
        infer_body(body, &mut types, &vars, functions, budget, &scalar_loops)?;
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
        let result = (|| -> Result<(), Error> {
            match &s.kind {
                StmtKind::Assign(target, e) => {
                    call_evidence_expr(e, types, vars, functions, evidence)?;
                    if let Target::Index(_, args) = target {
                        for e in args {
                            call_evidence_expr(e, types, vars, functions, evidence)?;
                        }
                    }
                }
                StmtKind::Call(e) => call_evidence_expr(e, types, vars, functions, evidence)?,
                StmtKind::If(branches, other) => {
                    for (c, b) in branches {
                        call_evidence_expr(c, types, vars, functions, evidence)?;
                        call_evidence(b, types, vars, functions, evidence)?;
                    }
                    call_evidence(other, types, vars, functions, evidence)?;
                }
                StmtKind::While(e, b) | StmtKind::For(_, e, b) => {
                    call_evidence_expr(e, types, vars, functions, evidence)?;
                    call_evidence(b, types, vars, functions, evidence)?;
                }
                _ => {}
            }
            Ok(())
        })();
        result.map_err(|error| at_statement(error, s.line))?;
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
        match &s.kind {
            StmtKind::Assign(target, e) => {
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
            StmtKind::Call(e) => reads(e, vars, &defined, optional),
            StmtKind::If(branches, other) => {
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
            StmtKind::While(e, b) => {
                reads(e, vars, &defined, optional);
                definite(b, vars, defined.clone(), optional, outputs);
            }
            StmtKind::For(n, e, b) => {
                reads(e, vars, &defined, optional);
                defined.insert(n.clone());
                definite(b, vars, defined.clone(), optional, outputs);
            }
            StmtKind::Return => {
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
    borrowed: BTreeSet<String>,
    mutable: BTreeSet<String>,
    hidden_loops: BTreeSet<String>,
    inline: BTreeSet<String>,
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
        let scalar = self.types[n].scalar();
        if self.optional.contains(n) {
            let reference = format!("v_{n}.as_ref().ok_or_else(|| rt::Error::undefined({n:?}))?");
            if scalar {
                format!("*({reference})")
            } else {
                format!("({reference}).clone()")
            }
        } else if scalar {
            format!("v_{n}")
        } else if self.borrowed.contains(n) {
            format!("(*v_{n}).clone()")
        } else {
            format!("v_{n}.clone()")
        }
    }
    fn borrow_variable(&self, n: &str) -> String {
        if self.optional.contains(n) {
            format!("v_{n}.as_ref().ok_or_else(|| rt::Error::undefined({n:?}))?")
        } else if self.borrowed.contains(n) {
            format!("v_{n}")
        } else {
            format!("&v_{n}")
        }
    }
    fn move_variable(&self, n: &str) -> String {
        if self.optional.contains(n) {
            format!("v_{n}.ok_or_else(|| rt::Error::undefined({n:?}))?")
        } else if self.borrowed.contains(n) {
            format!("(*v_{n}).clone()")
        } else {
            format!("v_{n}")
        }
    }
    fn borrow(&mut self, e: &Expr, end: Option<&str>) -> Result<String, Error> {
        if let Expr::Var(n) = e
            && self.vars.contains(n)
        {
            Ok(self.borrow_variable(n))
        } else {
            Ok(format!("&({})", self.expr(e, end)?))
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
        if args.len() > 32 {
            return Err(error(1, "built-in calls support at most 32 arguments"));
        }
        args.iter()
            .map(|e| self.borrow(e, end))
            .collect::<Result<Vec<_>, _>>()
            .map(|v| argument_tuple(&v))
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
                    "-" if a_ty.scalar() => format!("-({})", self.cast(v, a_ty, Ty::Number)),
                    "~" if a_ty == Ty::Bool => format!("!({v})"),
                    "~" if a_ty.scalar() => format!("!rt::scalar_truth(&({v}))?"),
                    "'" | ".'" if a_ty.scalar() => v,
                    _ => {
                        let name = match op.as_str() {
                            "+" => "positive",
                            "-" => "negative",
                            "~" => "not",
                            "'" | ".'" => "transpose",
                            _ => return Err(error(1, "unsupported unary operator")),
                        };
                        format!("rt::{name}::<{}>({})?", ty.rust(), self.borrow(a, end)?)
                    }
                }
            }
            Expr::Binary(op, a, b) => {
                let (a_ty, b_ty) = (self.ty(a)?, self.ty(b)?);
                let (a, b) = (self.expr(a, end)?, self.expr(b, end)?);
                if op == "&&" || op == "||" {
                    {
                        let a = if a_ty == Ty::Bool {
                            a
                        } else {
                            format!("rt::scalar_truth(&({a}))?")
                        };
                        let b = if b_ty == Ty::Bool {
                            b
                        } else {
                            format!("rt::scalar_truth(&({b}))?")
                        };
                        format!("({a}) {op} ({b})")
                    }
                } else if ["==", "~=", "<", "<=", ">", ">="].contains(&op.as_str())
                    && matches!(e, Expr::Binary(_,left,right) if literal_nan(left)||literal_nan(right))
                {
                    format!("rt::{}::<{}>(&({a}), &({b}))?", binary_name(op)?, ty.rust())
                } else if a_ty.scalar() && b_ty.scalar() {
                    let (a, b) = (
                        self.cast(a, a_ty, Ty::Number),
                        self.cast(b, b_ty, Ty::Number),
                    );
                    match op.as_str() {
                        "+" | "-" | "*" | "/" | "<" | "<=" | ">" | ">=" | "==" => {
                            format!("({a}) {op} ({b})")
                        }
                        ".*" => format!("({a}) * ({b})"),
                        "./" => format!("({a}) / ({b})"),
                        "\\" | ".\\" => format!("({b}) / ({a})"),
                        "^" | ".^" => {
                            let exponent = match e {
                                Expr::Binary(_, _, right) => integer_literal(right),
                                _ => None,
                            };
                            if let Some(exponent) = exponent {
                                format!("({a}).powi({exponent})")
                            } else {
                                format!("rt::power::<f64>(&({a}), &({b}))?")
                            }
                        }
                        "~=" => format!("({a}) != ({b})"),
                        "&" | "|" => format!("rt::{}::<bool>(&({a}), &({b}))?", binary_name(op)?),
                        _ => return Err(error(1, format!("unsupported scalar operator {op}"))),
                    }
                } else {
                    format!(
                        "rt::{}::<{}>({}, {})?",
                        binary_name(op)?,
                        ty.rust(),
                        self.borrow(
                            match e {
                                Expr::Binary(_, a, _) => a,
                                _ => unreachable!(),
                            },
                            end
                        )?,
                        self.borrow(
                            match e {
                                Expr::Binary(_, _, b) => b,
                                _ => unreachable!(),
                            },
                            end
                        )?
                    )
                }
            }
            Expr::Range(a, b, c) => format!(
                "rt::range({}, {}, {})?",
                self.scalar(a, end)?,
                self.scalar(b, end)?,
                self.scalar(c, end)?
            ),
            Expr::Array(rows) => {
                if rows.len() > 32 {
                    return Err(error(
                        1,
                        "typed array literals support at most 32 rows per concatenation",
                    ));
                }
                let rows = rows
                    .iter()
                    .map(|row| self.args(row, end))
                    .collect::<Result<Vec<_>, _>>()?;
                format!("rt::concat::<{}>({})?", ty.rust(), argument_tuple(&rows))
            }
            Expr::Apply(n, args) if self.vars.contains(n) => {
                let base = self.borrow_variable(n);
                let indices = self.indices(args, "_indexed")?;
                if args.iter().all(|arg| matches!(arg, Expr::All)) {
                    format!("rt::index::<{}>({base}, &[{indices}])?", ty.rust())
                } else {
                    format!(
                        "{{ let _indexed = {base}; rt::index::<{}>(_indexed, &[{indices}])? }}",
                        ty.rust()
                    )
                }
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
                        self.expr(e, Some(&format!("rt::end({base}, {i}, {})?", args.len())))?
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
                if !t.scalar() && from == *t {
                    values.push(self.borrow(e, end)?);
                } else {
                    let code = self.expr(e, end)?;
                    let converted = self.cast(code, from, *t);
                    values.push(if t.scalar() {
                        converted
                    } else {
                        format!("&({converted})")
                    });
                }
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
            if count == 0 && ["disp", "assert", "error"].contains(&n) {
                return Ok((format!("rt::{n}({values})?"), vec![]));
            }
            if count == 0 && n == "fprintf" {
                return Ok((format!("rt::fprintf_void({values})?"), vec![]));
            }
            let types = args
                .iter()
                .map(|e| self.ty(e))
                .collect::<Result<Vec<_>, _>>()?;
            let ty = flattened_reduction(n, args, &self.vars)
                .map(Ok)
                .unwrap_or_else(|| builtin_ty(n, &types))?;
            if count <= 1 {
                if n == "transpose" {
                    if args.len() != 1 {
                        return Err(error(1, "transpose expects one argument"));
                    }
                    return Ok((
                        format!(
                            "rt::transpose::<{}>({})?",
                            ty.rust(),
                            self.borrow(&args[0], end)?
                        ),
                        vec![ty],
                    ));
                }
                let name = if n == "mod" { "modulo" } else { n };
                Ok((format!("rt::{name}::<{}>({values})?", ty.rust()), vec![ty]))
            } else {
                if n != "size" || count != 2 {
                    return Err(error(1, format!("multiple outputs unsupported for {n}")));
                }
                Ok((
                    format!("rt::{n}_outputs::<ArrayD<f64>>({values}, {count})?"),
                    vec![Ty::Numbers; count],
                ))
            }
        }
    }
    fn store(&mut self, n: &str, code: String, from: Ty) -> Result<String, Error> {
        let to = *self
            .types
            .get(n)
            .ok_or_else(|| error(1, format!("cannot infer type of '{n}'")))?;
        let code = self.cast(code, from, to);
        if self.inline.remove(n) {
            return Ok(format!(
                "let {}v_{n}: {} = {code};\n",
                if self.mutable.contains(n) { "mut " } else { "" },
                to.rust()
            ));
        }
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
            .map(|n| self.move_variable(n))
            .collect::<Vec<_>>();
        format!("Ok({})", tuple(&values))
    }
    fn body(&mut self, body: &[Stmt], depth: usize) -> Result<String, Error> {
        let mut out = String::new();
        for s in body {
            let result = (|| -> Result<(), Error> {
                match &s.kind {
                    StmtKind::Assign(Target::Name(n), e) => {
                        let t = self.ty(e)?;
                        let c = self.expr(e, None)?;
                        out.push_str(&self.store(n, c, t)?);
                    }
                    StmtKind::Assign(Target::Index(n, args), e) => {
                        let ty = self.types[n];
                        let value = self.expr(e, None)?;
                        let indices = self.indices(args, "_indexed")?;
                        out.push_str(&format!("{{ let assigned_value = {value};\n"));
                        if self.optional.contains(n) {
                            out.push_str(&format!("let empty: {} = ArrayD::from_shape_vec(ndarray::IxDyn(&[0,0]), vec![]).expect(\"empty array\");\nlet indices = {{ let _indexed = v_{n}.as_ref().unwrap_or(&empty); [{indices}] }};\nif v_{n}.is_none() {{ v_{n}=Some(empty); }}\nrt::assign(v_{n}.as_mut().expect(\"initialized array\"), &indices, &assigned_value)?;\n",ty.rust()));
                        } else {
                            out.push_str(&format!("let indices = {{ let _indexed = &v_{n}; [{indices}] }};\nrt::assign(&mut v_{n}, &indices, &assigned_value)?;\n"));
                        }
                        out.push_str("}\n");
                    }
                    StmtKind::Assign(Target::Many(names), e) if names.len() == 1 => {
                        let t = self.ty(e)?;
                        let c = self.expr(e, None)?;
                        out.push_str(&self.store(&names[0], c, t)?);
                    }
                    StmtKind::Assign(Target::Many(names), Expr::Apply(n, args)) => {
                        self.serial += 1;
                        let id = self.serial;
                        let (code, types) = self.call(n, args, names.len(), None)?;
                        out.push_str(&format!("{{ let result_{id} = {code};\n"));
                        for (i, name) in names.iter().enumerate() {
                            let c = if self.functions.contains_key(n) {
                                if types.len() == 1 {
                                    format!("result_{id}")
                                } else {
                                    format!("result_{id}.{i}")
                                }
                            } else {
                                format!("result_{id}[{i}].clone()")
                            };
                            out.push_str(&self.store(name, c, types[i])?)
                        }
                        out.push_str("}\n");
                    }
                    StmtKind::Assign(Target::Many(_), _) => {
                        return Err(error(1, "multiple assignment requires a call"));
                    }
                    StmtKind::Call(Expr::Apply(n, args)) => {
                        let (code, outputs) = self.call(n, args, 0, None)?;
                        out.push_str(&format!(
                            "{}{code};\n",
                            if outputs.is_empty() { "" } else { "let _ = " }
                        ));
                    }
                    StmtKind::Call(_) => {
                        return Err(error(1, "bare expression display is unsupported"));
                    }
                    StmtKind::If(branches, other) => {
                        for (i, (c, b)) in branches.iter().enumerate() {
                            let cond = if self.ty(c)? == Ty::Bool {
                                self.expr(c, None)?
                            } else {
                                format!("rt::truth({})?", self.borrow(c, None)?)
                            };
                            out.push_str(&format!(
                                "{}if {cond} {{\n{} }}",
                                if i == 0 { "" } else { " else " },
                                self.body(b, depth)?
                            ));
                        }
                        if !other.is_empty() {
                            out.push_str(&format!(" else {{\n{} }}", self.body(other, depth)?));
                        }
                        out.push('\n');
                    }
                    StmtKind::While(c, b) => {
                        let cond = self.borrow(c, None)?;
                        out.push_str(&format!(
                            "while rt::truth({cond})? {{\n{} }}\n",
                            self.body(b, depth + 1)?
                        ));
                    }
                    StmtKind::For(n, e, b) => {
                        self.serial += 1;
                        let id = self.serial;
                        let ty = self.types[n];
                        if let Expr::Range(start, step, stop) = e {
                            let start = self.scalar(start, None)?;
                            let step = self.scalar(step, None)?;
                            let stop = self.scalar(stop, None)?;
                            out.push_str(&format!("{{ let range_start_{id}={start}; let values_{id}=rt::range_iter(range_start_{id}, {step}, {stop})?;\n"));
                            if !self.hidden_loops.contains(n) {
                                let initial = if ty.scalar() {
                                    format!("range_start_{id}")
                                } else {
                                    "rt::array(1, 0, vec![])?".to_string()
                                };
                                out.push_str(&self.store(n, initial, ty)?);
                            }
                            out.push_str(&format!("for range_value_{id} in values_{id} {{\n"));
                            if self.hidden_loops.contains(n) {
                                let mut writes = BTreeSet::new();
                                assigned(b, &mut writes);
                                let value = self.cast(format!("range_value_{id}"), Ty::Number, ty);
                                out.push_str(&format!(
                                    "let {}v_{n}: {} = {value};\n",
                                    if writes.contains(n) { "mut " } else { "" },
                                    ty.rust()
                                ));
                            } else {
                                out.push_str(&self.store(
                                    n,
                                    format!("range_value_{id}"),
                                    Ty::Number,
                                )?);
                            }
                            out.push_str(&self.body(b, depth + 1)?);
                            out.push_str("} }\n");
                        } else {
                            let from = self.ty(e)?;
                            let value = self.expr(e, None)?;
                            let converted = self.cast(format!("range_{id}.clone()"), from, ty);
                            out.push_str(&format!("{{ let range_{id}={value}; {} for column in rt::columns::<{}>(&range_{id})? {{ {} {} }} }}\n",self.store(n,converted,ty)?,ty.rust(),self.store(n,"column".into(),ty)?,self.body(b,depth+1)?));
                        }
                    }
                    StmtKind::Break | StmtKind::Continue => {
                        if depth == 0 {
                            return Err(error(1, "break/continue outside loop"));
                        }
                        out.push_str(if matches!(&s.kind, StmtKind::Break) {
                            "break;\n"
                        } else {
                            "continue;\n"
                        });
                    }
                    StmtKind::Return => {
                        if !self.in_function {
                            return Err(error(1, "return outside function"));
                        }
                        out.push_str(&format!("return {};\n", self.returns()));
                    }
                }
                Ok(())
            })();
            result.map_err(|error| at_statement(error, s.line))?;
        }
        Ok(out)
    }
}
fn literal_nan(e: &Expr) -> bool {
    matches!(e,Expr::Var(n) if n=="NaN"||n=="nan")
}
fn integer_literal(e: &Expr) -> Option<i32> {
    let n = match e {
        Expr::Number(n) => *n,
        Expr::Unary(op, n) if op == "-" => -f64::from(integer_literal(n)?),
        _ => return None,
    };
    (n.is_finite() && n.fract() == 0.0 && n >= i32::MIN as f64 && n <= i32::MAX as f64)
        .then_some(n as i32)
}
fn binary_name(op: &str) -> Result<&'static str, Error> {
    Ok(match op {
        "+" => "add",
        "-" => "subtract",
        ".*" => "times",
        "./" => "rdivide",
        ".\\" => "ldivide",
        ".^" => "power",
        "*" => "mtimes",
        "/" => "mrdivide",
        "\\" => "mldivide",
        "^" => "mpower",
        "==" => "eq",
        "~=" => "ne",
        "<" => "lt",
        "<=" => "le",
        ">" => "gt",
        ">=" => "ge",
        "&" => "and",
        "|" => "or",
        _ => return Err(error(1, "unsupported binary operator")),
    })
}
fn argument_tuple(values: &[String]) -> String {
    if values.is_empty() {
        "()".into()
    } else {
        format!("({},)", values.join(", "))
    }
}
fn assignment_counts(body: &[Stmt], counts: &mut BTreeMap<String, usize>, in_loop: bool) {
    for s in body {
        match &s.kind {
            StmtKind::Assign(t, _) => {
                let names = match t {
                    Target::Name(n) | Target::Index(n, _) => vec![n],
                    Target::Many(ns) => ns.iter().collect(),
                };
                for n in names {
                    *counts.entry(n.clone()).or_default() += if in_loop { 2 } else { 1 };
                }
            }
            StmtKind::For(n, _, b) => {
                *counts.entry(n.clone()).or_default() += 2;
                assignment_counts(b, counts, true);
            }
            StmtKind::While(_, b) => assignment_counts(b, counts, true),
            StmtKind::If(branches, other) => {
                let mut maximum = BTreeMap::<String, usize>::new();
                for b in branches
                    .iter()
                    .map(|(_, b)| b)
                    .chain(std::iter::once(other))
                {
                    let mut branch = BTreeMap::new();
                    assignment_counts(b, &mut branch, in_loop);
                    for (name, count) in branch {
                        maximum
                            .entry(name)
                            .and_modify(|old| *old = (*old).max(count))
                            .or_insert(count);
                    }
                }
                for (name, count) in maximum {
                    *counts.entry(name).or_default() += count;
                }
            }
            _ => {}
        }
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
fn inline_names(body: &[Stmt], args: &[String], optional: &BTreeSet<String>) -> BTreeSet<String> {
    let mut seen: BTreeSet<_> = args.iter().cloned().collect();
    let mut inline = BTreeSet::new();
    for s in body {
        match &s.kind {
            StmtKind::Assign(Target::Name(n), _) => {
                if !seen.contains(n) && !optional.contains(n) {
                    inline.insert(n.clone());
                }
            }
            // Multiple outputs are unpacked inside a temporary-result block, so
            // their first declarations must remain in the enclosing MATLAB scope.
            StmtKind::Assign(Target::Many(names), _) if names.len() == 1 => {
                let n = &names[0];
                if !seen.contains(n) && !optional.contains(n) {
                    inline.insert(n.clone());
                }
            }
            _ => {}
        }
        assigned(std::slice::from_ref(s), &mut seen);
    }
    inline
}
fn declarations(generator: &TypedGenerator<'_>, args: &BTreeSet<String>) -> String {
    generator
        .types
        .iter()
        .filter(|(n, _)| {
            !args.contains(*n)
                && !generator.hidden_loops.contains(*n)
                && !generator.inline.contains(*n)
        })
        .map(|(n, t)| {
            if generator.optional.contains(n) {
                format!("let mut v_{n}: Option<{}> = None;\n", t.rust())
            } else {
                format!(
                    "let {}v_{n}: {};\n",
                    if generator.mutable.contains(n) {
                        "mut "
                    } else {
                        ""
                    },
                    t.rust()
                )
            }
        })
        .collect()
}
/// Generate compact typed Rust using the separately packaged ndarray-based runtime.
/// Undeclared function parameters use `ArrayD<f64>`; unsupported type joins are diagnostics.
// Preserve MATLAB spelling and overwrite semantics without suppressing unrelated lints.
fn source_allowances(
    generator: &TypedGenerator<'_>,
    counts: &BTreeMap<String, usize>,
    function: Option<&str>,
    arguments: &[String],
) -> String {
    let mut lints = Vec::new();
    if generator
        .types
        .keys()
        .map(String::as_str)
        .chain(function)
        .any(|n| n.chars().any(char::is_uppercase))
    {
        lints.push("non_snake_case");
    }
    if counts
        .iter()
        .any(|(n, count)| *count > 1 || arguments.contains(n))
    {
        lints.push("unused_assignments");
    }
    if lints.is_empty() {
        String::new()
    } else {
        format!("#[allow({})]\n", lints.join(", "))
    }
}

// An unused MATLAB local is valid; give it an idiomatic underscore-prefixed Rust
// binding instead of suppressing unused-variable diagnostics for the entire file.
fn mark_unread_bindings(source: &str) -> Result<String, Error> {
    use syn::visit_mut::{self, VisitMut};
    #[derive(Default)]
    struct Uses {
        bindings: BTreeSet<String>,
        reads: BTreeSet<String>,
    }
    impl VisitMut for Uses {
        fn visit_pat_ident_mut(&mut self, node: &mut syn::PatIdent) {
            let name = node.ident.to_string();
            if name.starts_with("v_") {
                self.bindings.insert(name);
            }
            visit_mut::visit_pat_ident_mut(self, node);
        }
        fn visit_expr_path_mut(&mut self, node: &mut syn::ExprPath) {
            if let Some(name) = node.path.get_ident() {
                self.reads.insert(name.to_string());
            }
            visit_mut::visit_expr_path_mut(self, node);
        }
        fn visit_expr_assign_mut(&mut self, node: &mut syn::ExprAssign) {
            if !matches!(&*node.left, syn::Expr::Path(_)) {
                self.visit_expr_mut(&mut node.left);
            }
            self.visit_expr_mut(&mut node.right);
        }
    }
    struct Rename(BTreeSet<String>);
    impl VisitMut for Rename {
        fn visit_ident_mut(&mut self, node: &mut syn::Ident) {
            if self.0.contains(&node.to_string()) {
                *node = syn::Ident::new(&format!("_{node}"), node.span());
            }
        }
        fn visit_field_value_mut(&mut self, node: &mut syn::FieldValue) {
            self.visit_expr_mut(&mut node.expr);
        }
        fn visit_expr_field_mut(&mut self, node: &mut syn::ExprField) {
            self.visit_expr_mut(&mut node.base);
        }
    }
    let mut file = syn::parse_file(source).map_err(|e| error(1, format!("generated Rust: {e}")))?;
    for item in &mut file.items {
        if let syn::Item::Fn(function) = item {
            let mut uses = Uses::default();
            uses.visit_item_fn_mut(function);
            Rename(uses.bindings.difference(&uses.reads).cloned().collect())
                .visit_item_fn_mut(function);
        }
    }
    Ok(prettyplease::unparse(&file))
}

pub fn transpile_typed(source: &str, library: bool) -> Result<String, Error> {
    let mut parser = Parser {
        tokens: lex(source)?,
        pos: 0,
        depth: 0,
    };
    let script = parser.body()?;
    let mut functions = vec![];
    while parser.named("function") {
        if functions.len() >= 64 {
            return Err(error(1, "typed source exceeds 64-function limit"));
        }
        let function = parser.function_typed()?;
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
                    args: f
                        .args
                        .iter()
                        .map(|name| {
                            f.declarations.get(name).map_or(Ty::Numbers, |declaration| {
                                let scalar = declaration.dimensions == [Some(1), Some(1)];
                                match declaration.kind {
                                    ArgumentKind::Double => {
                                        if scalar {
                                            Ty::Number
                                        } else {
                                            Ty::Numbers
                                        }
                                    }
                                    ArgumentKind::Logical => {
                                        if scalar {
                                            Ty::Bool
                                        } else {
                                            Ty::Bools
                                        }
                                    }
                                    ArgumentKind::Character => {
                                        if declaration.dimensions[0] == Some(1) {
                                            Ty::Text
                                        } else {
                                            Ty::Chars
                                        }
                                    }
                                }
                            })
                        })
                        .collect(),
                    declared: f
                        .args
                        .iter()
                        .map(|name| f.declarations.contains_key(name))
                        .collect(),
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
            scope_types(&script, &[], &signatures, &mut inference_budget, &[])?;
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
            for ((parameter, declared), observed) in
                sig.args.iter_mut().zip(&sig.declared).zip(observed)
            {
                if !*declared
                    && let Some(t) = observed
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
            let (types, vars) = scope_types(
                &f.body,
                &args,
                &signatures,
                &mut inference_budget,
                &f.outputs,
            )?;
            call_evidence(&f.body, &types, &vars, &signatures, &mut evidence)?;
        }
        for (name, observed) in evidence {
            let sig = signatures.get_mut(&name).unwrap();
            for ((parameter, declared), observed) in
                sig.args.iter_mut().zip(&sig.declared).zip(observed)
            {
                if !*declared
                    && let Some(t) = observed
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
            let (types, _) = scope_types(
                &f.body,
                &args,
                &signatures,
                &mut inference_budget,
                &f.outputs,
            )?;
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
        "// Generated typed Rust. MATLAB indexing remains one-based in runtime helpers.\nuse unlinked_matlab_rt as rt;\n",
    );
    for f in &functions {
        let sig = &signatures[&f.name];
        let args = f
            .args
            .iter()
            .cloned()
            .zip(sig.args.iter().copied())
            .collect::<Vec<_>>();
        let (types, vars) = scope_types(
            &f.body,
            &args,
            &signatures,
            &mut inference_budget,
            &f.outputs,
        )?;
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
        let mut counts = BTreeMap::new();
        assignment_counts(&f.body, &mut counts, false);
        let borrowed = args
            .iter()
            .filter(|(n, t)| !t.scalar() && !counts.contains_key(n))
            .map(|(n, _)| n.clone())
            .collect();
        let mutable = counts
            .iter()
            .filter(|(n, count)| **count > 1 || optional.contains(*n))
            .map(|(n, _)| n.clone())
            .collect();
        let inline = inline_names(&f.body, &f.args, &optional);
        let mut generator = TypedGenerator {
            types,
            vars,
            optional,
            inline,
            borrowed,
            mutable,
            hidden_loops: loop_types(&f.body, &f.outputs).1,
            functions: &signatures,
            outputs: f.outputs.clone(),
            serial: 0,
            in_function: true,
        };
        let params = args
            .iter()
            .map(|(n, t)| {
                format!(
                    "{}v_{n}: {}{}",
                    if t.scalar() && generator.types[n] == *t && counts.contains_key(n) {
                        "mut "
                    } else {
                        ""
                    },
                    if t.scalar() { "" } else { "&" },
                    t.rust()
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let output_types = f
            .outputs
            .iter()
            .map(|n| generator.types[n].rust().to_string())
            .collect::<Vec<_>>();
        out.push_str(&source_allowances(
            &generator,
            &counts,
            Some(&f.name),
            &f.args,
        ));
        out.push_str(&format!(
            "pub fn f_{}({params}) -> rt::Result<{}> {{\n",
            f.name,
            tuple(&output_types)
        ));
        for (name, declaration) in &f.declarations {
            let ty = sig.args[f.args.iter().position(|n| n == name).unwrap()];
            if !ty.scalar() {
                if ty == Ty::Text {
                    if let Some(columns) = declaration.dimensions[1] {
                        out.push_str(&format!("if v_{name}.len() != {columns} {{ return Err(rt::Error::Shape({:?}.into())); }}\n",format!("argument {name} must have {columns} characters")));
                    }
                } else {
                    out.push_str(&format!("if v_{name}.ndim() != 2 {{ return Err(rt::Error::UnsupportedRank {{ rank: v_{name}.ndim() }}); }}\n"));
                    for (axis, dimension) in declaration.dimensions.iter().enumerate() {
                        if let Some(dimension) = dimension {
                            out.push_str(&format!("if v_{name}.shape()[{axis}] != {dimension} {{ return Err(rt::Error::Shape({:?}.into())); }}\n",format!("argument {name} dimension {} must equal {dimension}",axis+1)));
                        }
                    }
                }
            }
        }
        // Argument assignments can promote types; preserve incoming ABI and convert once.
        for (n, t) in &args {
            if generator.types[n] != *t {
                out.push_str(&format!(
                    "let {}v_{n}: {} = rt::convert({}v_{n})?;\n",
                    if generator.mutable.contains(n) {
                        "mut "
                    } else {
                        ""
                    },
                    generator.types[n].rust(),
                    if t.scalar() { "&" } else { "" }
                ));
            } else if !t.scalar() && counts.contains_key(n) {
                out.push_str(&format!(
                    "let {}v_{n} = (*v_{n}).clone();\n",
                    if generator.mutable.contains(n) {
                        "mut "
                    } else {
                        ""
                    }
                ));
            }
        }
        out.push_str(&declarations(&generator, &f.args.iter().cloned().collect()));
        out.push_str(&generator.body(&f.body, 0)?);
        out.push_str(&format!("{}\n}}\n", generator.returns()));
    }
    if !library {
        let (types, vars) = scope_types(&script, &[], &signatures, &mut inference_budget, &[])?;
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
        let mut counts = BTreeMap::new();
        assignment_counts(&script, &mut counts, false);
        let mutable = counts
            .iter()
            .filter(|(n, count)| **count > 1 || optional.contains(*n))
            .map(|(n, _)| n.clone())
            .collect();
        let inline = inline_names(&script, &[], &optional);
        let mut generator = TypedGenerator {
            types,
            vars,
            optional,
            inline,
            borrowed: BTreeSet::new(),
            mutable,
            hidden_loops: loop_types(&script, &[]).1,
            functions: &signatures,
            outputs: vec![],
            serial: 0,
            in_function: false,
        };
        out.push_str("#[derive(Debug)]\n");
        if generator
            .types
            .keys()
            .any(|n| n.chars().any(char::is_uppercase))
        {
            out.push_str("#[allow(non_snake_case)]\n");
        }
        out.push_str("pub struct ScriptOutput {\n");
        for (n, t) in &generator.types {
            if generator.hidden_loops.contains(n) {
                continue;
            }
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
        out.push_str("}\n");
        out.push_str(&source_allowances(&generator, &counts, None, &[]));
        out.push_str("pub fn run_script() -> rt::Result<ScriptOutput> {\n");
        out.push_str(&declarations(&generator, &BTreeSet::new()));
        out.push_str(&generator.body(&script, 0)?);
        out.push_str("Ok(ScriptOutput {\n");
        for n in generator.types.keys() {
            if generator.hidden_loops.contains(n) {
                continue;
            }
            let value = if defined.contains(n) {
                generator.move_variable(n)
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
    if out.contains("ArrayD<") {
        out.insert_str(0, "use ndarray::ArrayD;\n");
    }
    crate::format_generated(&mark_unread_bindings(&out)?)
}

#[cfg(test)]
mod tests {
    use super::transpile_typed;
    #[test]
    fn first_multiple_output_assignment_declares_in_enclosing_scope() {
        for source in [
            "[r,c]=size(zeros(3,4)); disp(r); disp(c);",
            "[r,c]=pair(); disp(r); function [r,c]=pair(); r=3; c=4; end",
            "function [r,c]=dims(); [r,c]=size(zeros(3,4)); end",
        ] {
            let code = transpile_typed(source, source.starts_with("function")).unwrap();
            let result = code.find("let result_").unwrap();
            let scope = &code[..result];
            assert!(scope.contains("let v_r:"), "{code}");
            assert!(scope.contains("let v_c:"), "{code}");
            assert!(!code[result..].contains("let v_r:"), "{code}");
            assert!(!code[result..].contains("let v_c:"), "{code}");
        }
    }
    #[test]
    fn simple_functions_need_no_lint_suppression_and_unused_locals_are_named() {
        let code = transpile_typed(
            "function y=poly(x); arguments; x(1,1)double; end; y=x^2+1; end",
            true,
        )
        .unwrap();
        assert!(!code.contains("#[allow"));
        let code =
            transpile_typed("function y=f(x); y=1; for i=1:3; y=y+1; end; end", true).unwrap();
        assert!(code.contains("_v_x:"));
        assert!(code.contains("let _v_i:"));
        assert!(!code.contains("unused_variables"));
        let code = transpile_typed("x=1; if x>0; x=2; end", false).unwrap();
        assert!(!code.contains("else"));
    }
    #[test]
    fn scalar_locals_are_typed_and_only_uncertain_variables_are_optional() {
        let code = transpile_typed("x=2; y=x*x+3; if y>5; z=4; end; disp(z);", false).unwrap();
        assert!(code.contains("let v_x: f64 ="));
        assert!(code.contains("let v_y: f64 ="));
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
        assert!(code.contains("let v_b: ArrayD<f64> ="));
        let code = transpile_typed("x=1; x(2)=true;", false).unwrap();
        assert!(code.contains("let mut v_x: ArrayD<f64> ="));
    }
    #[test]
    fn char_calls_specialize_while_unknown_inputs_remain_numeric_arrays() {
        let code =
            transpile_typed("y=f('abc'); function y=f(x); y=strcmp(x,'abc'); end", false).unwrap();
        assert!(code.contains("f_f(v_x: &String) -> rt::Result<bool>"));
        let code = transpile_typed("function y=f(x); y=x*2; end", true).unwrap();
        assert!(code.contains("v_x: &ArrayD<f64>"));
    }
    #[test]
    fn one_output_brackets_and_rust_reserved_names_emit_valid_forms() {
        let code = transpile_typed(
            "[a]=zeros(1,2); [b]=sort([2 1]); [c]=f(); _=3; function [x,y]=f(); x=1;y=2;end",
            false,
        )
        .unwrap();
        assert!(code.contains("let v_a: ArrayD<f64> ="));
        assert!(code.contains("(f_f()?).0"));
        assert!(code.contains("pub matlab_field__: f64,"));
        assert!(!code.contains("result_1.0"));
        let code =
            transpile_typed("disp(f(true)); function y=f(x); x=false;y=x;end", false).unwrap();
        assert!(code.contains("v_x: bool"));
    }
    #[test]
    fn typed_diagnostics_report_statement_lines() {
        let error = transpile_typed("x=1;\n\nx='text';", false).unwrap_err();
        assert_eq!(error.line, 3);
        let error =
            transpile_typed("x=1;\nif true\n y=unsupported_thing(x);\nend", false).unwrap_err();
        assert_eq!(error.line, 3);
        let error = transpile_typed("x=1;\n\nbreak;", false).unwrap_err();
        assert_eq!(error.line, 3);
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
