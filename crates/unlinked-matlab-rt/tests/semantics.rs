use ndarray::{array, ArrayD, IxDyn};
use unlinked_matlab_rt as rt;

fn matrix(v: &ArrayD<f64>, rows: usize, cols: usize, expected: &[f64]) {
    assert_eq!(v.shape(), &[rows, cols]);
    let actual = rt::Matlab::matlab_column_major(v).unwrap();
    assert_eq!(actual.len(), expected.len());
    for (a, b) in actual.iter().zip(expected) {
        assert!((a - b).abs() < 1e-10, "{actual:?} != {expected:?}");
    }
}
#[test]
fn coordinates_not_memory_order_drive_ndarray_nalgebra_conversion() {
    let a = array![[1., 2., 3.], [4., 5., 6.]].into_dyn(); // C order
    let b = array![[7., 8.], [9., 10.], [11., 12.]].into_dyn();
    let product: ArrayD<f64> = rt::mtimes(&a, &b).unwrap();
    matrix(&product, 2, 2, &[58., 139., 64., 154.]);
    let transposed = a.reversed_axes(); // non-C order
    matrix(
        &rt::convert(&transposed).unwrap(),
        3,
        2,
        &[1., 2., 3., 4., 5., 6.],
    );
    let back: ArrayD<f64> = rt::unary("'", &transposed).unwrap();
    matrix(&back, 2, 3, &[1., 4., 2., 5., 3., 6.]);
}
#[test]
fn nalgebra_solve_inverse_determinant_and_matrix_power() {
    let a = array![[0., 2.], [1., 3.]].into_dyn(); // requires pivot
    let b = array![[4., 10.], [7., 17.]].into_dyn();
    let solution: ArrayD<f64> = rt::solve(&a, &b).unwrap();
    matrix(&solution, 2, 2, &[1., 2., 2., 5.]);
    assert_eq!(rt::call::<f64>("det", &[&a]).unwrap(), -2.);
    let inv: ArrayD<f64> = rt::call("inv", &[&a]).unwrap();
    matrix(&rt::mtimes(&a, &inv).unwrap(), 2, 2, &[1., 0., 0., 1.]);
    let squared: ArrayD<f64> = rt::binary("^", &a, &2.).unwrap();
    matrix(&squared, 2, 2, &[2., 3., 6., 11.]);
    let singular = array![[1., 2.], [2., 4.]].into_dyn();
    assert!(rt::solve::<ArrayD<f64>>(&singular, &b).is_err());
    let rectangular = rt::array(2, 3, vec![1.; 6]).unwrap();
    assert!(rt::call::<ArrayD<f64>>("inv", &[&rectangular]).is_err());
    let empty = rt::zeros(0, 0).unwrap();
    assert_eq!(rt::call::<f64>("det", &[&empty]).unwrap(), 1.);
    matrix(
        &rt::call::<ArrayD<f64>>("inv", &[&empty]).unwrap(),
        0,
        0,
        &[],
    );
}
#[test]
fn indexing_is_one_based_column_major_and_growth_fills_zeros() {
    let mut a = array![[1., 2., 3.], [4., 5., 6.]].into_dyn();
    assert_eq!(
        rt::index::<f64>(&a, &[rt::Index::values(&2.).unwrap()]).unwrap(),
        4.
    );
    assert_eq!(rt::end(&a, 0, 1).unwrap(), 6.);
    assert_eq!(rt::end(&a, 0, 2).unwrap(), 2.);
    assert_eq!(rt::end(&a, 1, 2).unwrap(), 3.);
    let mask = array![[true, false, false], [false, true, true]].into_dyn();
    matrix(
        &rt::index::<ArrayD<f64>>(&a, &[rt::Index::values(&mask).unwrap()]).unwrap(),
        3,
        1,
        &[1., 5., 6.],
    );
    rt::assign(
        &mut a,
        &[
            rt::Index::values(&3.).unwrap(),
            rt::Index::values(&4.).unwrap(),
        ],
        &9.,
    )
    .unwrap();
    matrix(&a, 3, 4, &[1., 4., 0., 2., 5., 0., 3., 6., 0., 0., 0., 9.]);
    assert!(rt::index::<f64>(&a, &[rt::Index::values(&0.).unwrap()]).is_err());
    let before = a.clone();
    assert!(rt::assign(&mut a, &[rt::Index::values(&100.).unwrap()], &3.).is_err());
    assert_eq!(a, before);
}
#[test]
fn logical_empty_broadcast_and_rank_contract() {
    let a = array![[1.], [2.]].into_dyn();
    let b = array![[10., 20., 30.]].into_dyn();
    matrix(
        &rt::binary::<ArrayD<f64>>("+", &a, &b).unwrap(),
        2,
        3,
        &[11., 12., 21., 22., 31., 32.],
    );
    let logical: ArrayD<bool> = rt::binary(">", &a, &1.).unwrap();
    assert_eq!(logical, array![[false], [true]].into_dyn());
    assert!(!rt::truth(&logical).unwrap());
    assert!(rt::scalar_truth(&logical).is_err());
    assert!(rt::convert::<bool>(&f64::NAN).is_err());
    let empty = rt::zeros(2, 0).unwrap();
    matrix(&empty, 2, 0, &[]);
    assert!(rt::call::<bool>("isempty", &[&empty]).unwrap());
    let mut empty = empty;
    assert!(rt::assign(&mut empty, &[rt::Index::values(&3.).unwrap()], &1.).is_err());
    let mut dimensionless = rt::zeros(0, 0).unwrap();
    rt::assign(&mut dimensionless, &[rt::Index::values(&3.).unwrap()], &1.).unwrap();
    matrix(&dimensionless, 1, 3, &[0., 0., 1.]);
    let rank3 = ArrayD::<f64>::zeros(IxDyn(&[1, 2, 3]));
    assert!(rt::convert::<ArrayD<f64>>(&rank3).is_err());
    assert!(rt::zeros(rt::MAX_ELEMENTS + 1, 0).is_err());
}
#[test]
fn characters_formatting_concatenation_and_builtins() {
    let hi = "ab".to_string();
    let bye = "cd".to_string();
    let chars: ArrayD<u8> = rt::concatenate(&[&[&hi], &[&bye]]).unwrap();
    assert_eq!(chars, array![[b'a', b'b'], [b'c', b'd']].into_dyn());
    let row: String =
        rt::index(&chars, &[rt::Index::values(&2.).unwrap(), rt::Index::All]).unwrap();
    assert_eq!(row, "cd");
    assert!(rt::convert::<String>(&chars).is_err());
    let text: String = rt::call("sprintf", &[&"%s: %.2f".to_string(), &hi, &1.25]).unwrap();
    assert_eq!(text, "ab: 1.25");
    let a = rt::range(1., 1., 4.).unwrap();
    assert_eq!(rt::call::<f64>("sum", &[&a]).unwrap(), 10.);
    let shape = rt::call_outputs::<f64>("size", &[&chars], 2).unwrap();
    assert_eq!(shape, vec![2., 2.]);
    let cols = rt::columns::<ArrayD<u8>>(&chars).unwrap();
    assert_eq!(cols.len(), 2);
    assert_eq!(cols[0], array![[b'a'], [b'c']].into_dyn());
}

#[test]
fn ndarray_storage_stays_column_major_through_shape_changes_and_mapping() {
    let a = array![[1., 2., 3.], [4., 5., 6.]].into_dyn();
    let reshaped: ArrayD<f64> = rt::call("reshape", &[&a, &3., &2.]).unwrap();
    matrix(&reshaped, 3, 2, &[1., 4., 2., 5., 3., 6.]);
    let negative: ArrayD<f64> = rt::unary("-", &reshaped).unwrap();
    matrix(&negative, 3, 2, &[-1., -4., -2., -5., -3., -6.]);
    let logical: ArrayD<bool> = rt::unary("~", &negative).unwrap();
    assert_eq!(logical.shape(), &[3, 2]);
    assert!(logical.iter().all(|v| !*v));
    let identity: ArrayD<f64> = rt::call("eye", &[&2.]).unwrap();
    matrix(
        &rt::mtimes(&negative, &identity).unwrap(),
        3,
        2,
        &[-1., -4., -2., -5., -3., -6.],
    );
    let doubled: ArrayD<f64> = rt::concatenate(&[&[&a, &a], &[&a, &a]]).unwrap();
    matrix(
        &doubled,
        4,
        6,
        &[
            1., 4., 1., 4., 2., 5., 2., 5., 3., 6., 3., 6., 1., 4., 1., 4., 2., 5., 2., 5., 3., 6.,
            3., 6.,
        ],
    );
}

#[test]
fn for_columns_does_not_iterate_shaped_empty_arrays() {
    for shape in [(0, 3), (2, 0), (0, 0)] {
        let a = rt::zeros(shape.0, shape.1).unwrap();
        assert!(rt::columns::<ArrayD<f64>>(&a).unwrap().is_empty());
    }
}
