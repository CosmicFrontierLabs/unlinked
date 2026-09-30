//! Typed helpers for generated MATLAB programs. Public arrays use `ndarray`;
//! dense matrix multiplication and linear solves use `nalgebra`.
//!
//! No interpreter, environment, instruction counter, or dynamically typed value
//! is required in generated programs. The current MATLAB subset is real and
//! two-dimensional; arrays with higher rank are rejected explicitly.
mod compat;
pub use ndarray;
use ndarray::{ArrayD, IxDyn, ShapeBuilder};
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    UndefinedVariable(String),
    UnsupportedRank { rank: usize },
    Shape(String),
    Semantic(String),
}
impl Error {
    pub fn undefined(name: impl Into<String>) -> Self {
        Self::UndefinedVariable(name.into())
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UndefinedVariable(name) => write!(f, "undefined variable '{name}'"),
            Self::UnsupportedRank { rank } => write!(
                f,
                "unsupported array rank {rank}; MATLAB helpers currently support at most two dimensions"
            ),
            Self::Shape(message) | Self::Semantic(message) => f.write_str(message),
        }
    }
}
impl std::error::Error for Error {}
impl From<String> for Error {
    fn from(message: String) -> Self {
        Self::Semantic(message)
    }
}
impl From<&str> for Error {
    fn from(message: &str) -> Self {
        Self::Semantic(message.into())
    }
}
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ElementKind {
    Numeric,
    Logical,
    Character,
}
mod sealed {
    pub trait Sealed {}
}
/// Types that can be passed to the MATLAB semantic helpers.
pub trait Matlab: sealed::Sealed {
    fn matlab_shape(&self) -> Result<(usize, usize)>;
    fn matlab_kind(&self) -> ElementKind;
    fn matlab_column_major(&self) -> Result<Vec<f64>>;
    /// Read one coordinate without materializing or cloning an array.
    fn matlab_value_at(&self, row: usize, col: usize) -> Result<f64>;
}
/// Statically selected result type; conversions check shape and element validity.
pub trait Output: Matlab + Sized {
    /// In-place fast path for a scalar write that stays inside the current shape.
    fn assign_scalar_in_bounds(&mut self, _row: usize, _col: usize, _value: f64) -> Result<bool> {
        Ok(false)
    }
    fn from_matlab_parts(
        rows: usize,
        cols: usize,
        data: Vec<f64>,
        kind: ElementKind,
    ) -> Result<Self>;
}
fn checked_shape(rows: usize, cols: usize) -> Result<()> {
    let size = rows
        .checked_mul(cols)
        .ok_or_else(|| Error::Shape("array dimensions overflow".into()))?;
    if rows > isize::MAX as usize
        || cols > isize::MAX as usize
        || size
            .checked_mul(std::mem::size_of::<f64>())
            .is_none_or(|bytes| bytes > isize::MAX as usize)
    {
        return Err(Error::Shape(
            "array shape cannot be represented in the address space".into(),
        ));
    }
    Ok(())
}

fn shape<T>(a: &ArrayD<T>) -> Result<(usize, usize)> {
    let result = match a.shape() {
        [] => (1, 1),
        [n] => (1, *n),
        [r, c] => (*r, *c),
        _ => return Err(Error::UnsupportedRank { rank: a.ndim() }),
    };
    checked_shape(result.0, result.1)?;
    Ok(result)
}
fn collect<T>(a: &ArrayD<T>, map: impl Fn(&T) -> f64) -> Result<Vec<f64>> {
    let (rows, cols) = shape(a)?;
    let mut values = Vec::with_capacity(a.len());
    // Index coordinates explicitly. ndarray iteration order and contiguous
    // memory order need not be MATLAB column order (including transposed arrays).
    if a.is_empty() {
        return Ok(values);
    }
    for c in 0..cols {
        for r in 0..rows {
            let v = match a.ndim() {
                0 => &a[IxDyn(&[])],
                1 => &a[IxDyn(&[c])],
                _ => &a[IxDyn(&[r, c])],
            };
            values.push(map(v));
        }
    }
    Ok(values)
}
fn read(value: &dyn Matlab) -> Result<compat::Value> {
    let (rows, cols) = value.matlab_shape()?;
    checked_shape(rows, cols)?;
    let mut v = compat::Value::new(rows, cols, value.matlab_column_major()?)?;
    v.kind = match value.matlab_kind() {
        ElementKind::Numeric => compat::ValueKind::Numeric,
        ElementKind::Logical => compat::ValueKind::Logical,
        ElementKind::Character => compat::ValueKind::Character,
    };
    v.validate()?;
    Ok(v)
}
fn output<T: Output>(v: compat::Value) -> Result<T> {
    v.validate()?;
    let (rows, cols) = (v.rows(), v.cols());
    let kind = match v.kind {
        compat::ValueKind::Numeric => ElementKind::Numeric,
        compat::ValueKind::Logical => ElementKind::Logical,
        compat::ValueKind::Character => ElementKind::Character,
    };
    let (data, offset) = v.data.into_raw_vec_and_offset();
    if offset.is_some_and(|offset| offset != 0) {
        return Err(Error::Shape(
            "unexpected nonzero array storage offset".into(),
        ));
    }
    T::from_matlab_parts(rows, cols, data, kind)
}

fn validated_parts(rows: usize, cols: usize, data: &[f64]) -> Result<()> {
    checked_shape(rows, cols)?;
    if rows * cols != data.len() {
        return Err("array shape does not match elements".into());
    }
    Ok(())
}
macro_rules! scalar_input {
    ($ty:ty,$kind:ident,$convert:expr) => {
        impl sealed::Sealed for $ty {}
        impl Matlab for $ty {
            fn matlab_shape(&self) -> Result<(usize, usize)> {
                Ok((1, 1))
            }
            fn matlab_kind(&self) -> ElementKind {
                ElementKind::$kind
            }
            fn matlab_column_major(&self) -> Result<Vec<f64>> {
                Ok(vec![($convert)(*self)])
            }
            fn matlab_value_at(&self, row: usize, col: usize) -> Result<f64> {
                if row != 0 || col != 0 {
                    return Err(Error::Shape("scalar coordinate out of bounds".into()));
                }
                Ok(($convert)(*self))
            }
        }
    };
}
scalar_input!(f64, Numeric, |v: f64| v);
scalar_input!(bool, Logical, |v: bool| f64::from(v));
impl Output for f64 {
    fn from_matlab_parts(rows: usize, cols: usize, data: Vec<f64>, _: ElementKind) -> Result<Self> {
        validated_parts(rows, cols, &data)?;
        if data.len() != 1 {
            return Err("expected a scalar".into());
        }
        Ok(data[0])
    }
}
impl Output for bool {
    fn from_matlab_parts(
        rows: usize,
        cols: usize,
        data: Vec<f64>,
        kind: ElementKind,
    ) -> Result<Self> {
        let v = f64::from_matlab_parts(rows, cols, data, kind)?;
        if v.is_nan() {
            return Err("NaN cannot be converted to logical".into());
        }
        Ok(v != 0.0)
    }
}
macro_rules! array_input {
    ($ty:ty,$kind:ident,$convert:expr) => {
        impl sealed::Sealed for ArrayD<$ty> {}
        impl Matlab for ArrayD<$ty> {
            fn matlab_shape(&self) -> Result<(usize, usize)> {
                shape(self)
            }
            fn matlab_kind(&self) -> ElementKind {
                ElementKind::$kind
            }
            fn matlab_column_major(&self) -> Result<Vec<f64>> {
                collect(self, $convert)
            }
            fn matlab_value_at(&self, row: usize, col: usize) -> Result<f64> {
                let (rows, cols) = shape(self)?;
                if row >= rows || col >= cols {
                    return Err(Error::Shape("array coordinate out of bounds".into()));
                }
                let value = match self.ndim() {
                    0 => &self[IxDyn(&[])],
                    1 => &self[IxDyn(&[col])],
                    _ => &self[IxDyn(&[row, col])],
                };
                let value = ($convert)(value);
                if ElementKind::$kind == ElementKind::Character && value > 127.0 {
                    return Err("character arrays require ASCII codes".into());
                }
                Ok(value)
            }
        }
    };
}
array_input!(f64, Numeric, |v: &f64| *v);
array_input!(bool, Logical, |v: &bool| f64::from(*v));
array_input!(u8, Character, |v: &u8| f64::from(*v));
impl Output for ArrayD<f64> {
    fn assign_scalar_in_bounds(&mut self, row: usize, col: usize, value: f64) -> Result<bool> {
        let (rows, cols) = shape(self)?;
        if row >= rows || col >= cols {
            return Ok(false);
        }
        match self.ndim() {
            0 => self[IxDyn(&[])] = value,
            1 => self[IxDyn(&[col])] = value,
            _ => self[IxDyn(&[row, col])] = value,
        }
        Ok(true)
    }

    fn from_matlab_parts(rows: usize, cols: usize, data: Vec<f64>, _: ElementKind) -> Result<Self> {
        validated_parts(rows, cols, &data)?;
        ArrayD::from_shape_vec(IxDyn(&[rows, cols]).f(), data)
            .map_err(|e| Error::Shape(e.to_string()))
    }
}
impl Output for ArrayD<bool> {
    fn from_matlab_parts(rows: usize, cols: usize, data: Vec<f64>, _: ElementKind) -> Result<Self> {
        validated_parts(rows, cols, &data)?;
        if data.iter().any(|v| v.is_nan()) {
            return Err("NaN cannot be converted to logical".into());
        }
        ArrayD::from_shape_vec(
            IxDyn(&[rows, cols]).f(),
            data.into_iter().map(|v| v != 0.0).collect(),
        )
        .map_err(|e| Error::Shape(e.to_string()))
    }
}
impl Output for ArrayD<u8> {
    fn from_matlab_parts(rows: usize, cols: usize, data: Vec<f64>, _: ElementKind) -> Result<Self> {
        validated_parts(rows, cols, &data)?;
        if data
            .iter()
            .any(|v| !(0.0..=127.0).contains(v) || v.fract() != 0.0)
        {
            return Err("character arrays require ASCII codes".into());
        }
        ArrayD::from_shape_vec(
            IxDyn(&[rows, cols]).f(),
            data.into_iter().map(|v| v as u8).collect(),
        )
        .map_err(|e| Error::Shape(e.to_string()))
    }
}
impl sealed::Sealed for String {}
impl Matlab for String {
    fn matlab_shape(&self) -> Result<(usize, usize)> {
        if !self.is_ascii() {
            return Err("only ASCII character arrays are supported".into());
        }
        checked_shape(1, self.len())?;
        Ok((1, self.len()))
    }
    fn matlab_kind(&self) -> ElementKind {
        ElementKind::Character
    }
    fn matlab_column_major(&self) -> Result<Vec<f64>> {
        if !self.is_ascii() {
            return Err("only ASCII character arrays are supported".into());
        }
        Ok(self.bytes().map(f64::from).collect())
    }
    fn matlab_value_at(&self, row: usize, col: usize) -> Result<f64> {
        if row != 0 {
            return Err(Error::Shape("string coordinate out of bounds".into()));
        }
        let value = self
            .as_bytes()
            .get(col)
            .copied()
            .ok_or_else(|| Error::Shape("string coordinate out of bounds".into()))?;
        if !value.is_ascii() {
            return Err("only ASCII character arrays are supported".into());
        }
        Ok(f64::from(value))
    }
}
impl Output for String {
    fn from_matlab_parts(
        rows: usize,
        cols: usize,
        data: Vec<f64>,
        kind: ElementKind,
    ) -> Result<Self> {
        validated_parts(rows, cols, &data)?;
        if kind != ElementKind::Character || rows > 1 {
            return Err("expected a character row vector".into());
        }
        if data
            .iter()
            .any(|v| !(0.0..=127.0).contains(v) || v.fract() != 0.0)
        {
            return Err("character arrays require ASCII codes".into());
        }
        Ok(data.into_iter().map(|v| v as u8 as char).collect())
    }
}
/// Convert a scalar/array to the statically selected result type.
pub fn convert<T: Output>(v: &impl Matlab) -> Result<T> {
    output(read(v)?)
}
pub fn truth(v: &impl Matlab) -> Result<bool> {
    let (rows, cols) = v.matlab_shape()?;
    if rows == 0 || cols == 0 {
        return Ok(false);
    }
    let mut result = true;
    for col in 0..cols {
        for row in 0..rows {
            let x = v.matlab_value_at(row, col)?;
            if x.is_nan() {
                return Err("NaN cannot be converted to logical".into());
            }
            result &= x != 0.0;
        }
    }
    Ok(result)
}
pub fn scalar_truth(v: &impl Matlab) -> Result<bool> {
    let (rows, cols) = v.matlab_shape()?;
    if rows * cols != 1 {
        return Err(Error::Shape("expected a scalar".into()));
    }
    let x = v.matlab_value_at(0, 0)?;
    if x.is_nan() {
        return Err("NaN cannot be converted to logical".into());
    }
    Ok(x != 0.0)
}
fn binary<T: Output>(op: &str, a: &dyn Matlab, b: &dyn Matlab) -> Result<T> {
    output(compat::binary(op, &read(a)?, &read(b)?)?)
}
fn unary<T: Output>(op: &str, a: &dyn Matlab) -> Result<T> {
    output(compat::unary(op, &read(a)?)?)
}
#[derive(Clone, Debug)]
pub struct Index(compat::Index);
impl Index {
    #[allow(non_upper_case_globals)]
    pub const All: Self = Self(compat::Index::All);
    pub fn values(v: &impl Matlab) -> Result<Self> {
        Ok(Self(compat::Index::Values(read(v)?)))
    }
}
pub fn end(v: &impl Matlab, dimension: usize, count: usize) -> Result<f64> {
    if count == 0 || count > 2 || dimension >= count {
        return Err("invalid index dimension".into());
    }
    let (rows, cols) = v.matlab_shape()?;
    Ok(if count == 1 {
        rows * cols
    } else if dimension == 0 {
        rows
    } else {
        cols
    } as f64)
}
pub fn index<T: Output>(v: &impl Matlab, indices: &[Index]) -> Result<T> {
    if let Some((row, col)) = scalar_coordinates(v, indices)? {
        let value = v.matlab_value_at(row, col)?;
        return T::from_matlab_parts(1, 1, vec![value], v.matlab_kind());
    }
    output(read(v)?.index(&indices.iter().map(|i| i.0.clone()).collect::<Vec<_>>())?)
}
pub fn assign<T: Output>(target: &mut T, indices: &[Index], rhs: &impl Matlab) -> Result<()> {
    if rhs.matlab_shape()? == (1, 1)
        && let Some((row, col)) = scalar_coordinates(target, indices)?
        && target.assign_scalar_in_bounds(row, col, rhs.matlab_value_at(0, 0)?)?
    {
        return Ok(());
    }
    let mut value = read(target)?;
    value.assign(
        &indices.iter().map(|i| i.0.clone()).collect::<Vec<_>>(),
        &read(rhs)?,
    )?;
    *target = output(value)?;
    Ok(())
}
pub fn range(start: f64, step: f64, stop: f64) -> Result<ArrayD<f64>> {
    output(compat::range(
        &compat::Value::scalar(start),
        &compat::Value::scalar(step),
        &compat::Value::scalar(stop),
    )?)
}
pub fn columns<T: Output>(v: &impl Matlab) -> Result<Vec<T>> {
    let value = read(v)?;
    if value.data.is_empty() {
        return Ok(Vec::new());
    }
    value.columns().map(output).collect()
}
pub fn display(v: &impl Matlab) -> Result<()> {
    Ok(compat::display(&read(v)?)?)
}
/// Construct numeric arrays from column-major literal data.
pub fn array(rows: usize, cols: usize, data: Vec<f64>) -> Result<ArrayD<f64>> {
    ArrayD::<f64>::from_matlab_parts(rows, cols, data, ElementKind::Numeric)
}

macro_rules! binary_functions {
    ($($name:ident => $op:literal),+ $(,)?) => {$ (
        pub fn $name<T:Output>(a:&impl Matlab,b:&impl Matlab)->Result<T> { binary($op,a,b) }
    )+};
}
binary_functions! {
    add=>"+", subtract=>"-", times=>".*", rdivide=>"./", ldivide=>".\\", power=>".^",
    mtimes=>"*", mrdivide=>"/", mldivide=>"\\", mpower=>"^",
    eq=>"==", ne=>"~=", lt=>"<", le=>"<=", gt=>">", ge=>">=", and=>"&", or=>"|",
}
macro_rules! unary_functions {
    ($($name:ident => $op:literal),+ $(,)?) => {$ (
        pub fn $name<T:Output>(a:&impl Matlab)->Result<T> { unary($op,a) }
    )+};
}
unary_functions! { positive=>"+", negative=>"-", not=>"~", transpose=>"'" }

mod typed_args {
    use super::*;
    pub trait Collect {
        fn values(self) -> Result<Vec<compat::Value>>;
    }
    pub trait Rows {
        fn rows(self) -> Result<Vec<Vec<compat::Value>>>;
    }
    impl Collect for () {
        fn values(self) -> Result<Vec<compat::Value>> {
            Ok(vec![])
        }
    }
    impl Rows for () {
        fn rows(self) -> Result<Vec<Vec<compat::Value>>> {
            Ok(vec![])
        }
    }
    impl<A: Matlab> Collect for &[&A] {
        fn values(self) -> Result<Vec<compat::Value>> {
            self.iter().map(|a| read(*a)).collect()
        }
    }
    impl<A: Collect + Copy> Rows for &[A] {
        fn rows(self) -> Result<Vec<Vec<compat::Value>>> {
            self.iter().map(|a| a.values()).collect()
        }
    }
    impl<A: Matlab, const N: usize> Collect for &[&A; N] {
        fn values(self) -> Result<Vec<compat::Value>> {
            self.as_slice().values()
        }
    }
    impl<A: Collect + Copy, const N: usize> Rows for &[A; N] {
        fn rows(self) -> Result<Vec<Vec<compat::Value>>> {
            self.as_slice().rows()
        }
    }
    macro_rules! tuples {
        ($($arg:ident:$index:tt),+) => {
            impl<$($arg:Matlab),+> Collect for ($(&$arg,)+) {
                fn values(self)->Result<Vec<compat::Value>> { Ok(vec![$(read(self.$index)?,)+]) }
            }
            impl<$($arg:Collect),+> Rows for ($($arg,)+) {
                fn rows(self)->Result<Vec<Vec<compat::Value>>> { Ok(vec![$(self.$index.values()?,)+]) }
            }
        };
    }
    tuples!(A0:0);
    tuples!(A0:0,A1:1);
    tuples!(A0:0,A1:1,A2:2);
    tuples!(A0:0,A1:1,A2:2,A3:3);
    tuples!(A0:0,A1:1,A2:2,A3:3,A4:4);
    tuples!(A0:0,A1:1,A2:2,A3:3,A4:4,A5:5);
    tuples!(A0:0,A1:1,A2:2,A3:3,A4:4,A5:5,A6:6);
    tuples!(A0:0,A1:1,A2:2,A3:3,A4:4,A5:5,A6:6,A7:7);
    tuples!(A0:0,A1:1,A2:2,A3:3,A4:4,A5:5,A6:6,A7:7,A8:8);
    tuples!(A0:0,A1:1,A2:2,A3:3,A4:4,A5:5,A6:6,A7:7,A8:8,A9:9);
    tuples!(A0:0,A1:1,A2:2,A3:3,A4:4,A5:5,A6:6,A7:7,A8:8,A9:9,A10:10);
    tuples!(A0:0,A1:1,A2:2,A3:3,A4:4,A5:5,A6:6,A7:7,A8:8,A9:9,A10:10,A11:11);
    tuples!(A0:0,A1:1,A2:2,A3:3,A4:4,A5:5,A6:6,A7:7,A8:8,A9:9,A10:10,A11:11,A12:12);
    tuples!(A0:0,A1:1,A2:2,A3:3,A4:4,A5:5,A6:6,A7:7,A8:8,A9:9,A10:10,A11:11,A12:12,A13:13);
    tuples!(A0:0,A1:1,A2:2,A3:3,A4:4,A5:5,A6:6,A7:7,A8:8,A9:9,A10:10,A11:11,A12:12,A13:13,A14:14);
    tuples!(A0:0,A1:1,A2:2,A3:3,A4:4,A5:5,A6:6,A7:7,A8:8,A9:9,A10:10,A11:11,A12:12,A13:13,A14:14,A15:15);
    tuples!(A0:0,A1:1,A2:2,A3:3,A4:4,A5:5,A6:6,A7:7,A8:8,A9:9,A10:10,A11:11,A12:12,A13:13,A14:14,A15:15,A16:16);
    tuples!(A0:0,A1:1,A2:2,A3:3,A4:4,A5:5,A6:6,A7:7,A8:8,A9:9,A10:10,A11:11,A12:12,A13:13,A14:14,A15:15,A16:16,A17:17);
    tuples!(A0:0,A1:1,A2:2,A3:3,A4:4,A5:5,A6:6,A7:7,A8:8,A9:9,A10:10,A11:11,A12:12,A13:13,A14:14,A15:15,A16:16,A17:17,A18:18);
    tuples!(A0:0,A1:1,A2:2,A3:3,A4:4,A5:5,A6:6,A7:7,A8:8,A9:9,A10:10,A11:11,A12:12,A13:13,A14:14,A15:15,A16:16,A17:17,A18:18,A19:19);
    tuples!(A0:0,A1:1,A2:2,A3:3,A4:4,A5:5,A6:6,A7:7,A8:8,A9:9,A10:10,A11:11,A12:12,A13:13,A14:14,A15:15,A16:16,A17:17,A18:18,A19:19,A20:20);
    tuples!(A0:0,A1:1,A2:2,A3:3,A4:4,A5:5,A6:6,A7:7,A8:8,A9:9,A10:10,A11:11,A12:12,A13:13,A14:14,A15:15,A16:16,A17:17,A18:18,A19:19,A20:20,A21:21);
    tuples!(A0:0,A1:1,A2:2,A3:3,A4:4,A5:5,A6:6,A7:7,A8:8,A9:9,A10:10,A11:11,A12:12,A13:13,A14:14,A15:15,A16:16,A17:17,A18:18,A19:19,A20:20,A21:21,A22:22);
    tuples!(A0:0,A1:1,A2:2,A3:3,A4:4,A5:5,A6:6,A7:7,A8:8,A9:9,A10:10,A11:11,A12:12,A13:13,A14:14,A15:15,A16:16,A17:17,A18:18,A19:19,A20:20,A21:21,A22:22,A23:23);
    tuples!(A0:0,A1:1,A2:2,A3:3,A4:4,A5:5,A6:6,A7:7,A8:8,A9:9,A10:10,A11:11,A12:12,A13:13,A14:14,A15:15,A16:16,A17:17,A18:18,A19:19,A20:20,A21:21,A22:22,A23:23,A24:24);
    tuples!(A0:0,A1:1,A2:2,A3:3,A4:4,A5:5,A6:6,A7:7,A8:8,A9:9,A10:10,A11:11,A12:12,A13:13,A14:14,A15:15,A16:16,A17:17,A18:18,A19:19,A20:20,A21:21,A22:22,A23:23,A24:24,A25:25);
    tuples!(A0:0,A1:1,A2:2,A3:3,A4:4,A5:5,A6:6,A7:7,A8:8,A9:9,A10:10,A11:11,A12:12,A13:13,A14:14,A15:15,A16:16,A17:17,A18:18,A19:19,A20:20,A21:21,A22:22,A23:23,A24:24,A25:25,A26:26);
    tuples!(A0:0,A1:1,A2:2,A3:3,A4:4,A5:5,A6:6,A7:7,A8:8,A9:9,A10:10,A11:11,A12:12,A13:13,A14:14,A15:15,A16:16,A17:17,A18:18,A19:19,A20:20,A21:21,A22:22,A23:23,A24:24,A25:25,A26:26,A27:27);
    tuples!(A0:0,A1:1,A2:2,A3:3,A4:4,A5:5,A6:6,A7:7,A8:8,A9:9,A10:10,A11:11,A12:12,A13:13,A14:14,A15:15,A16:16,A17:17,A18:18,A19:19,A20:20,A21:21,A22:22,A23:23,A24:24,A25:25,A26:26,A27:27,A28:28);
    tuples!(A0:0,A1:1,A2:2,A3:3,A4:4,A5:5,A6:6,A7:7,A8:8,A9:9,A10:10,A11:11,A12:12,A13:13,A14:14,A15:15,A16:16,A17:17,A18:18,A19:19,A20:20,A21:21,A22:22,A23:23,A24:24,A25:25,A26:26,A27:27,A28:28,A29:29);
    tuples!(A0:0,A1:1,A2:2,A3:3,A4:4,A5:5,A6:6,A7:7,A8:8,A9:9,A10:10,A11:11,A12:12,A13:13,A14:14,A15:15,A16:16,A17:17,A18:18,A19:19,A20:20,A21:21,A22:22,A23:23,A24:24,A25:25,A26:26,A27:27,A28:28,A29:29,A30:30);
    tuples!(A0:0,A1:1,A2:2,A3:3,A4:4,A5:5,A6:6,A7:7,A8:8,A9:9,A10:10,A11:11,A12:12,A13:13,A14:14,A15:15,A16:16,A17:17,A18:18,A19:19,A20:20,A21:21,A22:22,A23:23,A24:24,A25:25,A26:26,A27:27,A28:28,A29:29,A30:30,A31:31);
}
/// Statically typed heterogeneous argument tuples (up to 32 arguments), or borrowed homogeneous slices.
pub trait Arguments: typed_args::Collect {}
impl<A: typed_args::Collect> Arguments for A {}
/// Statically typed rows of argument tuples.
pub trait Rows: typed_args::Rows {}
impl<A: typed_args::Rows> Rows for A {}
fn named_call<T: Output>(name: &str, args: impl Arguments) -> Result<T> {
    let mut values = compat::builtin(name, args.values()?, 1)?;
    if values.len() != 1 {
        return Err(Error::Semantic(
            "builtin did not return exactly one value".into(),
        ));
    }
    output(values.remove(0))
}
fn named_outputs<T: Output>(name: &str, args: impl Arguments, count: usize) -> Result<Vec<T>> {
    compat::builtin(name, args.values()?, count)?
        .into_iter()
        .map(output)
        .collect()
}
fn named_void(name: &str, args: impl Arguments) -> Result<()> {
    compat::builtin(name, args.values()?, 0)?;
    Ok(())
}
macro_rules! builtins {
    ($($name:ident),+ $(,)?) => {$ (
        pub fn $name<T:Output>(args:impl Arguments)->Result<T> { named_call(stringify!($name),args) }
    )+};
}
builtins! { sprintf,num2str,strcmp,isempty,numel,length,norm,dot,fprintf,det,inv,size,zeros,ones,eye,linspace,find,reshape,diag,sort,all,any,isnan,isinf,isfinite,sum,prod,min,max,rem,atan2,abs,sin,cos,tan,asin,acos,atan,sqrt,exp,log,log2,log10,floor,ceil,round,sign }
pub fn disp(args: impl Arguments) -> Result<()> {
    named_void("disp", args)
}
pub fn assert(args: impl Arguments) -> Result<()> {
    named_void("assert", args)
}
pub fn error(args: impl Arguments) -> Result<()> {
    named_void("error", args)
}
pub fn fprintf_void(args: impl Arguments) -> Result<()> {
    named_void("fprintf", args)
}
macro_rules! multi_outputs {
    ($($name:ident=>$builtin:literal),+ $(,)?) => {$ (
        pub fn $name<T:Output>(args:impl Arguments,count:usize)->Result<Vec<T>> { named_outputs($builtin,args,count) }
    )+};
}
multi_outputs! { size_outputs=>"size", min_outputs=>"min", max_outputs=>"max", sort_outputs=>"sort", find_outputs=>"find" }
pub fn concat<T: Output>(rows: impl Rows) -> Result<T> {
    output(compat::concatenate(rows.rows()?)?)
}

pub fn modulo<T: Output>(args: impl Arguments) -> Result<T> {
    named_call("mod", args)
}

impl<T: Matlab + ?Sized> sealed::Sealed for &T {}
impl<T: Matlab + ?Sized> Matlab for &T {
    fn matlab_shape(&self) -> Result<(usize, usize)> {
        (**self).matlab_shape()
    }
    fn matlab_kind(&self) -> ElementKind {
        (**self).matlab_kind()
    }
    fn matlab_column_major(&self) -> Result<Vec<f64>> {
        (**self).matlab_column_major()
    }
    fn matlab_value_at(&self, row: usize, col: usize) -> Result<f64> {
        (**self).matlab_value_at(row, col)
    }
}

fn scalar_coordinates(v: &impl Matlab, indices: &[Index]) -> Result<Option<(usize, usize)>> {
    let (rows, _) = v.matlab_shape()?;
    let mut coordinates = [0usize; 2];
    if indices.is_empty() || indices.len() > 2 {
        return Ok(None);
    }
    for (i, index) in indices.iter().enumerate() {
        let compat::Index::Values(value) = &index.0 else {
            return Ok(None);
        };
        if value.kind != compat::ValueKind::Numeric || value.data.len() != 1 {
            return Ok(None);
        }
        let x = value.flat()[0];
        if !x.is_finite() || x < 1.0 || x.fract() != 0.0 || x >= usize::MAX as f64 {
            return Err(Error::Semantic(
                "indices must be positive finite integers representable by usize".into(),
            ));
        }
        coordinates[i] = x as usize - 1;
    }
    if indices.len() == 1 {
        if rows == 0 {
            return Ok(None);
        }
        Ok(Some((coordinates[0] % rows, coordinates[0] / rows)))
    } else {
        Ok(Some((coordinates[0], coordinates[1])))
    }
}

/// Lazy numeric colon iteration for generated scalar for loops.
#[derive(Clone, Debug)]
pub struct RangeIter {
    start: f64,
    last: f64,
    step: f64,
    next: usize,
    count: usize,
}
impl Iterator for RangeIter {
    type Item = f64;
    fn next(&mut self) -> Option<f64> {
        if self.next == self.count {
            return None;
        }
        let last_index = self.count - 1;
        let i = self.next;
        let result = if i == 0 {
            self.start
        } else if i == last_index {
            self.last
        } else if i == last_index - i {
            self.start.midpoint(self.last)
        } else if i < last_index - i {
            self.start + i as f64 * self.step
        } else {
            self.last - (last_index - i) as f64 * self.step
        };
        self.next += 1;
        Some(result)
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.count - self.next;
        (remaining, Some(remaining))
    }
}
impl ExactSizeIterator for RangeIter {}
pub fn range_iter(start: f64, step: f64, stop: f64) -> Result<RangeIter> {
    if !start.is_finite() || !step.is_finite() || !stop.is_finite() {
        return Err(Error::Semantic("range operands must be finite".into()));
    }
    let intervals = if step == 0.0 {
        -1.0
    } else {
        (stop - start) / step
    };
    let count = if intervals < 0.0 {
        0
    } else {
        let count = (intervals + 4.0 * f64::EPSILON * intervals.abs().max(1.0)).floor() + 1.0;
        if !count.is_finite() || count >= usize::MAX as f64 {
            return Err(Error::Shape(
                "range length cannot be represented by usize".into(),
            ));
        }
        count as usize
    };
    let last_index = count.saturating_sub(1);
    let tolerance = 4.0 * f64::EPSILON * intervals.abs().max(1.0);
    let last = if count <= 1 {
        start
    } else if (intervals - last_index as f64).abs() <= tolerance {
        stop
    } else {
        start + last_index as f64 * step
    };
    Ok(RangeIter {
        start,
        last,
        step,
        next: 0,
        count,
    })
}
