//! Private compatibility kernels; generated programs use the typed public API.
use ndarray::{Array2, ShapeBuilder};
use std::io::Write;

pub type ArrayResult<T> = Result<T, String>;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueKind {
    Numeric,
    Logical,
    Character,
}
/// Elements are column-major, as in MATLAB. Character values are ASCII codes.
#[derive(Debug, Clone, PartialEq)]
pub struct Value {
    pub data: Array2<f64>,
    pub kind: ValueKind,
}
#[derive(Debug, Clone)]
pub enum Index {
    All,
    Values(Value),
}
fn checked_size(rows: usize, cols: usize) -> ArrayResult<usize> {
    let size = rows.checked_mul(cols).ok_or("matrix dimensions overflow")?;
    if rows > isize::MAX as usize
        || cols > isize::MAX as usize
        || size
            .checked_mul(std::mem::size_of::<f64>())
            .is_none_or(|bytes| bytes > isize::MAX as usize)
    {
        return Err("matrix shape cannot be represented in the address space".into());
    }
    Ok(size)
}
impl Value {
    pub fn rows(&self) -> usize {
        self.data.nrows()
    }
    pub fn cols(&self) -> usize {
        self.data.ncols()
    }
    /// Storage is always ndarray-owned and contiguous in MATLAB column order.
    pub fn flat(&self) -> &[f64] {
        self.data
            .as_slice_memory_order()
            .expect("F-order storage invariant")
    }
    fn flat_mut(&mut self) -> &mut [f64] {
        self.data
            .as_slice_memory_order_mut()
            .expect("F-order storage invariant")
    }
    pub fn validate(&self) -> ArrayResult<()> {
        checked_size(self.rows(), self.cols())?;
        if self.kind == ValueKind::Logical && self.flat().iter().any(|x| *x != 0.0 && *x != 1.0) {
            return Err("logical array data must be zero or one".into());
        }
        if self.kind == ValueKind::Character
            && self
                .flat()
                .iter()
                .any(|x| !x.is_finite() || !(0.0..=127.0).contains(x) || x.fract() != 0.0)
        {
            return Err("character array data must be ASCII codes".into());
        }
        Ok(())
    }
    pub fn new(rows: usize, cols: usize, data: Vec<f64>) -> ArrayResult<Self> {
        checked_size(rows, cols)?;
        Ok(Self {
            data: Array2::from_shape_vec((rows, cols).f(), data).map_err(|e| e.to_string())?,
            kind: ValueKind::Numeric,
        })
    }
    pub fn reshape(&mut self, rows: usize, cols: usize) -> ArrayResult<()> {
        checked_size(rows, cols)?;
        self.data = self
            .data
            .clone()
            .into_shape_with_order(((rows, cols), ndarray::Order::F))
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    pub fn scalar(value: f64) -> Self {
        Self {
            data: Array2::from_elem((1, 1).f(), value),
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
            data: Array2::zeros((0, 0).f()),
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
        if self.kind != ValueKind::Character || self.rows() > 1 {
            return Err("expected a character row vector".into());
        }
        Ok(self.flat().iter().map(|x| *x as u8 as char).collect())
    }
    pub fn number(&self) -> ArrayResult<f64> {
        if self.flat().len() != 1 {
            Err("expected a scalar".into())
        } else {
            Ok(self.flat()[0])
        }
    }
    pub fn truth(&self) -> ArrayResult<bool> {
        if self.flat().iter().any(|x| x.is_nan()) {
            return Err("NaN cannot be converted to logical".into());
        }
        Ok(!self.flat().is_empty() && self.flat().iter().all(|x| *x != 0.0))
    }
    pub fn scalar_truth(&self) -> ArrayResult<bool> {
        self.number()?;
        self.truth()
    }
    pub fn transpose(&self) -> Self {
        Self {
            data: Array2::from_shape_fn((self.cols(), self.rows()).f(), |(r, c)| self.data[[c, r]]),
            kind: self.kind,
        }
    }
    pub fn columns(&self) -> impl Iterator<Item = Value> + '_ {
        (0..self.cols()).map(|c| Value {
            data: Array2::from_shape_fn((self.rows(), 1).f(), |(r, _)| self.data[[r, c]]),
            kind: self.kind,
        })
    }
    pub fn end_value(&self, dimension: usize, count: usize) -> Value {
        Value::scalar(if count == 1 {
            self.flat().len()
        } else if dimension == 0 {
            self.rows()
        } else {
            self.cols()
        } as f64)
    }
    pub fn index(&self, indices: &[Index]) -> ArrayResult<Value> {
        if indices.len() == 1 {
            let positions = index_positions(&indices[0], self.flat().len(), false)?;
            let (rows, cols) = match &indices[0] {
                Index::All => (positions.len(), 1),
                Index::Values(index) if self.is_vector() && index.is_vector() => {
                    if self.rows() == 1 {
                        (1, positions.len())
                    } else {
                        (positions.len(), 1)
                    }
                }
                Index::Values(index) if index.kind == ValueKind::Logical => (positions.len(), 1),
                Index::Values(index) => (index.rows(), index.cols()),
            };
            let mut result = Value::new(
                rows,
                cols,
                positions.iter().map(|i| self.flat()[*i]).collect(),
            )?;
            result.kind = self.kind;
            Ok(result)
        } else if indices.len() == 2 {
            let rows = index_positions(&indices[0], self.rows(), false)?;
            let cols = index_positions(&indices[1], self.cols(), false)?;
            let size = checked_size(rows.len(), cols.len())?;
            if size == 0 {
                let mut result = Value::new(rows.len(), cols.len(), Vec::new())?;
                result.kind = self.kind;
                return Ok(result);
            }
            let mut data = Vec::with_capacity(size);
            for c in &cols {
                for r in &rows {
                    data.push(self.flat()[r + c * self.rows()]);
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
        if rhs.flat().is_empty() {
            return Err("empty indexed assignment/deletion is unsupported".into());
        }
        let positions;
        if indices.len() == 1 {
            positions = index_positions(&indices[0], self.flat().len(), true)?;
            let required = positions
                .iter()
                .max()
                .map_or(self.flat().len(), |i| (i + 1).max(self.flat().len()));
            if required > self.flat().len() {
                if !self.is_vector() && (self.rows() != 0 || self.cols() != 0) {
                    return Err(
                        "linear indexed growth of non-vector matrices is unsupported".into(),
                    );
                }
                let (rows, cols) = if self.cols() == 1 && self.rows() > 1 {
                    (required, 1)
                } else {
                    (1, required)
                };
                checked_size(rows, cols)?;
                let mut data = Array2::zeros((rows, cols).f());
                data.as_slice_memory_order_mut().unwrap()[..self.flat().len()]
                    .copy_from_slice(self.flat());
                self.data = data;
            }
        } else if indices.len() == 2 {
            let rows = index_positions(&indices[0], self.rows(), true)?;
            let cols = index_positions(&indices[1], self.cols(), true)?;
            if rhs.flat().len() != 1 && (rhs.rows() != rows.len() || rhs.cols() != cols.len()) {
                return Err("indexed assignment shape mismatch".into());
            }
            let new_rows = rows
                .iter()
                .max()
                .map_or(self.rows(), |r| (r + 1).max(self.rows()));
            let new_cols = cols
                .iter()
                .max()
                .map_or(self.cols(), |c| (c + 1).max(self.cols()));
            if new_rows != self.rows() || new_cols != self.cols() {
                let mut data = vec![0.0; checked_size(new_rows, new_cols)?];
                for c in 0..self.cols() {
                    for r in 0..self.rows() {
                        data[r + c * new_rows] = self.flat()[r + c * self.rows()];
                    }
                }
                self.data = Array2::from_shape_vec((new_rows, new_cols).f(), data)
                    .map_err(|e| e.to_string())?;
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
        if rhs.flat().len() != 1 && positions.len() != rhs.flat().len() {
            return Err("indexed assignment element count mismatch".into());
        }
        for (i, position) in positions.into_iter().enumerate() {
            let value = rhs.flat()[if rhs.flat().len() == 1 { 0 } else { i }];
            self.flat_mut()[position] = match self.kind {
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
        self.rows() == 1 || self.cols() == 1
    }
}
fn index_positions(index: &Index, length: usize, grow: bool) -> ArrayResult<Vec<usize>> {
    match index {
        Index::All => Ok((0..length).collect()),
        Index::Values(value) if value.kind == ValueKind::Logical => {
            let positions: Vec<_> = value
                .flat()
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
            .flat()
            .iter()
            .map(|x| {
                if !x.is_finite() || *x < 1.0 || x.fract() != 0.0 || *x >= usize::MAX as f64 {
                    return Err(
                        "indices must be positive finite integers representable by usize".into(),
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
            .filter(|v| v.rows() != 0 || v.cols() != 0)
            .collect();
        if parts.is_empty() {
            continue;
        }
        let height = parts[0].rows();
        if parts.iter().any(|v| v.rows() != height) {
            return Err("horizontal concatenation height mismatch".into());
        }
        let width: usize = parts.iter().map(|v| v.cols()).sum();
        checked_size(height, width)?;
        assembled.push(Value::new(
            height,
            width,
            parts.into_iter().flat_map(|v| v.flat().to_vec()).collect(),
        )?);
    }
    if assembled.is_empty() {
        return Ok(Value::empty());
    }
    let width = assembled[0].cols();
    if assembled.iter().any(|v| v.cols() != width) {
        return Err("vertical concatenation width mismatch".into());
    }
    let height: usize = assembled.iter().map(|v| v.rows()).sum();
    let size = checked_size(height, width)?;
    // Shaped empty inputs retain dimensions but have no cells to interleave.
    // Avoid width * row-count work when the total element count is zero.
    if size == 0 {
        let mut value = Value::new(height, width, Vec::new())?;
        value.kind = kind;
        return Ok(value);
    }
    // Empty rows constrain width above but contribute no output cells.
    // Removing them also bounds mixed empty/nonempty concatenation work.
    assembled.retain(|row| row.rows() != 0);
    let mut data = Vec::with_capacity(size);
    for c in 0..width {
        for row in &assembled {
            data.extend_from_slice(&row.flat()[c * row.rows()..(c + 1) * row.rows()]);
        }
    }
    if kind == ValueKind::Character
        && data
            .iter()
            .any(|x| !x.is_finite() || *x < 0.0 || *x > 127.0 || x.fract() != 0.0)
    {
        return Err("character concatenation requires ASCII codes".into());
    }
    let mut value = Value::new(height, width, data)?;
    value.kind = kind;
    Ok(value)
}
pub fn range(start: &Value, step: &Value, stop: &Value) -> ArrayResult<Value> {
    let iter = crate::range_iter(start.number()?, step.number()?, stop.number()?)
        .map_err(|e| e.to_string())?;
    Value::new(1, iter.len(), iter.collect())
}

pub fn unary(op: &str, value: &Value) -> ArrayResult<Value> {
    match op {
        "+" => {
            let mut result = value.clone();
            result.kind = ValueKind::Numeric;
            Ok(result)
        }
        "-" => Ok(Value {
            data: value.data.mapv(|x| -x),
            kind: ValueKind::Numeric,
        }),
        "~" => {
            if value.flat().iter().any(|x| x.is_nan()) {
                return Err("NaN cannot be converted to logical".into());
            }
            Ok(Value {
                data: value.data.mapv(|x| (x == 0.0) as u8 as f64),
                kind: ValueKind::Logical,
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
    let rows = broadcast(a.rows(), b.rows())?;
    let cols = broadcast(a.cols(), b.cols())?;
    let mut data = Vec::with_capacity(checked_size(rows, cols)?);
    for c in 0..if rows == 0 { 0 } else { cols } {
        for r in 0..rows {
            let x = a.flat()
                [if a.rows() == 1 { 0 } else { r } + if a.cols() == 1 { 0 } else { c * a.rows() }];
            let y = b.flat()
                [if b.rows() == 1 { 0 } else { r } + if b.cols() == 1 { 0 } else { c * b.rows() }];
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
    if a.flat().len() == 1 || b.flat().len() == 1 {
        return elementwise(".*", a, b);
    }
    if a.cols() != b.rows() {
        return Err("matrix multiplication inner dimensions disagree".into());
    }
    let size = checked_size(a.rows(), b.cols())?;
    // An empty result requires no multiply-adds; looping over its empty rows
    // otherwise permits enormous inner-dimension * output-column work.
    if size == 0 {
        return Value::new(a.rows(), b.cols(), Vec::new());
    }
    let product = nalgebra::DMatrix::from_column_slice(a.rows(), a.cols(), a.flat())
        * nalgebra::DMatrix::from_column_slice(b.rows(), b.cols(), b.flat());
    Value::new(a.rows(), b.cols(), product.as_slice().to_vec())
}
fn identity(n: usize) -> ArrayResult<Value> {
    let mut v = Value::new(n, n, vec![0.0; checked_size(n, n)?])?;
    for i in 0..n {
        v.flat_mut()[i + i * n] = 1.0;
    }
    Ok(v)
}
fn solve(a: &Value, b: &Value) -> ArrayResult<Value> {
    if a.flat().len() == 1 {
        return elementwise("./", b, a);
    }
    if a.rows() != a.cols() || a.rows() != b.rows() {
        return Err("left division supports square nonsingular coefficient matrices only".into());
    }
    if a.flat().iter().chain(b.flat()).any(|v| !v.is_finite()) {
        return Err("linear solve requires finite inputs".into());
    }
    let coefficients = nalgebra::DMatrix::from_column_slice(a.rows(), a.cols(), a.flat());
    let rhs = nalgebra::DMatrix::from_column_slice(b.rows(), b.cols(), b.flat());
    let solution = coefficients
        .lu()
        .solve(&rhs)
        .ok_or("singular matrix in left division")?;
    Value::new(b.rows(), b.cols(), solution.as_slice().to_vec())
}
pub fn binary(op: &str, a: &Value, b: &Value) -> ArrayResult<Value> {
    match op {
        "*" => multiply(a, b),
        "\\" => solve(a, b),
        "/" => {
            if b.flat().len() == 1 {
                elementwise("./", a, b)
            } else {
                Ok(solve(&b.transpose(), &a.transpose())?.transpose())
            }
        }
        "^" => {
            if a.flat().len() == 1 {
                if b.flat().len() != 1 {
                    return Err(
                        "scalar-to-matrix powers are unsupported; use .^ for elementwise powers"
                            .into(),
                    );
                }
                return elementwise(".^", a, b);
            }
            let exponent = b.number()?;
            if a.rows() != a.cols() || !exponent.is_finite() || exponent.fract() != 0.0 {
                return Err(
                    "matrix powers require a square matrix and a finite integer exponent".into(),
                );
            }
            let mut base = if exponent < 0.0 {
                solve(a, &identity(a.rows())?)?
            } else {
                a.clone()
            };
            let mut result = identity(a.rows())?;
            let mut power = exponent.abs();
            while power > 0.0 {
                if power % 2.0 == 1.0 {
                    result = multiply(&result, &base)?;
                }
                power = (power / 2.0).floor();
                if power > 0.0 {
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
    if !n.is_finite() || n < 0.0 || n.fract() != 0.0 || n >= usize::MAX as f64 {
        Err("dimensions must be nonnegative integers representable by usize".into())
    } else {
        Ok(n as usize)
    }
}
fn shape(args: &[Value]) -> ArrayResult<(usize, usize)> {
    match args {
        [a] if a.flat().len() == 2 => Ok((
            dimension(&Value::scalar(a.flat()[0]))?,
            dimension(&Value::scalar(a.flat()[1]))?,
        )),
        [a] => {
            let n = dimension(a)?;
            Ok((n, n))
        }
        [a, b] => Ok((dimension(a)?, dimension(b)?)),
        _ => Err("expected one size or two dimensions".into()),
    }
}
// Every finite IEEE double has a terminating decimal expansion within 1074
// fractional places. Beyond that, append zeros explicitly instead of passing
// enormous precision/width to std::fmt's internally bounded argument encoding.
fn decimal_format(x: f64, precision: usize, scientific: bool) -> ArrayResult<String> {
    let digits = precision.min(1074);
    let mut text = if scientific {
        format!("{x:.digits$e}")
    } else {
        format!("{x:.digits$}")
    };
    if x.is_finite() && precision > digits {
        let zeros = precision - digits;
        let exponent = if scientific {
            Some(text.split_off(text.rfind('e').unwrap()))
        } else {
            None
        };
        text.try_reserve(zeros).map_err(|e| e.to_string())?;
        text.extend(std::iter::repeat_n('0', zeros));
        if let Some(exponent) = exponent {
            text.push_str(&exponent);
        }
    }
    Ok(text)
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
    if exp < -4 || exp >= 0 && exp as usize >= p {
        let s = format!("{:.*e}", (p - 1).min(1074), x);
        let (mantissa, exponent) = s.split_once('e').unwrap();
        format!(
            "{}e{}",
            mantissa.trim_end_matches('0').trim_end_matches('.'),
            exponent
        )
    } else {
        let decimals = if exp >= 0 {
            p.saturating_sub(1 + exp as usize)
        } else {
            p.saturating_add((-exp) as usize).saturating_sub(1)
        };
        let digits = decimals.min(1074);
        let s = format!("{x:.digits$}");
        if decimals == 0 {
            s
        } else {
            s.trim_end_matches('0').trim_end_matches('.').into()
        }
    }
}
pub fn display(value: &Value) -> ArrayResult<()> {
    let mut output = String::new();
    if value.flat().is_empty() {
        output.push_str(&format!("[]({}x{})\n", value.rows(), value.cols()));
    } else {
        for r in 0..value.rows() {
            for c in 0..value.cols() {
                if value.kind == ValueKind::Character {
                    output.push(value.flat()[r + c * value.rows()] as u8 as char);
                } else {
                    if c > 0 {
                        output.push(' ');
                    }
                    output.push_str(&significant(value.flat()[r + c * value.rows()], 17));
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
            values.extend(v.flat().iter().map(|v| Value::scalar(*v)));
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
                    'f' | 'e' => {
                        decimal_format(value.number()?, precision.unwrap_or(6), spec == 'e')?
                    }
                    'g' => significant(value.number()?, precision.unwrap_or(6)),
                    _ => return Err(format!("unsupported format specifier %{spec}")),
                };
                let padding = width.saturating_sub(text.len());
                out.try_reserve(
                    padding
                        .checked_add(text.len())
                        .ok_or("formatted size overflow")?,
                )
                .map_err(|e| e.to_string())?;
                out.extend(std::iter::repeat_n(' ', padding));
                out.push_str(&text);
            } else {
                out.push(c);
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
            (value.rows().min(1), value.cols())
        } else {
            (
                1,
                if value.rows() == 0 && value.cols() == 0 {
                    1
                } else {
                    value.cols()
                },
            )
        }
    } else {
        (
            value.rows(),
            if name == "min" || name == "max" {
                value.cols().min(1)
            } else {
                1
            },
        )
    };
    let mut out = Value::new(rows, cols, vec![0.0; checked_size(rows, cols)?])?;
    if out.flat().is_empty() {
        if name == "all" || name == "any" {
            out.kind = ValueKind::Logical;
        }
        return Ok(out);
    }
    for c in 0..cols {
        for r in 0..rows {
            let length = if dim == 1 { value.rows() } else { value.cols() };
            let iter = (0..length).map(|k| {
                value.flat()[if dim == 1 {
                    k + c * value.rows()
                } else {
                    r + k * value.rows()
                }]
            });
            out.flat_mut()[r + c * rows] = match name {
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
                    if vals.flat().is_empty() {
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
            Value::scalar(args[0].rows() as f64),
            Value::scalar(args[0].cols() as f64),
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
        "inv" => {
            arity(1)?;
            solve(&args[0], &identity(args[0].rows())?)?
        }
        "det" => {
            arity(1)?;
            let a = &args[0];
            if a.rows() != a.cols() {
                return Err("det requires a square matrix".into());
            }
            if a.flat().iter().any(|x| !x.is_finite()) {
                return Err("det requires finite inputs".into());
            }
            Value::scalar(
                nalgebra::DMatrix::from_column_slice(a.rows(), a.cols(), a.flat()).determinant(),
            )
        }
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
                "numel" => Value::scalar(args[0].flat().len() as f64),
                "length" => Value::scalar(if args[0].flat().is_empty() {
                    0
                } else {
                    args[0].rows().max(args[0].cols())
                } as f64),
                _ => Value::logical(args[0].flat().is_empty()),
            }
        }
        "size" => match args.as_slice() {
            [a] => Value::row(&[a.rows() as f64, a.cols() as f64])?,
            [a, d] => {
                let d = dimension(d)?;
                if d == 0 {
                    return Err("size dimension must be positive".into());
                }
                Value::scalar(match d {
                    1 => a.rows() as f64,
                    2 => a.cols() as f64,
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
                    v.flat_mut()[i + i * rows] = 1.0;
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
            if checked_size(rows, cols)? != v.flat().len() {
                return Err("reshape element count mismatch".into());
            }
            v.reshape(rows, cols)?;
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
            if a.rows() == 0 && a.cols() == 0 {
                return Ok(vec![Value::empty()]);
            }
            if a.is_vector() {
                let mut v = Value::new(
                    a.flat().len(),
                    a.flat().len(),
                    vec![0.0; checked_size(a.flat().len(), a.flat().len())?],
                )?;
                for (i, x) in a.flat().iter().enumerate() {
                    v.data[[i, i]] = *x;
                }
                v
            } else {
                Value::new(
                    a.rows().min(a.cols()),
                    1,
                    (0..a.rows().min(a.cols()))
                        .map(|i| a.flat()[i + i * a.rows()])
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
            } else if args[0].rows() != 1 {
                1
            } else {
                2
            };
            reduction(name, &args[0], dim)?
        }
        "min" | "max" => match args.len() {
            1 => reduction(name, &args[0], if args[0].rows() != 1 { 1 } else { 2 })?,
            2 => elementwise(name, &args[0], &args[1])?,
            3 if args[1].flat().is_empty() => reduction(name, &args[0], dimension(&args[2])?)?,
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
            Value::scalar(args[0].flat().iter().fold(0.0_f64, |a, x| a.hypot(*x)))
        }
        "dot" => {
            arity(2)?;
            if !args[0].is_vector()
                || !args[1].is_vector()
                || args[0].flat().len() != args[1].flat().len()
            {
                return Err("dot requires equal-length vectors".into());
            }
            Value::scalar(
                args[0]
                    .flat()
                    .iter()
                    .zip(args[1].flat())
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
            v.flat_mut().sort_by(|a, b| match (a.is_nan(), b.is_nan()) {
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
            if (a.rows() == 0 && a.cols() == 0) || (a.flat().len() == 1 && a.flat()[0] == 0.0) {
                return Ok(vec![Value::empty()]);
            }
            let data: Vec<_> = a
                .flat()
                .iter()
                .enumerate()
                .filter_map(|(i, x)| (*x != 0.0).then_some((i + 1) as f64))
                .collect();
            Value::new(
                if a.rows() == 1 { 1 } else { data.len() },
                if a.rows() == 1 { data.len() } else { 1 },
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
            for x in value.flat_mut() {
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
