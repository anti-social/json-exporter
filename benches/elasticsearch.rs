#![feature(test)]

use jsonpath::{Match, Step};

use json_exporter::config::Config;
use json_exporter::convert::ResolvedMetric;
use json_exporter::prepare::PreparedConfig;

use mimalloc::MiMalloc;

use serde_json;
use serde_yaml;

use std::fs::File;
use std::io::BufReader;
use std::io::Write;

extern crate test;
use test::Bencher;

use url::Url;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

const ES_INFO: &'static str = "tests/es_info.json";
const ES_CLUSTER_HEALTH: &'static str = "tests/es_cluster_health.json";
const ES_NODES_STATS: &'static str = "benches/es_nodes_stats.json";
const ES_INDICES_STATS: &'static str = "benches/es_indices_stats.json";
// const ES_INDICES_STATS: &'static str = "benches/es_indices_shards_stats.json";

#[bench]
fn bench_elasticsearch(b: &mut Bencher) {
    let es_url = Url::parse("http://example.com:9200").expect("es url");
    let es_config_filename = "elasticsearch_exporter.yaml";
    let es_config_file = BufReader::new(
        File::open(es_config_filename).expect(es_config_filename)
    );
    let config: Config = serde_yaml::from_reader(es_config_file)
        .expect("es config");
    let prepared_config = PreparedConfig::create_from(&config, &es_url, &Default::default())
        .expect("prepare es config");

    let es_info = read_json(ES_INFO);
    let es_info = Match {
        value: &es_info,
        path: vec!(Step::Root),
    };
    let global_labels = prepared_config.global_labels.iter()
        .map(|global_label| {
            match global_label.url.as_str() {
                "http://example.com:9200/?" => global_label.labels.resolve(&es_info),
                _ => unreachable!(),
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
    b.iter(|| {
        buf.clear();
        for endpoint in &prepared_config.endpoints {
            match endpoint.url.as_str() {
                "http://example.com:9200/_cluster/health?" => {
                    let es_cluster_health = read_json(ES_CLUSTER_HEALTH);
                    endpoint.process(
                        &root_metric, &es_cluster_health, &mut buf
                    );
                    buf.write_all(b"\n\n").unwrap();
                }
                "http://example.com:9200/_nodes/_local/stats?groups=_all" => {
                    let es_nodes_stats = read_json(ES_NODES_STATS);
                    endpoint.process(&root_metric, &es_nodes_stats, &mut buf);
                    buf.write_all(b"\n\n").unwrap();
                }
                "http://example.com:9200/_all/_stats?groups=_all" => {
                    let es_indices_stats = read_json(ES_INDICES_STATS);
                    endpoint.process(&root_metric, &es_indices_stats, &mut buf);
                    buf.write_all(b"\n\n").unwrap();
                }
                _ => {
                    unreachable!();
                },
            }
        }
        test::black_box(&buf);
    });
}

fn read_json(filename: &str) -> serde_json::Value {
    let file = BufReader::new(
        File::open(filename).expect(filename)
    );
     serde_json::from_reader(file)
        .expect(&format!("json file: {}", filename))
}
