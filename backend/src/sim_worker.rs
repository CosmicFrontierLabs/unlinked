//! Bounded simulation workers. HTTP/WS callers must authorize the immutable
//! input revision before entering this module.
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, OnceLock,
};
use tokio::sync::{mpsc, OwnedSemaphorePermit, Semaphore};
use unlinked_sim::{Options, Trace};

const MAX_CONCURRENT: usize = 4;
const MAX_SAMPLES: usize = 25_001;
const MAX_VALUES: usize = 1_000_000;
const CHUNK_SAMPLES: usize = 128;

#[derive(Debug)]
pub enum Event {
    Started {
        signals: Vec<(String, String)>,
    },
    Samples {
        time: Vec<f64>,
        values: Vec<Vec<f64>>,
    },
    Completed,
    Failed(String),
}

pub struct Worker {
    pub events: mpsc::Receiver<Event>,
    cancel: Arc<AtomicBool>,
}
impl Worker {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

fn capacity() -> Arc<Semaphore> {
    static SEMAPHORE: OnceLock<Arc<Semaphore>> = OnceLock::new();
    SEMAPHORE
        .get_or_init(|| Arc::new(Semaphore::new(MAX_CONCURRENT)))
        .clone()
}

pub fn start(
    user_id: uuid::Uuid,
    filename: String,
    bytes: Vec<u8>,
    mut options: Options,
    workspace: std::collections::BTreeMap<String, String>,
    complete: impl FnOnce(&Result<Trace, String>) -> Result<(), String> + Send + 'static,
) -> Result<Worker, String> {
    if bytes.len() > 16 * 1024 * 1024 {
        return Err("model exceeds 16 MiB upload limit".into());
    }
    options.max_samples = options.max_samples.min(MAX_SAMPLES);
    let permit = capacity()
        .try_acquire_owned()
        .map_err(|_| "simulation capacity is full; retry later".to_string())?;
    static USERS: OnceLock<
        std::sync::Mutex<std::collections::HashMap<uuid::Uuid, std::sync::Weak<Semaphore>>>,
    > = OnceLock::new();
    let user_capacity = {
        let mut users = USERS
            .get_or_init(Default::default)
            .lock()
            .map_err(|_| "worker accounting unavailable".to_string())?;
        users.retain(|_, v| v.strong_count() > 0);
        let capacity = users
            .get(&user_id)
            .and_then(std::sync::Weak::upgrade)
            .unwrap_or_else(|| Arc::new(Semaphore::new(2)));
        users.insert(user_id, Arc::downgrade(&capacity));
        capacity
    };
    let user_permit = user_capacity
        .try_acquire_owned()
        .map_err(|_| "you already have two active simulations".to_string())?;
    let cancel = Arc::new(AtomicBool::new(false));
    let cancelled = cancel.clone();
    let (tx, rx) = mpsc::channel(4);
    tokio::task::spawn_blocking(move || {
        let _permit: OwnedSemaphorePermit = permit;
        let _user_permit: OwnedSemaphorePermit = user_permit;
        let began = std::time::Instant::now();
        let send = |event| {
            tokio::runtime::Handle::current().block_on(async {
                tokio::time::timeout(std::time::Duration::from_secs(10), tx.send(event))
                    .await
                    .map_err(|_| "client stream timed out".to_string())?
                    .map_err(|_| "client disconnected".to_string())
            })
        };
        let mut result = (|| {
            let mut model =
                unlinked_import::import(&filename, &bytes).map_err(|e| e.to_string())?;
            model.workspace.extend(workspace);
            let graph = unlinked_sim::compile(&model, &options).map_err(|e| e.to_string())?;
            options.max_internal_steps = options
                .max_internal_steps
                .min(10_000)
                .min(10_000_000 / graph.nodes.len().max(1))
                .max(1);
            let samples = ((options.stop - options.start) / options.step).ceil() + 1.0;
            if !samples.is_finite()
                || samples < 1.0
                || samples * graph.nodes.len() as f64 > MAX_VALUES as f64
            {
                return Err("simulation exceeds server output budget".into());
            }
            if cancelled.load(Ordering::Relaxed) {
                return Err("simulation cancelled".into());
            }
            send(Event::Started {
                signals: graph
                    .nodes
                    .iter()
                    .map(|n| (n.id.clone(), format!("{}:1", n.name)))
                    .collect(),
            })
            .map_err(|_| "client disconnected".to_string())?;
            let mut times = Vec::with_capacity(CHUNK_SAMPLES);
            let mut values = Vec::with_capacity(CHUNK_SAMPLES);
            let trace = unlinked_sim::simulate_with_observer(&graph, &options, |sample| {
                if cancelled.load(Ordering::Relaxed)
                    || began.elapsed() > std::time::Duration::from_secs(30)
                {
                    return false;
                }
                times.push(sample.time);
                values.push(sample.values.to_vec());
                if times.len() == CHUNK_SAMPLES {
                    send(Event::Samples {
                        time: std::mem::take(&mut times),
                        values: std::mem::take(&mut values),
                    })
                    .is_ok()
                } else {
                    true
                }
            })
            .map_err(|e| e.to_string())?;
            if !times.is_empty() {
                send(Event::Samples {
                    time: times,
                    values,
                })
                .map_err(|_| "client disconnected".to_string())?;
            }
            Ok(trace)
        })();
        if let Err(error) = complete(&result) {
            result = Err(error);
        }
        let event = match result {
            Ok(_) => Event::Completed,
            Err(message) => Event::Failed(message),
        };
        let _ = send(event);
    });
    Ok(Worker { events: rx, cancel })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn streams_named_bounded_samples_and_persists_before_completion() {
        let completed = Arc::new(AtomicBool::new(false));
        let flag = completed.clone();
        let mut worker = start(
            uuid::Uuid::new_v4(),
            "scalar.mdl".into(),
            include_bytes!("../../crates/unlinked-cli/tests/fixtures/scalar.mdl").to_vec(),
            Options {
                stop: 0.2,
                step: 0.1,
                ..Default::default()
            },
            Default::default(),
            move |result| {
                assert!(result.is_ok());
                flag.store(true, Ordering::SeqCst);
                Ok(())
            },
        )
        .unwrap();
        let mut count = 0;
        while let Some(event) = worker.events.recv().await {
            match event {
                Event::Started { signals } => {
                    assert_eq!(signals.len(), 3);
                    assert!(signals.iter().all(|s| s.1.ends_with(":1")));
                }
                Event::Samples { time, values } => {
                    assert_eq!(time.len(), values.len());
                    count += time.len();
                    assert!(values.iter().all(|v| v == &vec![2.0, 6.0, 6.0]));
                }
                Event::Completed => {
                    assert!(completed.load(Ordering::SeqCst));
                }
                Event::Failed(error) => panic!("{error}"),
            }
        }
        assert_eq!(count, 3);
    }
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;
    #[tokio::test]
    async fn cancellation_persists_failure_and_releases_worker() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = cancelled.clone();
        let mut worker = start(
            uuid::Uuid::new_v4(),
            "scalar.mdl".into(),
            include_bytes!("../../crates/unlinked-cli/tests/fixtures/scalar.mdl").to_vec(),
            Options {
                stop: 2.0,
                step: 0.0001,
                ..Default::default()
            },
            Default::default(),
            move |result| {
                assert!(result.is_err());
                flag.store(true, Ordering::SeqCst);
                Ok(())
            },
        )
        .unwrap();
        worker.cancel();
        let mut terminal = false;
        while let Some(event) = worker.events.recv().await {
            if let Event::Failed(error) = event {
                assert!(error.contains("cancel"));
                terminal = true;
            }
        }
        assert!(terminal);
        assert!(cancelled.load(Ordering::SeqCst));
    }
}
