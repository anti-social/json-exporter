use jsonpath::{Found, Step};

use json_exporter::config::Config;
use json_exporter::convert::ResolvedMetric;
use json_exporter::prepare::PreparedConfig;

use std::collections::BTreeMap;
use std::fs::File;
use std::io::BufReader;
use std::io::Write;

use serde_json;
use serde_yaml;

const ES_INFO: &'static str = include_str!("es_info.json");
const ES_CLUSTER_HEALTH: &'static str = include_str!("es_cluster_health.json");
const ES_NODES_STATS: &'static str = include_str!("es_nodes_stats.json");
const ES_INDICES_STATS: &'static str = include_str!("es_indices_stats.json");
const ES_METRICS: &'static str = include_str!("es_metrics.txt");

#[test]
fn test_elasticsearch() {
    let es_config_filename = "elasticsearch_exporter.yaml";
    let es_config_file = BufReader::new(
        File::open(es_config_filename).expect(es_config_filename)
    );
    let config: Config = serde_yaml::from_reader(es_config_file)
        .expect("es config");
    let prepared_config = PreparedConfig::create_from(&config)
        .expect("prepare es config");

    let es_info = serde_json::from_str(ES_INFO).expect("es info");
    let es_info = Found {
        value: &es_info,
        path: vec!(Step::Root),
    };
    let global_labels = prepared_config.global_labels.iter()
        .map(|global_label| {
            match global_label.url.as_str() {
                "/" => global_label.labels.resolve(&es_info),
                _ => unreachable!(),
            }
        })
        .flatten()
        .collect::<BTreeMap<_, _>>();

    let root_metric = ResolvedMetric {
        metric_type: None,
        name: prepared_config.namespace.clone().unwrap_or("".to_string()),
        labels: global_labels,
    };

    let mut buf = vec!();
    for endpoint in &prepared_config.endpoints {
        match endpoint.url.as_str() {
            "/_cluster/health" => {
                let es_cluster_health = serde_json::from_str(ES_CLUSTER_HEALTH)
                    .expect("es cluster health");
                endpoint.metrics.process(&root_metric, &es_cluster_health, &mut buf);
                buf.write_all(b"\n\n").unwrap();
            }
            "/_nodes/_local/stats" => {
                let es_nodes_stats = serde_json::from_str(ES_NODES_STATS)
                    .expect("es nodes stats");
                endpoint.metrics.process(&root_metric, &es_nodes_stats, &mut buf);
                buf.write_all(b"\n\n").unwrap();
            }
            "/_all/_stats" => {
                let es_indices_stats = serde_json::from_str(ES_INDICES_STATS)
                    .expect("es indices stats");
                endpoint.metrics.process(&root_metric, &es_indices_stats, &mut buf);
                buf.write_all(b"\n\n").unwrap();
            }
            _ => {
                unreachable!();
            },
        }
    }

    // let mut es_metrics_file = File::open("tests/es_metrics_new.txt").unwrap();
    // es_metrics_file.write(&buf[..]).unwrap();
    // es_metrics_file.flush();

    assert_eq!(&buf[..], ES_METRICS.as_bytes());
}