use crate::agent::working_indicator::*;

#[test]
fn pulse_visits_each_dot_in_order() {
    for active_index in 0..DOT_COUNT {
        let delta = active_index as f32 / DOT_COUNT as f32;

        for index in 0..DOT_COUNT {
            let pulse = dot_pulse(delta, index);
            if index == active_index {
                assert!((pulse - 1.0).abs() < 0.001);
            } else {
                assert!(pulse < 0.001);
            }
        }
    }
}

#[test]
fn pulse_remains_in_unit_range() {
    for step in 0..=100 {
        let delta = step as f32 / 100.0;
        for index in 0..DOT_COUNT {
            assert!((0.0..=1.0).contains(&dot_pulse(delta, index)));
        }
    }
}
