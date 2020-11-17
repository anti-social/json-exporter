![Build, lint and test](https://github.com/anti-social/json-exporter/workflows/Build,%20lint%20and%20test/badge.svg)

# Json exporter
Prometheus metrics exporter for any json metrics that are provided by elasticsearch or kafka-manager

## Why I need another exporter?

- you can drop metrics you don't use. Less metrics - better for Prometheus
- you can add metrics you need without patching source code. Just put them into config.
- [Elasticsearch exporter](https://github.com/justwatchcom/elasticsearch_exporter) is not maintained for a long time
