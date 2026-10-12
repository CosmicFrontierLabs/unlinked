//! A separately loaded reference trace; its own time grid is never resampled.
use shared::{SimulationResult, SimulationTrace};
use uuid::Uuid;

pub(super) struct ReferenceTrace {
    pub id: Uuid,
    pub version: i32,
    pub trace: SimulationTrace,
}
impl ReferenceTrace {
    pub fn from_result(result: SimulationResult) -> Result<Self, String> {
        let trace = result
            .trace
            .filter(|t| !t.time.is_empty())
            .ok_or("This run has no recorded samples to compare.")?;
        if trace.signals.values().all(Vec::is_empty) {
            return Err("This run has no recorded signals to compare.".into());
        }
        Ok(Self {
            id: result.run.id,
            version: result.run.file_version,
            trace,
        })
    }
    /// Match recorded IDs, not display names (names can repeat or change).
    pub fn column(&self, id: &str) -> Option<(&[f64], &[f64])> {
        let values = self.trace.signals.get(id)?;
        let n = self.trace.time.len().min(values.len());
        (n > 0).then_some((&self.trace.time[..n], &values[..n]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn matches_ids_and_preserves_independent_time_grid() {
        let reference = ReferenceTrace {
            id: Uuid::nil(),
            version: 2,
            trace: SimulationTrace {
                solver: shared::Solver::Rk4,
                time: vec![0.0, 0.3, 0.9],
                signals: [("1".into(), vec![2.0, 4.0]), ("2".into(), vec![])]
                    .into_iter()
                    .collect(),
            },
        };
        let (time, values) = reference.column("1").unwrap();
        assert_eq!(time, [0.0, 0.3]);
        assert_eq!(values, [2.0, 4.0]);
        assert!(reference.column("2").is_none());
        assert!(reference.column("missing").is_none());
    }
}
