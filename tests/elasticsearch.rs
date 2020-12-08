use jsonpath::{Match, Step};

use json_exporter::config::Config;
use json_exporter::convert::ResolvedMetric;
use json_exporter::prepare::PreparedConfig;

use std::fs::File;
use std::io::{BufReader, BufRead};
use std::io::Write;
use std::iter::{repeat, repeat_with};

use serde_json;
use serde_yaml;

use url::Url;

const ES_INFO: &'static str = include_str!("es_info.json");
const ES_CLUSTER_HEALTH: &'static str = include_str!("es_cluster_health.json");
const ES_NODES_STATS: &'static str = include_str!("es_nodes_stats.json");
const ES_INDICES_STATS: &'static str = include_str!("es_indices_stats.json");

#[test]
fn test_elasticsearch() {
    let base_url = Url::parse("http://example.com:9200").expect("base url");
    let es_config_filename = "elasticsearch_exporter.yaml";
    let es_config_file = BufReader::new(
        File::open(es_config_filename).expect(es_config_filename)
    );
    let config: Config = serde_yaml::from_reader(es_config_file)
        .expect("es config");
    let prepared_config = PreparedConfig::create_from(
        &config, &base_url, &Default::default()
    )
        .expect("prepare es config");

    let es_info = serde_json::from_str(ES_INFO).expect("es info");
    let es_info = Match {
        value: &es_info,
        path: vec!(Step::Root),
    };
    let global_labels = prepared_config.global_labels.iter()
        .map(|global_label| {
            match global_label.url.as_str() {
                "http://example.com:9200/?" => global_label.labels.resolve(&es_info),
                labels_url => unreachable!("Global labels url: {}", labels_url),
            }
        })
        .collect::<Result<Vec<_>, _>>().expect("global labels")
        .into_iter()
        .flatten()
        .collect();

    let root_metric = ResolvedMetric::new_root(
        prepared_config.namespace.clone().unwrap_or("".to_string()),
        global_labels,
    );

    let mut buf = vec!();
    for endpoint in &prepared_config.endpoints {
        match endpoint.url.as_str() {
            "http://example.com:9200/_cluster/health?" => {
                let es_cluster_health = serde_json::from_str(ES_CLUSTER_HEALTH)
                    .expect("es cluster health");
                assert_eq!(
                    endpoint.process(
                        &root_metric, &es_cluster_health, &mut buf
                    ),
                    vec!()
                );
                buf.write_all(b"\n\n").unwrap();
            }
            "http://example.com:9200/_nodes/_local/stats?groups=_all" => {
                let es_nodes_stats = serde_json::from_str(ES_NODES_STATS)
                    .expect("es nodes stats");
                assert_eq!(
                    endpoint.process(
                        &root_metric, &es_nodes_stats, &mut buf
                    ),
                    vec!()
                );
                buf.write_all(b"\n\n").unwrap();
            }
            "http://example.com:9200/_all/_stats?groups=_all" => {
                let es_indices_stats = serde_json::from_str(ES_INDICES_STATS)
                    .expect("es indices stats");
                assert_eq!(
                    endpoint.process(
                        &root_metric, &es_indices_stats, &mut buf
                    ),
                    vec!()
                );
                buf.write_all(b"\n\n").unwrap();
            }
            endpoint_url => {
                unreachable!("Endpoint url: {}", endpoint_url);
            },
        }
    }

    // let mut es_metrics_file = File::open("tests/es_metrics_new.txt").unwrap();
    // es_metrics_file.write(&buf[..]).unwrap();
    // es_metrics_file.flush();

    let es_metrics_filename = "tests/es_metrics.txt";
    let expected_metrics = BufReader::new(
        File::open(es_metrics_filename).expect(es_metrics_filename)
    );
    let metrics = String::from_utf8(buf).expect("metrics must be utf8");

    for (line_ix, (line, expected_line)) in metrics.lines()
        .map(|l| Some(l))
        .chain(repeat(None).into_iter())
        .zip(expected_metrics.lines()
            .map(|l| Some(l))
            .chain(
            repeat_with(|| None).into_iter()
            )
        )
        .enumerate()
    {
        let (line, expected_line) = match (line, expected_line) {
            (Some(line), Some(expected_line)) => (line, expected_line),
            (Some(line), None) => (line, Ok("".to_string())),
            (None, Some(expected_line)) => ("", expected_line),
            (None, None) => break,
        };
        let expected_line = expected_line.expect("read line");
        assert_eq!(line, expected_line, "Line number: {}", line_ix + 1);
    }
}
