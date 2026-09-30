use unlinked_matlab::array_runtime::{self as rt, Index, MAX_ELEMENTS, Value};

#[test]
fn huge_empty_shapes_require_no_per_cell_or_cartesian_work() {
    let wide = Value::new(0, MAX_ELEMENTS, vec![]).unwrap();
    let tall = Value::new(MAX_ELEMENTS, 0, vec![]).unwrap();
    let result = rt::concatenate((0..8000).map(|_| vec![wide.clone()]).collect()).unwrap();
    assert_eq!(
        (result.rows, result.cols, result.data.len()),
        (0, MAX_ELEMENTS, 0)
    );
    let transposed = wide.transpose();
    assert_eq!((transposed.rows, transposed.cols), (MAX_ELEMENTS, 0));
    let sum = rt::binary("+", &wide, &Value::scalar(1.)).unwrap();
    assert_eq!((sum.rows, sum.cols), (0, MAX_ELEMENTS));
    let product = rt::binary("*", &wide, &tall).unwrap();
    assert_eq!((product.rows, product.cols), (0, 0));
    let indexed = wide.index(&[Index::All, Index::All]).unwrap();
    assert_eq!((indexed.rows, indexed.cols), (0, MAX_ELEMENTS));
    let minimum = rt::builtin("min", vec![wide.clone()], 1).unwrap().remove(0);
    assert_eq!((minimum.rows, minimum.cols), (0, MAX_ELEMENTS));
    let mut assigned = wide;
    assigned
        .assign(&[Index::All, Index::All], &Value::scalar(1.))
        .unwrap();
    assert_eq!((assigned.rows, assigned.cols), (0, MAX_ELEMENTS));
}

#[test]
fn shaped_empty_dimension_limits_still_apply() {
    let wide = Value::new(0, MAX_ELEMENTS, vec![]).unwrap();
    assert!(rt::concatenate(vec![vec![wide.clone(), wide]]).is_err());
    assert!(Value::new(MAX_ELEMENTS + 1, 0, vec![]).is_err());
}

#[test]
fn many_empty_rows_do_not_multiply_nonempty_concat_work() {
    let wide = Value::new(0, MAX_ELEMENTS, vec![]).unwrap();
    let mut rows: Vec<_> = (0..8000).map(|_| vec![wide.clone()]).collect();
    rows.push(vec![
        Value::new(1, MAX_ELEMENTS, vec![7.0; MAX_ELEMENTS]).unwrap(),
    ]);
    let result = rt::concatenate(rows).unwrap();
    assert_eq!((result.rows, result.cols), (1, MAX_ELEMENTS));
    assert!(result.data.iter().all(|v| *v == 7.0));
}
