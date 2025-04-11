use boom::{
    alert::{AlertWorker, ZtfAlertWorker},
    conf,
    utils::{
        db::mongify,
        testing::{drop_alert_from_collections, AlertRandomizerTrait, ZtfAlertRandomizer},
    },
};
use mongodb::bson::doc;

const CONFIG_FILE: &str = "tests/config.test.yaml";

#[tokio::test]
async fn test_alert_from_avro_bytes() {
    let mut alert_worker = ZtfAlertWorker::new(CONFIG_FILE).await.unwrap();

    let (candid, object_id, ra, dec, bytes_content) = ZtfAlertRandomizer::default().get();
    let alert = alert_worker.alert_from_avro_bytes(&bytes_content).await;
    assert!(alert.is_ok());

    // validate the alert
    let mut alert = alert.unwrap();
    assert_eq!(alert.schemavsn, "4.02");
    assert_eq!(alert.publisher, "ZTF (www.ztf.caltech.edu)");
    assert_eq!(alert.object_id, object_id);
    assert_eq!(alert.candid, candid);

    // validate the candidate
    let candidate = alert.clone().candidate;
    assert_eq!(candidate.ra, ra);
    assert_eq!(candidate.dec, dec);

    // validate the prv_candidates
    let prv_candidates = alert.clone().prv_candidates;
    assert!(!prv_candidates.is_none());

    let prv_candidates = prv_candidates.unwrap();
    assert_eq!(prv_candidates.len(), 10);

    let non_detection = prv_candidates.get(0).unwrap();
    assert_eq!(non_detection.magpsf.is_none(), true);
    assert_eq!(!non_detection.diffmaglim.is_none(), true);

    let detection = prv_candidates.get(1).unwrap();
    assert_eq!(detection.magpsf.is_some(), true);
    assert_eq!(detection.sigmapsf.is_some(), true);
    assert_eq!(detection.diffmaglim.is_some(), true);
    assert_eq!(detection.isdiffpos.is_some(), true);

    // validate the fp_hists
    let fp_hists = alert.clone().fp_hists;
    assert!(!fp_hists.is_none());

    let fp_hists = fp_hists.unwrap();
    assert_eq!(fp_hists.len(), 10);

    // at the moment, negative fluxes yield non-detections
    // this is a conscious choice, might be revisited in the future
    let fp_negative_det = fp_hists.get(0).unwrap();
    assert!(fp_negative_det.magpsf.is_none());
    assert!(fp_negative_det.sigmapsf.is_none());
    assert!((fp_negative_det.diffmaglim - 20.879942).abs() < 1e-6);
    assert!(fp_negative_det.isdiffpos.is_none());
    assert!(fp_negative_det.snr.is_none());
    assert!((fp_negative_det.fp_hist.jd - 2460447.9202778).abs() < 1e-6);

    let fp_positive_det = fp_hists.get(9).unwrap();
    assert!((fp_positive_det.magpsf.unwrap() - 20.801506).abs() < 1e-6);
    assert!((fp_positive_det.sigmapsf.unwrap() - 0.3616859).abs() < 1e-6);
    assert!((fp_positive_det.diffmaglim - 20.247562).abs() < 1e-6);
    assert_eq!(fp_positive_det.isdiffpos.is_some(), true);
    assert!((fp_positive_det.snr.unwrap() - 3.0018756).abs() < 1e-6);
    assert!((fp_positive_det.fp_hist.jd - 2460420.9637616).abs() < 1e-6);

    // validate the cutouts
    assert_eq!(alert.cutout_science.clone().unwrap().len(), 13107);
    assert_eq!(alert.cutout_template.clone().unwrap().len(), 12410);
    assert_eq!(alert.cutout_difference.clone().unwrap().len(), 14878);

    let prv_candidates = alert.prv_candidates.take();
    let fp_hist = alert.fp_hists.take();

    // validate the prv_candidates
    assert!(!prv_candidates.is_none());
    assert_eq!(prv_candidates.clone().unwrap().len(), 10);

    // validate the fp_hist
    assert!(!fp_hist.is_none());
    assert_eq!(fp_hist.clone().unwrap().len(), 10);

    // validate the conversion to bson
    let alert_doc = mongify(&alert);
    assert_eq!(alert_doc.get_str("schemavsn").unwrap(), "4.02");
    assert_eq!(
        alert_doc.get_str("publisher").unwrap(),
        "ZTF (www.ztf.caltech.edu)"
    );
    assert_eq!(alert_doc.get_str("objectId").unwrap(), object_id);
    assert_eq!(alert_doc.get_i64("candid").unwrap(), candid);
    assert_eq!(
        alert_doc
            .get_document("candidate")
            .unwrap()
            .get_f64("ra")
            .unwrap(),
        ra
    );
    assert_eq!(
        alert_doc
            .get_document("candidate")
            .unwrap()
            .get_f64("dec")
            .unwrap(),
        dec
    );

    // validate the conversion to bson for prv_candidates
    let prv_candidates_doc = prv_candidates
        .unwrap()
        .into_iter()
        .map(|x| mongify(&x))
        .collect::<Vec<_>>();
    assert_eq!(prv_candidates_doc.len(), 10);

    let non_detection = prv_candidates_doc.get(0).unwrap();
    assert!(!non_detection.get_f64("magpsf").is_ok());

    let detection = prv_candidates_doc.get(1).unwrap();
    assert_eq!(detection.get_f64("magpsf").unwrap(), 16.800199508666992);

    // validate the conversion to bson for fp_hist
    let fp_hist_doc = fp_hist
        .unwrap()
        .into_iter()
        .map(|x| mongify(&x))
        .collect::<Vec<_>>();
    assert_eq!(fp_hist_doc.len(), 10);

    let fp_negative_flux = fp_hist_doc.get(0).unwrap();
    assert_eq!(
        fp_negative_flux.get_f64("forcediffimflux").unwrap(),
        -11859.8798828125
    );

    let fp_positive_flux = fp_hist_doc.get(9).unwrap();
    assert_eq!(
        fp_positive_flux.get_f64("forcediffimflux").unwrap(),
        138.2030029296875
    );
}

#[tokio::test]
async fn test_process_ztf_alert() {
    let mut alert_worker = ZtfAlertWorker::new(CONFIG_FILE).await.unwrap();

    let (candid, object_id, ra, dec, bytes_content) = ZtfAlertRandomizer::default().get();
    let result = alert_worker.process_alert(&bytes_content).await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), candid);

    // now that it has been inserted in the database, calling process alert should return an error
    let result = alert_worker.process_alert(&bytes_content).await;

    assert!(result.is_err());

    // let's query the database to check if the alert was inserted
    let config_file = conf::load_config(CONFIG_FILE).unwrap();
    let db = conf::build_db(&config_file).await.unwrap();
    let alert_collection_name = "ZTF_alerts";
    let filter = doc! {"_id": candid};

    let alert = db
        .collection::<mongodb::bson::Document>(alert_collection_name)
        .find_one(filter.clone())
        .await
        .unwrap();
    assert!(alert.is_some());
    let alert = alert.unwrap();
    assert_eq!(alert.get_i64("_id").unwrap(), candid);
    assert_eq!(alert.get_str("objectId").unwrap(), object_id);
    let candidate = alert.get_document("candidate").unwrap();
    assert_eq!(candidate.get_f64("ra").unwrap(), ra);
    assert_eq!(candidate.get_f64("dec").unwrap(), dec);

    // check that the cutouts were inserted
    let cutout_collection_name = "ZTF_alerts_cutouts";
    let cutouts = db
        .collection::<mongodb::bson::Document>(cutout_collection_name)
        .find_one(filter.clone())
        .await
        .unwrap();
    assert!(cutouts.is_some());
    let cutouts = cutouts.unwrap();
    assert_eq!(cutouts.get_i64("_id").unwrap(), candid);
    assert!(cutouts.contains_key("cutoutScience"));
    assert!(cutouts.contains_key("cutoutTemplate"));
    assert!(cutouts.contains_key("cutoutDifference"));

    // check that the aux collection was inserted
    let aux_collection_name = "ZTF_alerts_aux";
    let filter_aux = doc! {"_id": &object_id};
    let aux = db
        .collection::<mongodb::bson::Document>(aux_collection_name)
        .find_one(filter_aux.clone())
        .await
        .unwrap();

    assert!(aux.is_some());
    let aux = aux.unwrap();
    assert_eq!(aux.get_str("_id").unwrap(), &object_id);
    // check that we have the arrays prv_candidates, prv_nondetections and fp_hists
    let prv_candidates = aux.get_array("prv_candidates").unwrap();
    assert_eq!(prv_candidates.len(), 8);

    let prv_nondetections = aux.get_array("prv_nondetections").unwrap();
    assert_eq!(prv_nondetections.len(), 3);

    let fp_hists = aux.get_array("fp_hists").unwrap();
    assert_eq!(fp_hists.len(), 10);

    drop_alert_from_collections(candid, "ZTF").await.unwrap();
}
