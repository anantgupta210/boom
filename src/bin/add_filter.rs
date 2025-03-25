use clap::Parser;
use mongodb::bson::doc;
use tracing::{error, Level};
use tracing_subscriber::FmtSubscriber;

use boom::{conf, utils::db::create_index};

#[derive(Parser)]
struct Cli {
    #[arg(help = "Survey to add a filter for. Options are 'ZTF' or 'LSST'")]
    survey: String,
    #[arg(help = "Filter ID to add")]
    filter_id: i32,
    #[arg(help = "Path to the JSON file containing the filter")]
    filter_file: String,
}

#[tokio::main]
async fn main() {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    let args = Cli::parse();
    let survey = args.survey;
    let filter_id = args.filter_id;
    let filter_file = args.filter_file;

    // read the JSON as a string
    let filter_pipeline = match std::fs::read_to_string(&filter_file) {
        Ok(filter) => filter,
        Err(e) => {
            eprintln!("Error reading filter file: {}", e);
            std::process::exit(1);
        }
    };

    // create a bson document with filter_id, active, catalog, permissions
    // group_id, and a fv array with one doc that has a fid field and a pipeline field

    let filter = doc! {
        "filter_id": filter_id,
        "active": true,
        "catalog": format!("{}_alerts", survey),
        "permissions": [1,2,3],
        "group_id": 41,
        "fv": [
            {
                "fid": "v2e0fs",
                "pipeline": filter_pipeline
            }
        ],
        "active_fid": "v2e0fs",
    };

    // insert the filter into the database
    let config_file = conf::load_config("config.yaml").unwrap();
    let db = conf::build_db(&config_file).await.unwrap();
    let collection = db.collection::<mongodb::bson::Document>("filters");

    // add a unique index on filter_id
    create_index(&collection, doc! {"filter_id": 1}, true)
        .await
        .unwrap();

    let x = collection.insert_one(filter).await;

    match x {
        Err(e) => {
            error!("error inserting filter obj: {}", e);
        }
        _ => {}
    }
}
