//! Keep numeric drafts as text so an empty or incomplete edit cannot silently
//! submit the previous numeric value. Only a validated draft becomes a request.
use shared::{SimulationOptions, Solver, MAX_SIMULATION_SAMPLES};
use std::collections::BTreeMap;

#[derive(Clone, PartialEq)]
pub struct RunForm {
    pub start: String,
    pub stop: String,
    pub step: String,
    pub relative_tolerance: String,
    pub absolute_tolerance: String,
    pub solver: Solver,
}

pub type Errors = BTreeMap<&'static str, String>;

impl From<SimulationOptions> for RunForm {
    fn from(o: SimulationOptions) -> Self {
        Self {
            start: o.start.to_string(),
            stop: o.stop.to_string(),
            step: o.step.to_string(),
            relative_tolerance: o.relative_tolerance.to_string(),
            absolute_tolerance: o.absolute_tolerance.to_string(),
            solver: o.solver,
        }
    }
}

impl RunForm {
    pub fn validate(&self) -> Result<(SimulationOptions, usize), Errors> {
        let mut errors = Errors::new();
        let mut number =
            |key, text: &str| match text.trim().parse::<f64>().ok().filter(|x| x.is_finite()) {
                Some(value) => value,
                None => {
                    errors.insert(key, "Enter a finite number.".to_string());
                    f64::NAN
                }
            };
        let mut options = SimulationOptions {
            start: number("start", &self.start),
            stop: number("stop", &self.stop),
            step: number("step", &self.step),
            solver: self.solver,
            max_samples: MAX_SIMULATION_SAMPLES,
            ..Default::default()
        };
        if self.solver == Solver::Rk45 {
            options.relative_tolerance = number("rtol", &self.relative_tolerance);
            options.absolute_tolerance = number("atol", &self.absolute_tolerance);
        }
        if options.stop < options.start {
            errors.insert("stop", "Stop time must be at least the start time.".into());
        }
        if options.step <= 0.0 {
            errors.insert("step", "Output step must be greater than zero.".into());
        }
        if self.solver == Solver::Rk45 {
            if options.relative_tolerance <= 0.0 || options.relative_tolerance > 1.0 {
                errors.insert(
                    "rtol",
                    "Use a tolerance greater than zero and at most 1.".into(),
                );
            }
            if options.absolute_tolerance <= 0.0 {
                errors.insert(
                    "atol",
                    "Absolute tolerance must be greater than zero.".into(),
                );
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        match unlinked_sim::sample_count(&options) {
            Ok(samples) => Ok((options, samples)),
            Err(_) => {
                errors.insert("step", format!("This run exceeds the {MAX_SIMULATION_SAMPLES} sample limit. Increase the output step or shorten the time range."));
                Err(errors)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_drafts_never_reuse_previous_numbers() {
        let mut form = RunForm::from(SimulationOptions::default());
        for draft in ["", "-", "1e", "NaN", "inf"] {
            form.stop = draft.into();
            assert!(form.validate().unwrap_err().contains_key("stop"), "{draft}");
        }
        form.stop = "1e1".into();
        assert_eq!(form.validate().unwrap().0.stop, 10.0);
    }

    #[test]
    fn validates_ranges_and_server_sample_budget() {
        let mut form = RunForm::from(SimulationOptions::default());
        form.step = "0".into();
        assert!(form.validate().unwrap_err().contains_key("step"));
        form.step = "0.00001".into();
        assert!(form.validate().unwrap_err()["step"].contains("sample limit"));
        form.step = "0.01".into();
        form.stop = "-1".into();
        assert!(form.validate().unwrap_err().contains_key("stop"));
        form.stop = "0".into();
        assert_eq!(form.validate().unwrap().1, 1);
        form.stop = "250".into();
        assert_eq!(form.validate().unwrap().1, MAX_SIMULATION_SAMPLES);
    }

    #[test]
    fn hidden_tolerance_drafts_do_not_block_fixed_step_solvers() {
        let mut form = RunForm::from(SimulationOptions::default());
        form.relative_tolerance = "".into();
        form.absolute_tolerance = "-1".into();
        assert!(form.validate().is_ok());
        form.solver = Solver::Rk45;
        let errors = form.validate().unwrap_err();
        assert!(errors.contains_key("rtol"));
        assert!(errors.contains_key("atol"));
    }
}
