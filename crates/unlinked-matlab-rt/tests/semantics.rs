use ndarray::{ArrayD, IxDyn, array};
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
    let back: ArrayD<f64> = rt::transpose(&transposed).unwrap();
    matrix(&back, 2, 3, &[1., 4., 2., 5., 3., 6.]);
}
#[test]
fn nalgebra_solve_inverse_determinant_and_matrix_power() {
    let a = array![[0., 2.], [1., 3.]].into_dyn(); // requires pivot
    let b = array![[4., 10.], [7., 17.]].into_dyn();
    let solution: ArrayD<f64> = rt::mldivide(&a, &b).unwrap();
    matrix(&solution, 2, 2, &[1., 2., 2., 5.]);
    assert_eq!(rt::det::<f64>((&a,)).unwrap(), -2.);
    let inv: ArrayD<f64> = rt::inv((&a,)).unwrap();
    matrix(&rt::mtimes(&a, &inv).unwrap(), 2, 2, &[1., 0., 0., 1.]);
    let squared: ArrayD<f64> = rt::mpower(&a, &2.).unwrap();
    matrix(&squared, 2, 2, &[2., 3., 6., 11.]);
    let singular = array![[1., 2.], [2., 4.]].into_dyn();
    assert!(rt::mldivide::<ArrayD<f64>>(&singular, &b).is_err());
    let rectangular = rt::array(2, 3, vec![1.; 6]).unwrap();
    assert!(rt::inv::<ArrayD<f64>>((&rectangular,)).is_err());
    let empty = rt::zeros::<ArrayD<f64>>((&0., &0.)).unwrap();
    assert_eq!(rt::det::<f64>((&empty,)).unwrap(), 1.);
    matrix(&rt::inv::<ArrayD<f64>>((&empty,)).unwrap(), 0, 0, &[]);
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
        &rt::add::<ArrayD<f64>>(&a, &b).unwrap(),
        2,
        3,
        &[11., 12., 21., 22., 31., 32.],
    );
    let logical: ArrayD<bool> = rt::gt(&a, &1.).unwrap();
    assert_eq!(logical, array![[false], [true]].into_dyn());
    assert!(!rt::truth(&logical).unwrap());
    assert!(rt::scalar_truth(&logical).is_err());
    assert!(rt::convert::<bool>(&f64::NAN).is_err());
    let empty = rt::zeros::<ArrayD<f64>>((&2., &0.)).unwrap();
    matrix(&empty, 2, 0, &[]);
    assert!(rt::isempty::<bool>((&empty,)).unwrap());
    let mut empty = empty;
    assert!(rt::assign(&mut empty, &[rt::Index::values(&3.).unwrap()], &1.).is_err());
    let mut dimensionless = rt::zeros::<ArrayD<f64>>((&0., &0.)).unwrap();
    rt::assign(&mut dimensionless, &[rt::Index::values(&3.).unwrap()], &1.).unwrap();
    matrix(&dimensionless, 1, 3, &[0., 0., 1.]);
    let rank3 = ArrayD::<f64>::zeros(IxDyn(&[1, 2, 3]));
    assert!(rt::convert::<ArrayD<f64>>(&rank3).is_err());
    assert!(rt::zeros::<ArrayD<f64>>((&f64::INFINITY, &0.)).is_err());
}
#[test]
fn characters_formatting_concatenation_and_builtins() {
    let hi = "ab".to_string();
    let bye = "cd".to_string();
    let chars: ArrayD<u8> = rt::concat(((&hi,), (&bye,))).unwrap();
    assert_eq!(chars, array![[b'a', b'b'], [b'c', b'd']].into_dyn());
    let row: String =
        rt::index(&chars, &[rt::Index::values(&2.).unwrap(), rt::Index::All]).unwrap();
    assert_eq!(row, "cd");
    assert!(rt::convert::<String>(&chars).is_err());
    let text: String = rt::sprintf((&"%s: %.2f".to_string(), &hi, &1.25)).unwrap();
    assert_eq!(text, "ab: 1.25");
    let a = rt::range(1., 1., 4.).unwrap();
    assert_eq!(rt::sum::<f64>((&a,)).unwrap(), 10.);
    let shape = rt::size_outputs::<f64>((&chars,), 2).unwrap();
    assert_eq!(shape, vec![2., 2.]);
    let cols = rt::columns::<ArrayD<u8>>(&chars).unwrap();
    assert_eq!(cols.len(), 2);
    assert_eq!(cols[0], array![[b'a'], [b'c']].into_dyn());
}

#[test]
fn ndarray_storage_stays_column_major_through_shape_changes_and_mapping() {
    let a = array![[1., 2., 3.], [4., 5., 6.]].into_dyn();
    let reshaped: ArrayD<f64> = rt::reshape((&a, &3., &2.)).unwrap();
    matrix(&reshaped, 3, 2, &[1., 4., 2., 5., 3., 6.]);
    let negative: ArrayD<f64> = rt::negative(&reshaped).unwrap();
    matrix(&negative, 3, 2, &[-1., -4., -2., -5., -3., -6.]);
    let logical: ArrayD<bool> = rt::not(&negative).unwrap();
    assert_eq!(logical.shape(), &[3, 2]);
    assert!(logical.iter().all(|v| !*v));
    let identity: ArrayD<f64> = rt::eye((&2.,)).unwrap();
    matrix(
        &rt::mtimes(&negative, &identity).unwrap(),
        3,
        2,
        &[-1., -4., -2., -5., -3., -6.],
    );
    let doubled: ArrayD<f64> = rt::concat(((&a, &a), (&a, &a))).unwrap();
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
        let a = rt::zeros::<ArrayD<f64>>((&(shape.0 as f64), &(shape.1 as f64))).unwrap();
        assert!(rt::columns::<ArrayD<f64>>(&a).unwrap().is_empty());
    }
}

#[test]
fn scalar_assignments_are_in_place_and_scalar_indices_do_not_clone_arrays() {
    let mut values = rt::zeros::<ArrayD<f64>>((&1., &20000.)).unwrap();
    let initial = values.as_ptr();
    let mut sum = 0.;
    for i in 1..=20000 {
        let index = rt::Index::values(&(i as f64)).unwrap();
        rt::assign(&mut values, std::slice::from_ref(&index), &(i as f64)).unwrap();
        assert_eq!(initial, values.as_ptr());
        sum += rt::index::<f64>(&values, &[index]).unwrap();
    }
    assert_eq!(sum, 200010000.);
}

#[test]
fn trusted_helpers_have_no_interpreter_work_or_element_caps() {
    let big = rt::ones::<ArrayD<f64>>((&1., &2000000.)).unwrap();
    assert_eq!(big.len(), 2000000);
    let a = rt::ones::<ArrayD<f64>>((&250., &250.)).unwrap();
    let p: ArrayD<f64> = rt::mtimes(&a, &a).unwrap(); // 15.6M multiply-adds
    assert!(p.iter().all(|x| *x == 250.));
    let identity = rt::eye::<ArrayD<f64>>((&220.,)).unwrap();
    let inverse: ArrayD<f64> = rt::inv((&identity,)).unwrap();
    assert_eq!(inverse, identity);
    let power: ArrayD<f64> = rt::mpower(&identity, &2048.).unwrap();
    assert_eq!(power, identity);
    let long: String = rt::sprintf((&"%4000010.64f".to_string(), &1.)).unwrap();
    assert_eq!(long.len(), 4000010);
    assert!(rt::zeros::<ArrayD<f64>>((&(usize::MAX as f64), &2.)).is_err());
}

#[test]
fn lazy_ranges_match_materialized_ranges_and_keep_shape_errors_typed() {
    for (a, step, b) in [(0., 0.1, 0.3), (3., -0.5, 0.), (4., 1., 2.)] {
        let eager = rt::range(a, step, b).unwrap();
        let lazy = rt::range_iter(a, step, b).unwrap().collect::<Vec<_>>();
        assert_eq!(lazy, rt::Matlab::matlab_column_major(&eager).unwrap());
    }
    let mut large = rt::range_iter(1., 1., 1000000000.).unwrap();
    assert_eq!(large.next(), Some(1.));
    assert_eq!(large.len(), 999999999);
    assert_eq!(rt::range_iter(0., 0., 1.).unwrap().len(), 0);
    let rank3 = ArrayD::<f64>::zeros(IxDyn(&[1, 1, 1]));
    assert!(matches!(
        rt::convert::<ArrayD<f64>>(&rank3),
        Err(rt::Error::UnsupportedRank { rank: 3 })
    ));
    assert_eq!(
        rt::Error::undefined("x").to_string(),
        "undefined variable 'x'"
    );
}

#[test]
fn colon_reaches_only_on_grid_endpoints_exactly() {
    for (start, step, stop) in [
        (0., 0.1, 0.3),
        (0.3, -0.1, 0.),
        (-0.3, 0.1, 0.),
        (0., -0.1, -0.3),
    ] {
        let lazy = rt::range_iter(start, step, stop)
            .unwrap()
            .collect::<Vec<_>>();
        let eager = rt::range(start, step, stop).unwrap();
        assert_eq!(lazy.first(), Some(&start));
        assert_eq!(lazy.last(), Some(&stop));
        assert_eq!(lazy, rt::Matlab::matlab_column_major(&eager).unwrap());
    }
    let positive = rt::range_iter(0., 0.1, 0.35).unwrap().collect::<Vec<_>>();
    assert_eq!(positive.len(), 4);
    assert_eq!(positive[3], 3.0 * 0.1);
    assert_ne!(positive[3], 0.35);
    let negative = rt::range_iter(0., -0.1, -0.35).unwrap().collect::<Vec<_>>();
    assert_eq!(negative.len(), 4);
    assert_eq!(negative[3], 3.0 * -0.1);
    assert_ne!(negative[3], -0.35);
}
