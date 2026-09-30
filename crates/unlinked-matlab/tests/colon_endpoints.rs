use unlinked_matlab::array_runtime::{Value, range};
#[test]
fn bounded_colon_preserves_reachable_endpoint_without_snapping_off_grid() {
    for (start, step, stop) in [
        (0., 0.1, 0.3),
        (0.3, -0.1, 0.),
        (-0.3, 0.1, 0.),
        (0., -0.1, -0.3),
    ] {
        let values = range(
            &Value::scalar(start),
            &Value::scalar(step),
            &Value::scalar(stop),
        )
        .unwrap();
        assert_eq!(values.data.first(), Some(&start));
        assert_eq!(values.data.last(), Some(&stop));
    }
    for step in [0.1, -0.1] {
        let stop = if step > 0.0 { 0.35 } else { -0.35 };
        let values = range(
            &Value::scalar(0.),
            &Value::scalar(step),
            &Value::scalar(stop),
        )
        .unwrap();
        assert_eq!(values.data.len(), 4);
        assert_eq!(values.data[3], 3.0 * step);
        assert_ne!(values.data[3], stop);
    }
    assert!(
        range(
            &Value::scalar(0.),
            &Value::scalar(1.),
            &Value::scalar(2_000_000.)
        )
        .is_err()
    );
}
