use jsonpath::Found;

use serde_json::Value;

use std::collections::BTreeMap;
use std::collections::HashMap;

use crate::config::{MetricType, PreparedMetric, PreparedMetrics};

impl PreparedMetrics {
    pub fn process(&self, json: &Value, buf: &mut Vec<u8>) {
        let mut stack: Vec<
            (
                std::slice::Iter<PreparedMetric>,
                Option<Vec<(&Value, ResolvedMetric)>>
            )
        > = vec!();
        stack.push((self.iter(), None));
        let mut seen_metrics = HashMap::new();

        println!("{:?}", json);
        println!("{:?}", jsonpath::Selector::new("$").unwrap().find(json).map(|v| v.value).collect::<Vec<_>>());

        while let Some((ref mut current_metrics, parent_state)) = stack.last_mut() {
            match current_metrics.next() {
                Some(metric) => {
                    let state = if let Some(parent_state) = parent_state {
                        parent_state.iter()
                            .flat_map(|(parent_json, parent_metric)| {
                                metric.selector.find(parent_json)
                                    .filter_map(|v| {
                                        metric.resolve(&v).map(|m| (v.value, m))
                                    })
                                    .map(move |(v, m)| {
                                        (v, m.merge_with_parent(&parent_metric))
                                    })
                            })
                            .collect::<Vec<_>>()
                    } else {
                        metric.selector.find(json)
                            .filter_map(|v| {
                                metric.resolve(&v).map(|m| (v.value, m))
                            })
                            .collect::<Vec<_>>()
                    };

                    if metric.metrics.is_empty() {
                        // leaf metric
                        println!("! {}", &metric.selector.expression);
                        for (json, resolved_metric) in &state {
                            println!("  {}", resolved_metric);
                            println!("  {:?}", json);
                        }

                        for (json, resolved_metric) in &state {
                            let metric_type = seen_metrics.get(&resolved_metric.name).cloned();
                            let dumped_metric_type = resolved_metric.dump(json, metric_type, buf);
                            if let Some(dumped_metric_type) = dumped_metric_type {
                                if metric_type.is_none() {
                                    seen_metrics.insert(
                                        resolved_metric.name.clone(), dumped_metric_type
                                    );
                                }
                            } else {
                                // TODO: log metric is not dumped
                            }
                        }
                    } else {
                        // parent_metric
                        println!("> {}", &metric.selector.expression);
                        for (json, resolved_metric) in &state {
                            println!("  {}", resolved_metric);
                            println!("  {:?}", json);
                        }

                        stack.push((metric.metrics.iter(), Some(state)));
                    }
                }
                None => {
                    stack.pop();
                }
            }

        }
    }
}

impl PreparedMetric {
    fn resolve(&self, found: &Found) -> Option<ResolvedMetric> {
        let name = if let Some(metric_name) = (self.name_processor)(found) {
            metric_name
        } else {
            return None;
        };

        let mut labels = BTreeMap::new();
        for label in &self.labels {
            if let Some(label_value) = (label.value_processor)(found) {
                let safe_value = match self.should_escape_label_value(&label_value) {
                    0 => label_value,
                    num_escapes => self.escape_label_value(&label_value, num_escapes),
                };
                labels.insert(
                    label.name.clone(), safe_value
                );
            }
        }

        Some(ResolvedMetric {
            name,
            metric_type: self.metric_type,
            labels,
        })
    }

    fn should_escape_label_value(&self, label_value: &str) -> usize {
        let mut count = 0;
        for c in label_value.chars() {
            if c == '\\' || c == '"' || c == '\n' {
                count += 1;
            }
        }
        count
    }

    fn escape_label_value(&self, label_value: &str, num_escapes: usize) -> String {
        let mut escaped_value = String::with_capacity(label_value.len() + num_escapes * 2);
        for c in label_value.chars() {
            match c {
                '"' => escaped_value.push_str("\\\""),
                '\n' => escaped_value.push_str("\\n"),
                '\\' => escaped_value.push_str("\\\\"),
                c => escaped_value.push(c),
            }
        }
        escaped_value
    }
}

struct ResolvedMetric {
    name: String,
    metric_type: Option<MetricType>,
    // Use BTreeMap for reproducible tests
    labels: BTreeMap<String, String>,
}

impl ResolvedMetric {
    fn merge_with_parent(mut self, parent: &ResolvedMetric) -> Self {
        self.name = format!("{}_{}", &parent.name, &self.name);
        for (parent_label_name, parent_label_value) in parent.labels.iter() {
            self.labels.entry(parent_label_name.clone())
                .or_insert(parent_label_value.clone());
        }
        self
    }

    fn dump(
        &self,
        value: &Value,
        seen_metric_type: Option<MetricType>,
        buf: &mut Vec<u8>
    ) -> Option<MetricType> {
        // See: https://prometheus.io/docs/instrumenting/exposition_formats/#comments-help-text-and-type-information

        use MetricType::*;

        let metric_type = match (self.metric_type, seen_metric_type) {
            (Some(mtype), None) | (None, Some(mtype)) => mtype,
            (Some(mtype), Some(seen)) => {
                if mtype != seen {
                    return None;
                }
                seen
            }
            (None, None) => {
                match value {
                    Value::String(_) | Value::Bool(_) => MetricType::Untyped,
                    Value::Number(_) => MetricType::Gauge,
                    _ => return None,
                }
            }
        };

        let value = match metric_type {
            Gauge | Counter => {
                if let Some(v) = value.as_f64() {
                    v.to_string()
                } else {
                    return None;
                }
            }
            Untyped => {
                match value {
                    Value::String(v) => v.clone(),
                    Value::Number(v) => v.to_string(),
                    Value::Bool(v) => v.to_string(),
                    _ => return None,
                }
            }
        };

        if seen_metric_type.is_none() {
            buf.extend(b"# TYPE ");
            buf.extend(self.name.as_bytes());
            match metric_type {
                Gauge => buf.extend(b" gauge\n"),
                Counter => buf.extend(b" counter\n"),
                Untyped => buf.extend(b" untyped\n"),
            }
        }
        self.dump_metric(buf);
        buf.push(b' ');
        buf.extend(value.as_bytes());
        buf.push(b'\n');
        Some(metric_type)
    }

    fn dump_metric(&self, buf: &mut Vec<u8>) {
        buf.extend(self.name.as_bytes());
        if !self.labels.is_empty() {
            buf.push(b'{');
            for (label_ix, (label_name, label_value)) in self.labels.iter().enumerate() {
                if label_ix > 0 {
                    buf.push(b',');
                }
                buf.extend(label_name.as_bytes());
                buf.push(b'=');
                self.dump_string_value(label_value, buf);
            }
            buf.push(b'}');
        }
    }

    fn dump_string_value(&self, value: &str, buf: &mut Vec<u8>) {
        buf.push(b'"');
        buf.extend(value.as_bytes());
        buf.push(b'"');
    }
}

impl std::fmt::Display for ResolvedMetric {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut buf = vec!();
        self.dump_metric(&mut buf);
        f.write_str(&String::from_utf8_lossy(&buf))
    }
}

#[cfg(test)]
mod tests {
    use crate::config::Metrics;

    use indoc::indoc;

    use serde_json::Value;
    use serde_yaml;

    const FLAT_DOCS_CONFIG: &'static str = indoc! {"
      metrics:
      - name: docs_count
        selector: _all.*.docs.count
        labels:
        - name: type
          value: $1
    "};

    const NESTED_DOCS_CONFIG: &'static str = indoc! {"
      metrics:
      - name: docs
        selector: _all.*
        labels:
        - name: shard_type
          value: $1
        metrics:
        - name: count
          selector: docs.*
          labels:
          - name: count_type
            value: $1
    "};

    const DOCS_STATS: &'static str = r#"
      {
        "_all": {
          "primaries": {
            "docs": {
              "count": 167172864,
              "deleted": 1345566
            }
          },
          "total": {
            "docs": {
              "count": 334345728,
              "deleted": 2825688
            }
          }
        }
      }
    "#;

    const INDICES_DOCS_CONFIG: &'static str = indoc! {"
      metrics:
      - name: shards_$1
        selector: _shards.*
      - name: indices
        selector: indices.*
        labels:
        - name: index
          value: $1
        metrics:
        - name: shards
          selector: shards.*.*
          labels:
          - name: shard
            value: $1
          - name: node
            value: ${.routing.node}
          metrics:
          - name: docs_$1
            selector: docs.*
    "};

    const INDICES_SEARCH_CONFIG: &'static str = indoc! {"
      metrics:
      - name: indices
        selector: indices.*
        labels:
        - name: index
          value: $1
        metrics:
        - name: shards
          selector: shards.*.*
          labels:
          - name: shard
            value: $1
          - name: node
            value: ${.routing.node}
          metrics:
          - name: search_$1
            type: counter
            selector: search.*
    "};

    const INDICES_STATS: &'static str = r#"
      {
        "_shards": {
          "total": 1023,
          "successful": 1023,
          "failed": 0
        },
        "indices": {
          "catalog": {
            "shards": {
              "0": [
                {
                  "routing": {
                    "primary": false,
                    "node": "kVLufQsXRL-q9l5KN42RIQ"
                  },
                  "docs": {
                    "count": 71317,
                    "deleted": 7724
                  },
                  "search": {
                    "query_total": 8,
                    "query_time_in_millis": 385
                  }
                },
                {
                  "routing": {
                    "primary": true,
                    "node": "g4x8KHe2TS2m7gxlPhwk8g"
                  },
                  "docs": {
                    "count": 71317,
                    "deleted": 9410
                  },
                  "search": {
                    "query_total": 9,
                    "query_time_in_millis": 902
                  }
                }
              ],
              "1": [
                {
                  "routing": {
                    "primary": false,
                    "node": "kVLufQsXRL-q9l5KN42RIQ"
                  },
                  "docs": {
                    "count": 7471,
                    "deleted": 4
                  },
                  "search": {
                    "query_total": 6,
                    "query_time_in_millis": 533
                  }
                },
                {
                  "routing": {
                    "primary": true,
                    "node": "g4x8KHe2TS2m7gxlPhwk8g"
                  },
                  "docs": {
                    "count": 7471,
                    "deleted": 4
                  },
                  "search": {
                    "query_total": 9,
                    "query_time_in_millis": 351
                  }
                }
              ]
            }
          }
        }
      }
    "#;

    const CLUSTER_HEALTH_CONFIG: &'static str = indoc! {"
    metrics:
    - name: cluster
      # We need to capture a label from a root node
      selector: ''
      labels:
      - name: cluster
        value: ${.cluster_name}
      metrics:
      - name: status
        selector: status
    "};

    const CLUSTER_HEALTH_STATS: &'static str = r#"
    {
      "cluster_name": "test-cluster",
      "status": "green",
      "timed_out": false,
      "number_of_nodes": 3,
      "number_of_data_nodes": 3,
      "active_primary_shards": 680,
      "active_shards": 1023,
      "relocating_shards": 0,
      "initializing_shards": 0,
      "unassigned_shards": 0,
      "delayed_unassigned_shards": 0,
      "number_of_pending_tasks": 0,
      "number_of_in_flight_fetch": 0,
      "task_max_waiting_in_queue_millis": 0,
      "active_shards_percent_as_number": 100.0
    }
    "#;

    #[test]
    fn test_process_with_flat_config() {
        let metrics_config: Metrics = serde_yaml::from_str(FLAT_DOCS_CONFIG).expect("config");
        let prepared_metrics = metrics_config.prepare().expect("prepare config");
        let json: Value = serde_json::from_str(DOCS_STATS).expect("parsed json");

        let mut buf = vec!();
        prepared_metrics.process(&json, &mut buf);
        assert_eq!(
            String::from_utf8(buf).expect("utf8 string"),
            indoc! {"
              # TYPE docs_count gauge
              docs_count{type=\"primaries\"} 167172864
              docs_count{type=\"total\"} 334345728
            "}
        );
    }

    #[test]
    fn test_process_with_nested_config() {
        let metrics_config: Metrics = serde_yaml::from_str(NESTED_DOCS_CONFIG).expect("config");
        let prepared_metrics = metrics_config.prepare().expect("prepare config");
        let json: Value = serde_json::from_str(DOCS_STATS).expect("parsed json");

        let mut buf = vec!();
        prepared_metrics.process(&json, &mut buf);
        assert_eq!(
            String::from_utf8(buf).expect("utf8 string"),
            indoc! {"
              # TYPE docs_count gauge
              docs_count{count_type=\"count\",shard_type=\"primaries\"} 167172864
              docs_count{count_type=\"deleted\",shard_type=\"primaries\"} 1345566
              docs_count{count_type=\"count\",shard_type=\"total\"} 334345728
              docs_count{count_type=\"deleted\",shard_type=\"total\"} 2825688
            "}
        );
    }

    #[test]
    fn test_indices_docs() {
        let metrics_config: Metrics = serde_yaml::from_str(INDICES_DOCS_CONFIG).expect("config");
        let prepared_metrics = metrics_config.prepare().expect("prepare config");
        let json: Value = serde_json::from_str(INDICES_STATS).expect("parsed json");

        let mut buf = vec!();
        prepared_metrics.process(&json, &mut buf);
        assert_eq!(
            String::from_utf8(buf).expect("utf8 string"),
            indoc! {"
              # TYPE shards_failed gauge
              shards_failed 0
              # TYPE shards_successful gauge
              shards_successful 1023
              # TYPE shards_total gauge
              shards_total 1023
              # TYPE indices_shards_docs_count gauge
              indices_shards_docs_count{index=\"catalog\",node=\"kVLufQsXRL-q9l5KN42RIQ\",shard=\"0\"} 71317
              # TYPE indices_shards_docs_deleted gauge
              indices_shards_docs_deleted{index=\"catalog\",node=\"kVLufQsXRL-q9l5KN42RIQ\",shard=\"0\"} 7724
              indices_shards_docs_count{index=\"catalog\",node=\"g4x8KHe2TS2m7gxlPhwk8g\",shard=\"0\"} 71317
              indices_shards_docs_deleted{index=\"catalog\",node=\"g4x8KHe2TS2m7gxlPhwk8g\",shard=\"0\"} 9410
              indices_shards_docs_count{index=\"catalog\",node=\"kVLufQsXRL-q9l5KN42RIQ\",shard=\"1\"} 7471
              indices_shards_docs_deleted{index=\"catalog\",node=\"kVLufQsXRL-q9l5KN42RIQ\",shard=\"1\"} 4
              indices_shards_docs_count{index=\"catalog\",node=\"g4x8KHe2TS2m7gxlPhwk8g\",shard=\"1\"} 7471
              indices_shards_docs_deleted{index=\"catalog\",node=\"g4x8KHe2TS2m7gxlPhwk8g\",shard=\"1\"} 4
            "}
        );
    }

    #[test]
    fn test_indices_search() {
        let metrics_config: Metrics = serde_yaml::from_str(INDICES_SEARCH_CONFIG).expect("config");
        let prepared_metrics = metrics_config.prepare().expect("prepare config");
        let json: Value = serde_json::from_str(INDICES_STATS).expect("parsed json");

        let mut buf = vec!();
        prepared_metrics.process(&json, &mut buf);
        assert_eq!(
            String::from_utf8(buf).expect("utf8 string"),
            indoc! {"
              # TYPE indices_shards_search_query_time_in_millis counter
              indices_shards_search_query_time_in_millis\
                {index=\"catalog\",node=\"kVLufQsXRL-q9l5KN42RIQ\",shard=\"0\"} 385
              # TYPE indices_shards_search_query_total counter
              indices_shards_search_query_total\
                {index=\"catalog\",node=\"kVLufQsXRL-q9l5KN42RIQ\",shard=\"0\"} 8
              indices_shards_search_query_time_in_millis\
                {index=\"catalog\",node=\"g4x8KHe2TS2m7gxlPhwk8g\",shard=\"0\"} 902
              indices_shards_search_query_total\
                {index=\"catalog\",node=\"g4x8KHe2TS2m7gxlPhwk8g\",shard=\"0\"} 9
              indices_shards_search_query_time_in_millis\
                {index=\"catalog\",node=\"kVLufQsXRL-q9l5KN42RIQ\",shard=\"1\"} 533
              indices_shards_search_query_total\
                {index=\"catalog\",node=\"kVLufQsXRL-q9l5KN42RIQ\",shard=\"1\"} 6
              indices_shards_search_query_time_in_millis\
                {index=\"catalog\",node=\"g4x8KHe2TS2m7gxlPhwk8g\",shard=\"1\"} 351
              indices_shards_search_query_total\
                {index=\"catalog\",node=\"g4x8KHe2TS2m7gxlPhwk8g\",shard=\"1\"} 9
            "}
        );
    }

    #[test]
    fn test_untyped_metric() {
        let metrics_config: Metrics = serde_yaml::from_str(CLUSTER_HEALTH_CONFIG).expect("config");
        let prepared_metrics = metrics_config.prepare().expect("prepare config");
        let json: Value = serde_json::from_str(CLUSTER_HEALTH_STATS).expect("parsed json");

        let mut buf = vec!();
        prepared_metrics.process(&json, &mut buf);
        assert_eq!(
            String::from_utf8(buf).expect("utf8 string"),
            indoc! {"
              # TYPE cluster_status untyped
              cluster_status{cluster=\"test-cluster\"} green
            "}
        );
    }
}
