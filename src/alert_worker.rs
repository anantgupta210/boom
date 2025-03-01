pub use crate::conf;
use crate::{
    alert,
    types::ztf_alert_schema,
    worker_util::{self, WorkerCmd},
};
use redis::AsyncCommands;
use std::sync::mpsc::{self, TryRecvError};
use tracing::{error, info, warn};

#[derive(thiserror::Error, Debug)]
pub enum AlertWorkerError {
    #[error("failed to load config")]
    LoadConfigError(#[from] conf::ConfigError),
    #[error("failed to connect to redis")]
    ConnectRedisError(#[source] redis::RedisError),
    #[error("failed to get alert schema")]
    GetAlertSchemaError,
    #[error("failed to pop from the alert queue")]
    PopAlertError(#[source] redis::RedisError),
    #[error("failed to get avro bytes from the alert queue")]
    GetAvroBytesError,
    #[error("failed to push candid onto the candid queue")]
    PushCandidError(#[source] redis::RedisError),
    #[error("failed to remove alert from the alert queue")]
    RemoveAlertError(#[source] redis::RedisError),
    #[error("failed to push alert onto the alert queue")]
    PushAlertError(#[source] redis::RedisError),
}

// alert worker as a standalone function which is run by the scheduler
#[tokio::main]
pub async fn alert_worker(
    id: &str,
    stream_name: String,
    config_path: &str,
    receiver: mpsc::Receiver<WorkerCmd>,
) -> Result<(), AlertWorkerError> {
    let config_file = conf::load_config(config_path)?;

    // XMATCH CONFIGS
    let xmatch_configs = conf::build_xmatch_configs(&config_file, &stream_name);

    // DATABASE
    let db: mongodb::Database = conf::build_db(&config_file).await;

    // create alert and alert-auxillary collections
    let alert_collection_name = format!("{}_alerts", stream_name);
    let alert_collection = db.collection(&alert_collection_name);
    let alert_aux_collection = db.collection(&format!("{}_alerts_aux", stream_name));

    // create index for alert collection
    let alert_candid_index = mongodb::IndexModel::builder()
        .keys(mongodb::bson::doc! { "candid": -1 })
        .options(
            mongodb::options::IndexOptions::builder()
                .unique(true)
                .build(),
        )
        .build();

    // TODO: Is it OK to proceed if this fails?
    if let Err(error) = alert_collection.create_index(alert_candid_index).await {
        warn!(
            error = %error,
            "Error when creating index for candidate.candid in collection {}",
            alert_collection_name,
        );
    };

    // REDIS
    let client_redis = redis::Client::open("redis://localhost:6379".to_string())
        .map_err(AlertWorkerError::ConnectRedisError)?;
    let mut con = client_redis
        .get_multiplexed_async_connection()
        .await
        .map_err(AlertWorkerError::ConnectRedisError)?;
    let queue_name = format!("{}_alerts_packets_queue", stream_name);
    let queue_temp_name = format!("{}_alerts_packets_queuetemp", stream_name);
    let classifer_queue_name = format!("{}_alerts_classifier_queue", stream_name);

    let command_interval = worker_util::get_check_command_interval(config_file, &stream_name);
    let mut command_check_countdown = command_interval;
    // ALERT SCHEMA (for fast avro decoding)
    let schema = ztf_alert_schema().ok_or(AlertWorkerError::GetAlertSchemaError)?;
    let mut count = 0;
    let start = std::time::Instant::now();
    loop {
        // check for command from threadpool
        if command_check_countdown == 0 {
            match receiver.try_recv() {
                Ok(WorkerCmd::TERM) => {
                    info!("alert worker {} received termination command", id);
                    break;
                }
                Err(TryRecvError::Disconnected) => {
                    warn!("alert worker {} receiver disconnected, terminating", id);
                    break;
                }
                Err(TryRecvError::Empty) => {
                    command_check_countdown = command_interval;
                }
            }
        }
        // retrieve candids from redis
        let Some(mut value): Option<Vec<Vec<u8>>> = con
            // TODO: If the intention is to have a reliable queue, then there
            // needs to be a separate thread that continuously monitors the temp
            // queue and adds any items it finds back to the main queue.
            // However, we're already pushing the value back onto the when we
            // handle a processing error, so do we need the temp queue?
            .rpoplpush(&queue_name, &queue_temp_name)
            .await
            .map_err(AlertWorkerError::PopAlertError)?
        else {
            info!("ALERT WORKER {}: Queue is empty", id);
            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
            command_check_countdown = 0;
            continue;
        };
        // TODO: Where does the nested vector structure come from, and are there
        // any guarantees about whether the outer vector always has exactly one
        // inner vector? There may be a way to simplify this.
        let avro_bytes = (!value.is_empty())
            .then_some(value.remove(0))
            .ok_or(AlertWorkerError::GetAvroBytesError)?;
        let result = alert::process_alert(
            &avro_bytes,
            &xmatch_configs,
            &db,
            &alert_collection,
            &alert_aux_collection,
            &schema,
        )
        .await;
        match result {
            Ok(Some(candid)) => {
                info!(
                    "Processed alert with candid: {}, queueing for classification",
                    candid
                );
                // queue the candid for processing by the classifier
                con.lpush::<&str, i64, isize>(&classifer_queue_name, candid)
                    .await
                    .map_err(AlertWorkerError::PushCandidError)?;
                // TODO: Is this correct? The value in the main queue was a nested vector.
                con.lrem::<&str, Vec<u8>, isize>(&queue_temp_name, 1, avro_bytes)
                    .await
                    .map_err(AlertWorkerError::RemoveAlertError)?;
            }
            Ok(None) => {
                info!("Alert already exists");
                // TODO: Is this correct? The value in the main queue was a nested vector.
                con.lrem::<&str, Vec<u8>, isize>(&queue_temp_name, 1, avro_bytes)
                    .await
                    .map_err(AlertWorkerError::RemoveAlertError)?;
            }
            Err(error) => {
                warn!(error = %error, "Error processing alert, requeueing");
                // put it back in the ZTF_alerts_packets_queue, to the left (pop from the right, push to the left)
                // TODO: Is this correct? The original value was a nested vector.
                con.lpush::<&str, Vec<u8>, isize>(&queue_name, avro_bytes.clone())
                    .await
                    .map_err(AlertWorkerError::PushAlertError)?;
                // TODO: Is this correct? The value in the main queue was a nested vector.
                con.lrem::<&str, Vec<u8>, isize>(&queue_temp_name, 1, avro_bytes)
                    .await
                    .map_err(AlertWorkerError::RemoveAlertError)?;
            }
        }
        if count > 1 && count % 100 == 0 {
            let elapsed = start.elapsed().as_secs();
            info!(
                "\nProcessed {} {} alerts in {} seconds, avg: {:.4} alerts/s\n",
                count,
                stream_name,
                elapsed,
                count as f64 / elapsed as f64
            );
        }
        count += 1;
        command_check_countdown -= 1;
    }
    Ok(())
}
