//! Bounded, real, two-dimensional MATLAB values used by the bounded reference interpreter.
use std::collections::BTreeMap;
use std::io::Write;

pub type ArrayResult<T> = Result<T, String>;
pub const MAX_ELEMENTS: usize = 1_000_000;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueKind {
    Numeric,
    Logical,
    Character,
}
/// Elements are column-major, as in MATLAB. Character values are ASCII codes.
#[derive(Debug, Clone, PartialEq)]
pub struct Value {
    pub rows: usize,
    pub cols: usize,
    pub data: Vec<f64>,
    pub kind: ValueKind,
}
#[derive(Debug, Clone)]
pub enum Index {
    All,
    Values(Value),
}
pub type Environment = BTreeMap<String, Value>;
pub fn get(env: &Environment, name: &str) -> ArrayResult<Value> {
    env.get(name)
        .cloned()
        .ok_or_else(|| format!("variable '{name}' is not assigned on this execution path"))
}
#[derive(Default)]
pub struct Runtime {
    steps: usize,
    depth: usize,
}
impl Runtime {
    pub fn tick(&mut self) -> ArrayResult<()> {
        self.steps += 1;
        if self.steps > 1_000_000 {
            Err("execution exceeds one million statement/loop steps".into())
        } else {
            Ok(())
        }
    }
    pub fn enter(&mut self) -> ArrayResult<()> {
        self.tick()?;
        if self.depth >= 64 {
            return Err("function recursion exceeds 64 calls".into());
        }
        self.depth += 1;
        Ok(())
    }
    pub fn leave(&mut self) {
        self.depth -= 1;
    }
}
fn checked_size(rows: usize, cols: usize) -> ArrayResult<usize> {
    let size = rows.checked_mul(cols).ok_or("matrix dimensions overflow")?;
    if rows > MAX_ELEMENTS || cols > MAX_ELEMENTS || size > MAX_ELEMENTS {
        return Err("matrix exceeds one million element/dimension limit".into());
    }
    Ok(size)
}
impl Value {
    /// Validate the public shape/data representation before accepting external values.
    pub fn validate(&self) -> ArrayResult<()> {
        if checked_size(self.rows, self.cols)? != self.data.len() {
            return Err("matrix shape does not match data".into());
        }
        if self.kind == ValueKind::Logical && self.data.iter().any(|x| *x != 0.0 && *x != 1.0) {
            return Err("logical array data must be zero or one".into());
        }
        if self.kind == ValueKind::Character
            && self
                .data
                .iter()
                .any(|x| !x.is_finite() || !(0.0..=127.0).contains(x) || x.fract() != 0.0)
        {
            return Err("character array data must be ASCII codes".into());
        }
        Ok(())
    }
    pub fn new(rows: usize, cols: usize, data: Vec<f64>) -> ArrayResult<Self> {
        if checked_size(rows, cols)? != data.len() {
            return Err("matrix shape does not match data".into());
        }
        Ok(Self {
            rows,
            cols,
            data,
            kind: ValueKind::Numeric,
        })
    }
    pub fn scalar(value: f64) -> Self {
        Self {
            rows: 1,
            cols: 1,
            data: vec![value],
            kind: ValueKind::Numeric,
        }
    }
    pub fn logical(value: bool) -> Self {
        let mut v = Self::scalar(value as u8 as f64);
        v.kind = ValueKind::Logical;
        v
    }
    pub fn row(data: &[f64]) -> ArrayResult<Self> {
        Self::new(1, data.len(), data.to_vec())
    }
    pub fn empty() -> Self {
        Self {
            rows: 0,
            cols: 0,
            data: Vec::new(),
            kind: ValueKind::Numeric,
        }
    }
    pub fn string(text: &str) -> ArrayResult<Self> {
        if !text.is_ascii() {
            return Err("only ASCII character arrays are supported".into());
        }
        let mut v = Self::new(1, text.len(), text.bytes().map(f64::from).collect())?;
        v.kind = ValueKind::Character;
        Ok(v)
    }
    pub fn text(&self) -> ArrayResult<String> {
        if self.kind != ValueKind::Character || self.rows > 1 {
            return Err("expected a character row vector".into());
        }
        Ok(self.data.iter().map(|x| *x as u8 as char).collect())
    }
    pub fn number(&self) -> ArrayResult<f64> {
        if self.data.len() != 1 {
            Err("expected a scalar".into())
        } else {
            Ok(self.data[0])
        }
    }
    pub fn truth(&self) -> ArrayResult<bool> {
        if self.data.iter().any(|x| x.is_nan()) {
            return Err("NaN cannot be converted to logical".into());
        }
        Ok(!self.data.is_empty() && self.data.iter().all(|x| *x != 0.0))
    }
    pub fn scalar_truth(&self) -> ArrayResult<bool> {
        self.number()?;
        self.truth()
    }
    pub fn transpose(&self) -> Self {
        let mut v = self.clone();
        v.rows = self.cols;
        v.cols = self.rows;
        if self.data.is_empty() {
            return v;
        }
        for c in 0..self.cols {
            for r in 0..self.rows {
                v.data[c + r * self.cols] = self.data[r + c * self.rows];
            }
        }
        v
    }
    pub fn columns(&self) -> impl Iterator<Item = Value> + '_ {
        (0..self.cols).map(|c| Value {
            rows: self.rows,
            cols: 1,
            data: self.data[c * self.rows..(c + 1) * self.rows].to_vec(),
            kind: self.kind,
        })
    }
    pub fn end_value(&self, dimension: usize, count: usize) -> Value {
        Value::scalar(if count == 1 {
            self.data.len()
        } else if dimension == 0 {
            self.rows
        } else {
            self.cols
        } as f64)
    }
    pub fn index(&self, indices: &[Index]) -> ArrayResult<Value> {
        if indices.len() == 1 {
            let positions = index_positions(&indices[0], self.data.len(), false)?;
            let (rows, cols) = match &indices[0] {
                Index::All => (positions.len(), 1),
                Index::Values(index) if self.is_vector() && index.is_vector() => {
                    if self.rows == 1 {
                        (1, positions.len())
                    } else {
                        (positions.len(), 1)
                    }
                }
                Index::Values(index) if index.kind == ValueKind::Logical => (positions.len(), 1),
                Index::Values(index) => (index.rows, index.cols),
            };
            let mut result = Value::new(
                rows,
                cols,
                positions.iter().map(|i| self.data[*i]).collect(),
            )?;
            result.kind = self.kind;
            Ok(result)
        } else if indices.len() == 2 {
            let rows = index_positions(&indices[0], self.rows, false)?;
            let cols = index_positions(&indices[1], self.cols, false)?;
            let size = checked_size(rows.len(), cols.len())?;
            if size == 0 {
                let mut result = Value::new(rows.len(), cols.len(), Vec::new())?;
                result.kind = self.kind;
                return Ok(result);
            }
            let mut data = Vec::with_capacity(size);
            for c in &cols {
                for r in &rows {
                    data.push(self.data[r + c * self.rows]);
                }
            }
            let mut result = Value::new(rows.len(), cols.len(), data)?;
            result.kind = self.kind;
            Ok(result)
        } else {
            Err("only one- and two-dimensional indexing is supported".into())
        }
    }
    pub fn assign(&mut self, indices: &[Index], rhs: &Value) -> ArrayResult<()> {
        if rhs.data.is_empty() {
            return Err("empty indexed assignment/deletion is unsupported".into());
        }
        let positions;
        if indices.len() == 1 {
            positions = index_positions(&indices[0], self.data.len(), true)?;
            let required = positions
                .iter()
                .max()
                .map_or(self.data.len(), |i| (i + 1).max(self.data.len()));
            if required > self.data.len() {
                if !self.is_vector() && (self.rows != 0 || self.cols != 0) {
                    return Err(
                        "linear indexed growth of non-vector matrices is unsupported".into(),
                    );
                }
                let (rows, cols) = if self.cols == 1 && self.rows > 1 {
                    (required, 1)
                } else {
                    (1, required)
                };
                checked_size(rows, cols)?;
                self.data.resize(required, 0.0);
                self.rows = rows;
                self.cols = cols;
            }
        } else if indices.len() == 2 {
            let rows = index_positions(&indices[0], self.rows, true)?;
            let cols = index_positions(&indices[1], self.cols, true)?;
            if rhs.data.len() != 1 && (rhs.rows != rows.len() || rhs.cols != cols.len()) {
                return Err("indexed assignment shape mismatch".into());
            }
            let new_rows = rows
                .iter()
                .max()
                .map_or(self.rows, |r| (r + 1).max(self.rows));
            let new_cols = cols
                .iter()
                .max()
                .map_or(self.cols, |c| (c + 1).max(self.cols));
            if new_rows != self.rows || new_cols != self.cols {
                let mut data = vec![0.0; checked_size(new_rows, new_cols)?];
                for c in 0..self.cols {
                    for r in 0..self.rows {
                        data[r + c * new_rows] = self.data[r + c * self.rows];
                    }
                }
                self.rows = new_rows;
                self.cols = new_cols;
                self.data = data;
            }
            positions = if rows.is_empty() || cols.is_empty() {
                Vec::new()
            } else {
                cols.iter()
                    .flat_map(|c| rows.iter().map(move |r| r + c * new_rows))
                    .collect()
            };
        } else {
            return Err("only one- and two-dimensional indexing is supported".into());
        }
        if rhs.data.len() != 1 && positions.len() != rhs.data.len() {
            return Err("indexed assignment element count mismatch".into());
        }
        for (i, position) in positions.into_iter().enumerate() {
            let value = rhs.data[if rhs.data.len() == 1 { 0 } else { i }];
            self.data[position] = match self.kind {
                ValueKind::Logical => {
                    if value.is_nan() {
                        return Err("NaN cannot be converted to logical".into());
                    }
                    (value != 0.0) as u8 as f64
                }
                ValueKind::Character => {
                    if !(0.0..=127.0).contains(&value) || value.fract() != 0.0 {
                        return Err("character assignment requires an ASCII code".into());
                    }
                    value
                }
                ValueKind::Numeric => value,
            };
        }
        Ok(())
    }
    fn is_vector(&self) -> bool {
        self.rows == 1 || self.cols == 1
    }
}
fn index_positions(index: &Index, length: usize, grow: bool) -> ArrayResult<Vec<usize>> {
    match index {
        Index::All => Ok((0..length).collect()),
        Index::Values(value) if value.kind == ValueKind::Logical => {
            let positions: Vec<_> = value
                .data
                .iter()
                .enumerate()
                .filter_map(|(i, x)| (*x != 0.0).then_some(i))
                .collect();
            if positions.last().is_some_and(|i| *i >= length) {
                return Err("logical index exceeds array bounds".into());
            }
            Ok(positions)
        }
        Index::Values(value) => value
            .data
            .iter()
            .map(|x| {
                if !x.is_finite() || *x < 1.0 || x.fract() != 0.0 || *x > MAX_ELEMENTS as f64 {
                    return Err(
                        "indices must be positive finite integers within the element limit".into(),
                    );
                }
                let i = *x as usize - 1;
                if !grow && i >= length {
                    Err("index exceeds array bounds".into())
                } else {
                    Ok(i)
                }
            })
            .collect(),
    }
}
pub fn concatenate(rows: Vec<Vec<Value>>) -> ArrayResult<Value> {
    if rows.is_empty() {
        return Ok(Value::empty());
    }
    let kind = if rows
        .iter()
        .flatten()
        .any(|v| v.kind == ValueKind::Character)
    {
        ValueKind::Character
    } else if rows.iter().flatten().all(|v| v.kind == ValueKind::Logical) {
        ValueKind::Logical
    } else {
        ValueKind::Numeric
    };
    let mut assembled = Vec::new();
    for row in rows {
        // Only the dimensionless [] literal is neutral. A 0-by-N or
        // N-by-0 array still constrains the concatenated dimensions.
        let parts: Vec<_> = row
            .into_iter()
            .filter(|v| v.rows != 0 || v.cols != 0)
            .collect();
        if parts.is_empty() {
            continue;
        }
        let height = parts[0].rows;
        if parts.iter().any(|v| v.rows != height) {
            return Err("horizontal concatenation height mismatch".into());
        }
        let width: usize = parts.iter().map(|v| v.cols).sum();
        checked_size(height, width)?;
        assembled.push(Value::new(
            height,
            width,
            parts.into_iter().flat_map(|v| v.data).collect(),
        )?);
    }
    if assembled.is_empty() {
        return Ok(Value::empty());
    }
    let width = assembled[0].cols;
    if assembled.iter().any(|v| v.cols != width) {
        return Err("vertical concatenation width mismatch".into());
    }
    let height: usize = assembled.iter().map(|v| v.rows).sum();
    let size = checked_size(height, width)?;
    // Shaped empty inputs retain dimensions but have no cells to interleave.
    // Avoid width * row-count work when the total element count is zero.
    if size == 0 {
        return Ok(Value {
            rows: height,
            cols: width,
            data: Vec::new(),
            kind,
        });
    }
    // Empty rows constrain width above but contribute no output cells.
    // Removing them also bounds mixed empty/nonempty concatenation work.
    assembled.retain(|row| row.rows != 0);
    let mut data = Vec::with_capacity(size);
    for c in 0..width {
        for row in &assembled {
            data.extend_from_slice(&row.data[c * row.rows..(c + 1) * row.rows]);
        }
    }
    if kind == ValueKind::Character
        && data
            .iter()
            .any(|x| !x.is_finite() || *x < 0.0 || *x > 127.0 || x.fract() != 0.0)
    {
        return Err("character concatenation requires ASCII codes".into());
    }
    Ok(Value {
        rows: height,
        cols: width,
        data,
        kind,
    })
}
pub fn range(start: &Value, step: &Value, stop: &Value) -> ArrayResult<Value> {
    let (start, step, stop) = (start.number()?, step.number()?, stop.number()?);
    if !start.is_finite() || !step.is_finite() || !stop.is_finite() {
        return Err("range operands must be finite".into());
    }
    if step == 0.0 {
        return Value::new(1, 0, vec![]);
    }
    let intervals = (stop - start) / step;
    let count = if intervals < 0.0 {
        0
    } else {
        let count = (intervals + 4.0 * f64::EPSILON * intervals.abs().max(1.0)).floor() + 1.0;
        if !count.is_finite() || count > MAX_ELEMENTS as f64 {
            return Err("range exceeds one million elements".into());
        }
        count as usize
    };
    // Match the exported helper's colon arithmetic: retain a reachable supplied
    // endpoint exactly and form the second half from that endpoint. An off-grid
    // stop is never substituted for the last arithmetic progression value.
    let last_index = count.saturating_sub(1);
    let tolerance = 4.0 * f64::EPSILON * intervals.abs().max(1.0);
    let last = if count <= 1 {
        start
    } else if (intervals - last_index as f64).abs() <= tolerance {
        stop
    } else {
        start + last_index as f64 * step
    };
    Value::new(
        1,
        count,
        (0..count)
            .map(|i| {
                if i == 0 {
                    start
                } else if i == last_index {
                    last
                } else if i == last_index - i {
                    start.midpoint(last)
                } else if i < last_index - i {
                    start + i as f64 * step
                } else {
                    last - (last_index - i) as f64 * step
                }
            })
            .collect(),
    )
}
pub fn unary(op: &str, value: &Value) -> ArrayResult<Value> {
    match op {
        "+" => {
            let mut result = value.clone();
            result.kind = ValueKind::Numeric;
            Ok(result)
        }
        "-" => Ok(Value {
            data: value.data.iter().map(|x| -x).collect(),
            kind: ValueKind::Numeric,
            ..value.clone()
        }),
        "~" => {
            if value.data.iter().any(|x| x.is_nan()) {
                return Err("NaN cannot be converted to logical".into());
            }
            Ok(Value {
                data: value
                    .data
                    .iter()
                    .map(|x| (*x == 0.0) as u8 as f64)
                    .collect(),
                kind: ValueKind::Logical,
                ..value.clone()
            })
        }
        "'" | ".'" => Ok(value.transpose()),
        _ => Err(format!("unsupported unary operator {op}")),
    }
}
fn broadcast(a: usize, b: usize) -> ArrayResult<usize> {
    if a == b {
        Ok(a)
    } else if a == 1 {
        Ok(b)
    } else if b == 1 {
        Ok(a)
    } else {
        Err("incompatible array dimensions".into())
    }
}
fn modulo(a: f64, b: f64) -> f64 {
    if b == 0.0 {
        return a;
    }
    let q = a / b;
    if !q.is_finite() || !b.is_finite() {
        return f64::NAN;
    }
    if (q - q.round()).abs() <= 2.0 * f64::EPSILON * q.abs() {
        0.0_f64.copysign(b)
    } else {
        a - q.floor() * b
    }
}
fn elementwise(op: &str, a: &Value, b: &Value) -> ArrayResult<Value> {
    let rows = broadcast(a.rows, b.rows)?;
    let cols = broadcast(a.cols, b.cols)?;
    let mut data = Vec::with_capacity(checked_size(rows, cols)?);
    for c in 0..if rows == 0 { 0 } else { cols } {
        for r in 0..rows {
            let x =
                a.data[if a.rows == 1 { 0 } else { r } + if a.cols == 1 { 0 } else { c * a.rows }];
            let y =
                b.data[if b.rows == 1 { 0 } else { r } + if b.cols == 1 { 0 } else { c * b.rows }];
            let n = match op {
                "+" => x + y,
                "-" => x - y,
                ".*" => x * y,
                "./" => x / y,
                ".\\" => y / x,
                ".^" => {
                    if x < 0.0 && y.fract() != 0.0 {
                        return Err("complex powers are unsupported".into());
                    }
                    x.powf(y)
                }
                "==" => (x == y) as u8 as f64,
                "~=" => (x != y) as u8 as f64,
                "<" => (x < y) as u8 as f64,
                ">" => (x > y) as u8 as f64,
                "<=" => (x <= y) as u8 as f64,
                ">=" => (x >= y) as u8 as f64,
                "&" | "|" => {
                    if x.is_nan() || y.is_nan() {
                        return Err("NaN cannot be converted to logical".into());
                    }
                    (if op == "&" {
                        x != 0.0 && y != 0.0
                    } else {
                        x != 0.0 || y != 0.0
                    }) as u8 as f64
                }
                "min" => x.min(y),
                "max" => x.max(y),
                "mod" => modulo(x, y),
                "rem" => x % y,
                "atan2" => x.atan2(y),
                _ => return Err(format!("unsupported elementwise operator {op}")),
            };
            data.push(n);
        }
    }
    let mut value = Value::new(rows, cols, data)?;
    if ["==", "~=", "<", ">", "<=", ">=", "&", "|"].contains(&op) {
        value.kind = ValueKind::Logical;
    }
    Ok(value)
}
fn multiply(a: &Value, b: &Value) -> ArrayResult<Value> {
    if a.data.len() == 1 || b.data.len() == 1 {
        return elementwise(".*", a, b);
    }
    if a.cols != b.rows {
        return Err("matrix multiplication inner dimensions disagree".into());
    }
    let size = checked_size(a.rows, b.cols)?;
    // An empty result requires no multiply-adds; looping over its empty rows
    // otherwise permits enormous inner-dimension * output-column work.
    if size == 0 {
        return Value::new(a.rows, b.cols, Vec::new());
    }
    if size.saturating_mul(a.cols) > 10_000_000 {
        return Err("matrix product exceeds ten million multiply-add operations".into());
    }
    let mut data = vec![0.0; size];
    for c in 0..b.cols {
        for k in 0..a.cols {
            for r in 0..a.rows {
                data[r + c * a.rows] += a.data[r + k * a.rows] * b.data[k + c * b.rows];
            }
        }
    }
    Value::new(a.rows, b.cols, data)
}
fn identity(n: usize) -> ArrayResult<Value> {
    let mut v = Value::new(n, n, vec![0.0; checked_size(n, n)?])?;
    for i in 0..n {
        v.data[i + i * n] = 1.0;
    }
    Ok(v)
}
fn solve(a: &Value, b: &Value) -> ArrayResult<Value> {
    if a.data.len() == 1 {
        return elementwise("./", b, a);
    }
    if a.rows != a.cols || a.rows != b.rows {
        return Err("left division supports square nonsingular coefficient matrices only".into());
    }
    let n = a.rows;
    if n.saturating_mul(n).saturating_mul(n + b.cols) > 10_000_000 {
        return Err("linear solve exceeds operation limit".into());
    }
    let mut a = a.clone();
    let mut x = b.clone();
    x.kind = ValueKind::Numeric;
    if a.data.iter().chain(&x.data).any(|v| !v.is_finite()) {
        return Err("linear solve requires finite inputs".into());
    }
    for p in 0..n {
        let pivot = (p..n)
            .max_by(|r, s| {
                a.data[*r + p * n]
                    .abs()
                    .total_cmp(&a.data[*s + p * n].abs())
            })
            .unwrap();
        if a.data[pivot + p * n] == 0.0 {
            return Err("singular matrix in left division".into());
        }
        for c in 0..n {
            a.data.swap(p + c * n, pivot + c * n);
        }
        for c in 0..x.cols {
            x.data.swap(p + c * n, pivot + c * n);
        }
        for r in p + 1..n {
            let scale = a.data[r + p * n] / a.data[p + p * n];
            for c in p + 1..n {
                a.data[r + c * n] -= scale * a.data[p + c * n];
            }
            for c in 0..x.cols {
                x.data[r + c * n] -= scale * x.data[p + c * n];
            }
        }
    }
    for c in 0..x.cols {
        for r in (0..n).rev() {
            let mut value = x.data[r + c * n];
            for k in r + 1..n {
                value -= a.data[r + k * n] * x.data[k + c * n];
            }
            x.data[r + c * n] = value / a.data[r + r * n];
        }
    }
    Ok(x)
}
pub fn binary(op: &str, a: &Value, b: &Value) -> ArrayResult<Value> {
    match op {
        "*" => multiply(a, b),
        "\\" => solve(a, b),
        "/" => {
            if b.data.len() == 1 {
                elementwise("./", a, b)
            } else {
                Ok(solve(&b.transpose(), &a.transpose())?.transpose())
            }
        }
        "^" => {
            if a.data.len() == 1 {
                if b.data.len() != 1 {
                    return Err(
                        "scalar-to-matrix powers are unsupported; use .^ for elementwise powers"
                            .into(),
                    );
                }
                return elementwise(".^", a, b);
            }
            let exponent = b.number()?;
            if a.rows != a.cols
                || !exponent.is_finite()
                || exponent.fract() != 0.0
                || exponent.abs() > 1024.0
            {
                return Err("matrix powers require a square matrix and integer exponent between -1024 and 1024".into());
            }
            let mut base = if exponent < 0.0 {
                solve(a, &identity(a.rows)?)?
            } else {
                a.clone()
            };
            let mut result = identity(a.rows)?;
            let mut power = exponent.abs() as u32;
            while power > 0 {
                if power & 1 == 1 {
                    result = multiply(&result, &base)?;
                }
                power /= 2;
                if power > 0 {
                    base = multiply(&base, &base)?;
                }
            }
            Ok(result)
        }
        _ => elementwise(op, a, b),
    }
}
fn dimension(value: &Value) -> ArrayResult<usize> {
    if value.kind == ValueKind::Character {
        return Err("dimensions must be numeric".into());
    }
    let n = value.number()?;
    if !n.is_finite() || n < 0.0 || n.fract() != 0.0 || n > MAX_ELEMENTS as f64 {
        Err("dimensions must be nonnegative integers within the element limit".into())
    } else {
        Ok(n as usize)
    }
}
fn shape(args: &[Value]) -> ArrayResult<(usize, usize)> {
    match args {
        [a] if a.data.len() == 2 => Ok((
            dimension(&Value::scalar(a.data[0]))?,
            dimension(&Value::scalar(a.data[1]))?,
        )),
        [a] => {
            let n = dimension(a)?;
            Ok((n, n))
        }
        [a, b] => Ok((dimension(a)?, dimension(b)?)),
        _ => Err("expected one size or two dimensions".into()),
    }
}
fn significant(x: f64, precision: usize) -> String {
    if !x.is_finite() {
        return if x.is_nan() {
            "NaN".into()
        } else if x > 0.0 {
            "Inf".into()
        } else {
            "-Inf".into()
        };
    }
    if x == 0.0 {
        return "0".into();
    }
    let p = precision.max(1);
    let exp = x.abs().log10().floor() as i32;
    if exp < -4 || exp >= p as i32 {
        let s = format!("{:.*e}", p - 1, x);
        let (mantissa, exponent) = s.split_once('e').unwrap();
        format!(
            "{}e{}",
            mantissa.trim_end_matches('0').trim_end_matches('.'),
            exponent
        )
    } else {
        let decimals = (p as i32 - 1 - exp).max(0) as usize;
        let s = format!("{x:.decimals$}");
        if decimals == 0 {
            s
        } else {
            s.trim_end_matches('0').trim_end_matches('.').into()
        }
    }
}
pub fn display(value: &Value) -> ArrayResult<()> {
    let mut output = String::new();
    if value.data.is_empty() {
        output.push_str(&format!("[]({}x{})\n", value.rows, value.cols));
    } else {
        for r in 0..value.rows {
            for c in 0..value.cols {
                if value.kind == ValueKind::Character {
                    output.push(value.data[r + c * value.rows] as u8 as char);
                } else {
                    if c > 0 {
                        output.push(' ');
                    }
                    output.push_str(&significant(value.data[r + c * value.rows], 17));
                }
            }
            output.push('\n');
        }
    }
    std::io::stdout()
        .lock()
        .write_all(output.as_bytes())
        .map_err(|e| e.to_string())
}
fn formatted(args: &[Value]) -> ArrayResult<String> {
    let Some(format) = args.first() else {
        return Err("format string required".into());
    };
    let format = format.text()?;
    let mut values: Vec<Value> = Vec::new();
    for v in &args[1..] {
        if v.kind == ValueKind::Character {
            values.push(v.clone());
        } else {
            values.extend(v.data.iter().map(|v| Value::scalar(*v)));
        }
    }
    let chars: Vec<char> = format.chars().collect();
    let mut next = 0;
    let mut out = String::new();
    loop {
        let before = next;
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            i += 1;
            if c == '\\' {
                let c = *chars.get(i).ok_or("incomplete format escape")?;
                i += 1;
                out.push(match c {
                    'n' => '\n',
                    't' => '\t',
                    'r' => '\r',
                    '\\' => '\\',
                    _ => return Err(format!("unsupported format escape \\{c}")),
                });
            } else if c == '%' {
                if chars.get(i) == Some(&'%') {
                    out.push('%');
                    i += 1;
                    continue;
                }
                let mut width = String::new();
                while chars.get(i).is_some_and(char::is_ascii_digit) {
                    width.push(chars[i]);
                    i += 1;
                }
                let width = if width.is_empty() {
                    0
                } else {
                    width.parse::<usize>().map_err(|_| "invalid format width")?
                };
                let precision = if chars.get(i) == Some(&'.') {
                    i += 1;
                    let mut p = String::new();
                    while chars.get(i).is_some_and(char::is_ascii_digit) {
                        p.push(chars[i]);
                        i += 1;
                    }
                    Some(p.parse::<usize>().map_err(|_| "invalid format precision")?)
                } else {
                    None
                };
                if width > 1024 || precision.is_some_and(|p| p > 32) {
                    return Err("format width/precision exceeds subset limit".into());
                }
                let spec = *chars.get(i).ok_or("incomplete format specifier")?;
                i += 1;
                let value = values
                    .get(next)
                    .ok_or("not enough values for format string")?;
                next += 1;
                let text = match spec {
                    's' => value.text()?,
                    'd' | 'i' => {
                        let n = value.number()?;
                        if !n.is_finite() || n.abs() > i64::MAX as f64 {
                            return Err("integer formatting outside supported range".into());
                        }
                        format!("{}", n.trunc() as i64)
                    }
                    'f' => format!("{:.*}", precision.unwrap_or(6), value.number()?),
                    'e' => format!("{:.*e}", precision.unwrap_or(6), value.number()?),
                    'g' => significant(value.number()?, precision.unwrap_or(6)),
                    _ => return Err(format!("unsupported format specifier %{spec}")),
                };
                out.push_str(&format!("{text:>width$}"));
            } else {
                out.push(c);
            }
            if out.len() > 4_000_000 {
                return Err("formatted output exceeds four MB".into());
            }
        }
        if next >= values.len() {
            break;
        }
        if next == before {
            return Err("unused format arguments".into());
        }
    }
    Ok(out)
}
fn reduction(name: &str, value: &Value, dim: usize) -> ArrayResult<Value> {
    if dim != 1 && dim != 2 {
        return Err("reduction supports dimensions 1 and 2 only".into());
    }
    let (rows, cols) = if dim == 1 {
        if name == "min" || name == "max" {
            (value.rows.min(1), value.cols)
        } else {
            (
                1,
                if value.rows == 0 && value.cols == 0 {
                    1
                } else {
                    value.cols
                },
            )
        }
    } else {
        (
            value.rows,
            if name == "min" || name == "max" {
                value.cols.min(1)
            } else {
                1
            },
        )
    };
    let mut out = Value::new(rows, cols, vec![0.0; checked_size(rows, cols)?])?;
    if out.data.is_empty() {
        if name == "all" || name == "any" {
            out.kind = ValueKind::Logical;
        }
        return Ok(out);
    }
    for c in 0..cols {
        for r in 0..rows {
            let length = if dim == 1 { value.rows } else { value.cols };
            let iter = (0..length).map(|k| {
                value.data[if dim == 1 {
                    k + c * value.rows
                } else {
                    r + k * value.rows
                }]
            });
            out.data[r + c * rows] = match name {
                "sum" => iter.sum(),
                "prod" => iter.product(),
                "min" => iter
                    .reduce(f64::min)
                    .ok_or("empty min reduction is unsupported")?,
                "max" => iter
                    .reduce(f64::max)
                    .ok_or("empty max reduction is unsupported")?,
                "all" => {
                    let vals = Value::row(&iter.collect::<Vec<_>>())?;
                    if vals.data.is_empty() {
                        1.0
                    } else {
                        vals.truth()? as u8 as f64
                    }
                }
                "any" => {
                    let vals: Vec<_> = iter.collect();
                    if vals.iter().any(|x| x.is_nan()) {
                        return Err("NaN cannot be converted to logical".into());
                    }
                    vals.iter().any(|x| *x != 0.0) as u8 as f64
                }
                _ => unreachable!(),
            };
        }
    }
    if name == "all" || name == "any" {
        out.kind = ValueKind::Logical;
    }
    Ok(out)
}
pub fn builtin(name: &str, args: Vec<Value>, outputs: usize) -> ArrayResult<Vec<Value>> {
    if outputs > 0 && ["disp", "assert"].contains(&name) {
        return Err(format!("{name} has no output value"));
    }
    if name == "size" && outputs == 2 && args.len() == 1 {
        return Ok(vec![
            Value::scalar(args[0].rows as f64),
            Value::scalar(args[0].cols as f64),
        ]);
    }
    if outputs > 1 {
        return Err(format!(
            "{name} does not support {outputs} outputs in this subset"
        ));
    }
    let arity = |n| {
        if args.len() == n {
            Ok(())
        } else {
            Err(format!("{name} expects {n} arguments"))
        }
    };
    let v = match name {
        "disp" => {
            arity(1)?;
            display(&args[0])?;
            Value::empty()
        }
        "fprintf" => {
            let text = formatted(&args)?;
            std::io::stdout()
                .lock()
                .write_all(text.as_bytes())
                .map_err(|e| e.to_string())?;
            Value::scalar(text.len() as f64)
        }
        "sprintf" => Value::string(&formatted(&args)?)?,
        "error" => return Err(formatted(&args)?),
        "assert" => {
            if args.is_empty() || args.len() > 2 {
                return Err("assert expects a condition and optional message".into());
            }
            if !args[0].truth()? {
                return Err(if args.len() == 2 {
                    args[1].text()?
                } else {
                    "assertion failed".into()
                });
            }
            Value::empty()
        }
        "numel" | "length" | "isempty" => {
            arity(1)?;
            match name {
                "numel" => Value::scalar(args[0].data.len() as f64),
                "length" => Value::scalar(if args[0].data.is_empty() {
                    0
                } else {
                    args[0].rows.max(args[0].cols)
                } as f64),
                _ => Value::logical(args[0].data.is_empty()),
            }
        }
        "size" => match args.as_slice() {
            [a] => Value::row(&[a.rows as f64, a.cols as f64])?,
            [a, d] => {
                let d = dimension(d)?;
                if d == 0 {
                    return Err("size dimension must be positive".into());
                }
                Value::scalar(match d {
                    1 => a.rows as f64,
                    2 => a.cols as f64,
                    _ => 1.0,
                })
            }
            _ => return Err("size expects one or two arguments".into()),
        },
        "zeros" | "ones" | "eye" => {
            let (rows, cols) = shape(&args)?;
            let mut v = Value::new(
                rows,
                cols,
                vec![if name == "ones" { 1.0 } else { 0.0 }; checked_size(rows, cols)?],
            )?;
            if name == "eye" {
                for i in 0..rows.min(cols) {
                    v.data[i + i * rows] = 1.0;
                }
            }
            v
        }
        "reshape" => {
            if args.len() != 2 && args.len() != 3 {
                return Err("reshape expects array and two dimensions".into());
            }
            let (rows, cols) = shape(&args[1..])?;
            let mut v = args[0].clone();
            if checked_size(rows, cols)? != v.data.len() {
                return Err("reshape element count mismatch".into());
            }
            v.rows = rows;
            v.cols = cols;
            v
        }
        "transpose" => {
            arity(1)?;
            args[0].transpose()
        }
        "num2str" => {
            arity(1)?;
            let number = args[0].number()?;
            let text = if number.is_finite() && number.fract() == 0.0 && number.abs() < 1e16 {
                format!("{number:.0}")
            } else {
                significant(number, 5)
            };
            Value::string(&text)?
        }
        "strcmp" => {
            arity(2)?;
            Value::logical(args[0].text()? == args[1].text()?)
        }
        "linspace" => {
            if args.len() != 2 && args.len() != 3 {
                return Err("linspace expects start, stop and optional count".into());
            }
            let a = args[0].number()?;
            let b = args[1].number()?;
            let n = if args.len() == 3 {
                dimension(&args[2])?
            } else {
                100
            };
            let data = (0..n)
                .map(|i| {
                    if n == 1 || i + 1 == n {
                        b
                    } else {
                        a + (b - a) * i as f64 / (n - 1) as f64
                    }
                })
                .collect();
            Value::new(1, n, data)?
        }
        "diag" => {
            arity(1)?;
            let a = &args[0];
            if a.rows == 0 && a.cols == 0 {
                return Ok(vec![Value::empty()]);
            }
            if a.is_vector() {
                let mut v = Value::new(
                    a.data.len(),
                    a.data.len(),
                    vec![0.0; checked_size(a.data.len(), a.data.len())?],
                )?;
                for (i, x) in a.data.iter().enumerate() {
                    v.data[i + i * v.rows] = *x;
                }
                v
            } else {
                Value::new(
                    a.rows.min(a.cols),
                    1,
                    (0..a.rows.min(a.cols))
                        .map(|i| a.data[i + i * a.rows])
                        .collect(),
                )?
            }
        }
        "sum" | "prod" | "all" | "any" => {
            if args.is_empty() || args.len() > 2 {
                return Err(format!("{name} expects array and optional dimension"));
            }
            let dim = if args.len() == 2 {
                dimension(&args[1])?
            } else if args[0].rows != 1 {
                1
            } else {
                2
            };
            reduction(name, &args[0], dim)?
        }
        "min" | "max" => match args.len() {
            1 => reduction(name, &args[0], if args[0].rows != 1 { 1 } else { 2 })?,
            2 => elementwise(name, &args[0], &args[1])?,
            3 if args[1].data.is_empty() => reduction(name, &args[0], dimension(&args[2])?)?,
            _ => return Err(format!("unsupported {name} arguments")),
        },
        "mod" | "rem" | "atan2" => {
            arity(2)?;
            elementwise(name, &args[0], &args[1])?
        }
        "norm" => {
            arity(1)?;
            if !args[0].is_vector() {
                return Err("norm currently supports vectors only".into());
            }
            Value::scalar(args[0].data.iter().fold(0.0_f64, |a, x| a.hypot(*x)))
        }
        "dot" => {
            arity(2)?;
            if !args[0].is_vector()
                || !args[1].is_vector()
                || args[0].data.len() != args[1].data.len()
            {
                return Err("dot requires equal-length vectors".into());
            }
            Value::scalar(
                args[0]
                    .data
                    .iter()
                    .zip(&args[1].data)
                    .map(|(a, b)| a * b)
                    .sum(),
            )
        }
        "sort" => {
            arity(1)?;
            let mut v = args[0].clone();
            if !v.is_vector() {
                return Err("sort currently supports vectors only".into());
            }
            // MATLAB/Octave place every NaN after real values, regardless
            // of its IEEE sign. Equal values retain their original order.
            v.data.sort_by(|a, b| match (a.is_nan(), b.is_nan()) {
                (true, true) => std::cmp::Ordering::Equal,
                (true, false) => std::cmp::Ordering::Greater,
                (false, true) => std::cmp::Ordering::Less,
                (false, false) => a.partial_cmp(b).unwrap(),
            });
            v
        }
        "find" => {
            arity(1)?;
            let a = &args[0];
            // Both MATLAB and Octave preserve the conventional [] result
            // for dimensionless empty input and scalar zero.
            if (a.rows == 0 && a.cols == 0) || (a.data.len() == 1 && a.data[0] == 0.0) {
                return Ok(vec![Value::empty()]);
            }
            let data: Vec<_> = a
                .data
                .iter()
                .enumerate()
                .filter_map(|(i, x)| (*x != 0.0).then_some((i + 1) as f64))
                .collect();
            Value::new(
                if a.rows == 1 { 1 } else { data.len() },
                if a.rows == 1 { data.len() } else { 1 },
                data,
            )?
        }
        "abs" | "sin" | "cos" | "tan" | "asin" | "acos" | "atan" | "sqrt" | "exp" | "log"
        | "log2" | "log10" | "floor" | "ceil" | "round" | "sign" | "isnan" | "isinf"
        | "isfinite" => {
            arity(1)?;
            let mut value = args[0].clone();
            value.kind = if name.starts_with("is") {
                ValueKind::Logical
            } else {
                ValueKind::Numeric
            };
            for x in &mut value.data {
                *x = match name {
                    "abs" => x.abs(),
                    "sin" => x.sin(),
                    "cos" => x.cos(),
                    "tan" => x.tan(),
                    "asin" => {
                        if x.abs() > 1.0 {
                            return Err("complex asin unsupported".into());
                        }
                        x.asin()
                    }
                    "acos" => {
                        if x.abs() > 1.0 {
                            return Err("complex acos unsupported".into());
                        }
                        x.acos()
                    }
                    "atan" => x.atan(),
                    "sqrt" => {
                        if *x < 0.0 {
                            return Err("complex sqrt unsupported".into());
                        }
                        x.sqrt()
                    }
                    "exp" => x.exp(),
                    "log" | "log2" | "log10" => {
                        if *x < 0.0 {
                            return Err("complex logarithm unsupported".into());
                        }
                        match name {
                            "log" => x.ln(),
                            "log2" => x.log2(),
                            _ => x.log10(),
                        }
                    }
                    "floor" => x.floor(),
                    "ceil" => x.ceil(),
                    "round" => x.round(),
                    "sign" => {
                        if *x == 0.0 {
                            0.0
                        } else {
                            x.signum()
                        }
                    }
                    "isnan" => x.is_nan() as u8 as f64,
                    "isinf" => x.is_infinite() as u8 as f64,
                    "isfinite" => x.is_finite() as u8 as f64,
                    _ => unreachable!(),
                };
            }
            value
        }
        _ => return Err(format!("unsupported builtin '{name}'")),
    };
    Ok(vec![v])
}
