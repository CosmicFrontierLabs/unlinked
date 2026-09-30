//! Typed helpers for generated MATLAB programs. Public arrays use `ndarray`;
//! dense matrix multiplication and linear solves use `nalgebra`.
//!
//! No interpreter, environment, instruction counter, or dynamically typed value
//! is required in generated programs. The current MATLAB subset is real and
//! two-dimensional; arrays with higher rank are rejected explicitly.
mod compat;
pub use ndarray;
use ndarray::{ArrayD, IxDyn, ShapeBuilder};
pub type Result<T> = std::result::Result<T, String>;
pub const MAX_ELEMENTS: usize = compat::MAX_ELEMENTS;

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
}
/// Statically selected result type; conversions check shape and element validity.
pub trait Output: Matlab + Sized {
    fn from_matlab_parts(
        rows: usize,
        cols: usize,
        data: Vec<f64>,
        kind: ElementKind,
    ) -> Result<Self>;
}
fn checked_shape(rows: usize, cols: usize) -> Result<()> {
    if rows > MAX_ELEMENTS
        || cols > MAX_ELEMENTS
        || rows.checked_mul(cols).is_none_or(|n| n > MAX_ELEMENTS)
    {
        Err("array exceeds one million element/dimension limit".into())
    } else {
        Ok(())
    }
}
fn shape<T>(a: &ArrayD<T>) -> Result<(usize, usize)> {
    let result = match a.shape() {
        [] => (1, 1),
        [n] => (1, *n),
        [r, c] => (*r, *c),
        _ => {
            return Err(
                "only rank zero, one and two arrays are supported by these MATLAB helpers".into(),
            )
        }
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
    T::from_matlab_parts(
        v.rows(),
        v.cols(),
        v.flat().to_vec(),
        match v.kind {
            compat::ValueKind::Numeric => ElementKind::Numeric,
            compat::ValueKind::Logical => ElementKind::Logical,
            compat::ValueKind::Character => ElementKind::Character,
        },
    )
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
        }
    };
}
array_input!(f64, Numeric, |v: &f64| *v);
array_input!(bool, Logical, |v: &bool| f64::from(*v));
array_input!(u8, Character, |v: &u8| f64::from(*v));
impl Output for ArrayD<f64> {
    fn from_matlab_parts(rows: usize, cols: usize, data: Vec<f64>, _: ElementKind) -> Result<Self> {
        validated_parts(rows, cols, &data)?;
        ArrayD::from_shape_vec(IxDyn(&[rows, cols]).f(), data).map_err(|e| e.to_string())
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
        .map_err(|e| e.to_string())
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
        .map_err(|e| e.to_string())
    }
}
impl sealed::Sealed for String {}
impl Matlab for String {
    fn matlab_shape(&self) -> Result<(usize, usize)> {
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
pub fn convert<T: Output>(v: &dyn Matlab) -> Result<T> {
    output(read(v)?)
}
pub fn truth(v: &dyn Matlab) -> Result<bool> {
    read(v)?.truth()
}
pub fn scalar_truth(v: &dyn Matlab) -> Result<bool> {
    read(v)?.scalar_truth()
}
pub fn binary<T: Output>(op: &str, a: &dyn Matlab, b: &dyn Matlab) -> Result<T> {
    output(compat::binary(op, &read(a)?, &read(b)?)?)
}
pub fn unary<T: Output>(op: &str, a: &dyn Matlab) -> Result<T> {
    output(compat::unary(op, &read(a)?)?)
}
pub fn call<T: Output>(name: &str, args: &[&dyn Matlab]) -> Result<T> {
    let mut values = compat::builtin(
        name,
        args.iter().map(|v| read(*v)).collect::<Result<_>>()?,
        1,
    )?;
    if values.len() != 1 {
        return Err("builtin did not return exactly one value".into());
    }
    output(values.remove(0))
}
pub fn call_outputs<T: Output>(name: &str, args: &[&dyn Matlab], count: usize) -> Result<Vec<T>> {
    compat::builtin(
        name,
        args.iter().map(|v| read(*v)).collect::<Result<_>>()?,
        count,
    )?
    .into_iter()
    .map(output)
    .collect()
}
pub fn call_void(name: &str, args: &[&dyn Matlab]) -> Result<()> {
    compat::builtin(
        name,
        args.iter().map(|v| read(*v)).collect::<Result<_>>()?,
        0,
    )?;
    Ok(())
}
#[derive(Clone, Debug)]
pub struct Index(compat::Index);
impl Index {
    #[allow(non_upper_case_globals)]
    pub const All: Self = Self(compat::Index::All);
    pub fn values(v: &dyn Matlab) -> Result<Self> {
        Ok(Self(compat::Index::Values(read(v)?)))
    }
}
pub fn end(v: &dyn Matlab, dimension: usize, count: usize) -> Result<f64> {
    if count == 0 || count > 2 || dimension >= count {
        return Err("invalid index dimension".into());
    }
    read(v)?.end_value(dimension, count).number()
}
pub fn index<T: Output>(v: &dyn Matlab, indices: &[Index]) -> Result<T> {
    output(read(v)?.index(&indices.iter().map(|i| i.0.clone()).collect::<Vec<_>>())?)
}
pub fn assign<T: Output>(target: &mut T, indices: &[Index], rhs: &dyn Matlab) -> Result<()> {
    let mut value = read(target)?;
    value.assign(
        &indices.iter().map(|i| i.0.clone()).collect::<Vec<_>>(),
        &read(rhs)?,
    )?;
    *target = output(value)?;
    Ok(())
}
pub fn concatenate<T: Output>(rows: &[&[&dyn Matlab]]) -> Result<T> {
    let values = rows
        .iter()
        .map(|r| r.iter().map(|v| read(*v)).collect::<Result<Vec<_>>>())
        .collect::<Result<Vec<_>>>()?;
    output(compat::concatenate(values)?)
}
pub fn range(start: f64, step: f64, stop: f64) -> Result<ArrayD<f64>> {
    output(compat::range(
        &compat::Value::scalar(start),
        &compat::Value::scalar(step),
        &compat::Value::scalar(stop),
    )?)
}
pub fn columns<T: Output>(v: &dyn Matlab) -> Result<Vec<T>> {
    let value = read(v)?;
    if value.data.is_empty() {
        return Ok(Vec::new());
    }
    value.columns().map(output).collect()
}
pub fn display(v: &dyn Matlab) -> Result<()> {
    compat::display(&read(v)?)
}
/// Construct numeric arrays from column-major literal data.
pub fn array(rows: usize, cols: usize, data: Vec<f64>) -> Result<ArrayD<f64>> {
    ArrayD::<f64>::from_matlab_parts(rows, cols, data, ElementKind::Numeric)
}
pub fn mtimes<T: Output>(a: &dyn Matlab, b: &dyn Matlab) -> Result<T> {
    binary("*", a, b)
}
pub fn solve<T: Output>(a: &dyn Matlab, b: &dyn Matlab) -> Result<T> {
    binary("\\", a, b)
}
pub fn zeros(rows: usize, cols: usize) -> Result<ArrayD<f64>> {
    call("zeros", &[&(rows as f64), &(cols as f64)])
}
