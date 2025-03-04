use crate::{
    alert::{run_alert_worker, LsstAlertWorker, ZtfAlertWorker},
    filter::{run_filter_worker, LsstFilterWorker, ZtfFilterWorker},
    ml::run_ml_worker,
    utils::worker::{WorkerCmd, WorkerType},
};
use std::thread;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::SendError;
use tracing::{error, info, warn};

// Thread pool
// allows spawning, killing, and managing of various worker threads through
// the use of a messages
pub struct ThreadPool {
    worker_type: WorkerType,
    stream_name: String,
    config_path: String,
    workers: Vec<Worker>,
}

/// Threadpool
///
/// The threadpool manages an array of workers of one type
impl ThreadPool {
    /// Create a new threadpool
    ///
    /// worker_type: a `WorkerType` enum to designate which type of workers this threadpool contains
    /// size: number of workers initially inside of threadpool
    /// stream_name: source stream. e.g. 'ZTF'
    /// config_path: path to config file
    pub fn new(
        worker_type: WorkerType,
        size: usize,
        stream_name: String,
        config_path: String,
    ) -> Self {
        let mut thread_pool = ThreadPool {
            worker_type,
            stream_name,
            config_path,
            workers: Vec::new(),
        };
        for _ in 0..size {
            thread_pool.add_worker();
        }
        thread_pool
    }

    /// Send a termination signal to each worker thread.
    async fn terminate(&self) {
        for worker in &self.workers {
            info!("sending termination signal to worker {}", &worker.id);
            if let Err(error) = worker.terminate().await {
                warn!(
                    error = %error,
                    "failed to send termination signal to worker {}",
                    &worker.id
                );
            }
        }
    }

    /// Join all worker threads in the pool.
    fn join(&mut self) {
        for worker in &mut self.workers {
            if let Some(thread) = worker.thread.take() {
                match thread.join() {
                    Ok(_) => info!("successfully shut down worker {}", &worker.id),
                    Err(error) => {
                        error!(error = ?error, "failed to shut down worker {}", &worker.id)
                    }
                }
            }
        }
    }

    /// Add a new worker to the thread pool
    fn add_worker(&mut self) {
        let id = uuid::Uuid::new_v4().to_string();
        info!("adding worker with id {}", id);
        self.workers.push(Worker::new(
            self.worker_type,
            id.clone(),
            self.stream_name.clone(),
            self.config_path.clone(),
        ));
    }
}

// Shut down all workers from the thread pool and drop the threadpool
impl Drop for ThreadPool {
    fn drop(&mut self) {
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(self.terminate());
        self.join();
    }
}

/// Worker Struct
/// The `worker` struct represents a threaded worker which might serve as
/// one of several possible roles in the processing pipeline. A `worker` is
/// controlled completely by a threadpool and has a listening channel through
/// which it listens for commands from it.
pub struct Worker {
    id: String,
    thread: Option<thread::JoinHandle<()>>,
    sender: mpsc::Sender<WorkerCmd>,
}

impl Worker {
    /// Create a new pipeline worker
    ///
    /// worker_type: an instance of enum `WorkerType`
    /// id: unique string identifier
    /// receiver: receiver by which the owning threadpool communicates with the worker
    /// stream_name: name of the stream worker from. e.g. 'ZTF' or 'WINTER'
    /// config_path: path to the config file we are working with
    fn new(
        worker_type: WorkerType,
        id: String,
        stream_name: String,
        config_path: String,
    ) -> Worker {
        let id_copy = id.clone();
        let (sender, receiver) = mpsc::channel(1);
        let thread = match worker_type {
            // TODO: Spawn a new worker thread when one dies? (A supervisor or something like that?)
            WorkerType::Alert => thread::spawn(move || {
                if stream_name == "ZTF" {
                    let _ = run_alert_worker::<ZtfAlertWorker>(id, receiver, &config_path);
                } else if stream_name == "LSST" {
                    let _ = run_alert_worker::<LsstAlertWorker>(id, receiver, &config_path);
                } else {
                    panic!("Unknown stream name: {}", stream_name);
                }
            }),
            WorkerType::Filter => thread::spawn(move || {
                if stream_name == "ZTF" {
                    let _ = run_filter_worker::<ZtfFilterWorker>(id, receiver, &config_path);
                } else if stream_name == "LSST" {
                    let _ = run_filter_worker::<LsstFilterWorker>(id, receiver, &config_path);
                } else {
                    panic!("Unknown stream name: {}", stream_name);
                }
            }),
            WorkerType::ML => thread::spawn(move || {
                run_ml_worker(id, receiver, stream_name, config_path);
            }),
        };

        Worker {
            id: id_copy,
            thread: Some(thread),
            sender,
        }
    }

    /// Send a termination signal to the worker's thread.
    async fn terminate(&self) -> Result<(), SendError<WorkerCmd>> {
        self.sender.send(WorkerCmd::TERM).await
    }
}
