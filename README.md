![Build, lint and test](https://github.com/anti-social/json-exporter/workflows/Build,%20lint%20and%20test/badge.svg)

# Json exporter
Prometheus metrics exporter for any json metrics that are provided by elasticsearch or kafka-manager

## Why do I need another exporter?

- you can drop metrics you don't use. Less metrics - better for Prometheus
- you can add metrics you need without patching source code. Just put them into config.
- [Elasticsearch exporter](https://github.com/justwatchcom/elasticsearch_exporter) is not maintained for a long time

## How to run

Download and unpack last release: https://github.com/anti-social/json-exporter/releases

Run it:

```shell script
./json-exporter --base-url http://localhost:9200 elasticsearch_exporter.yaml
```

Check it opening in your browser: http://localhost:9114/metrics

You can set log level via `RUST_LOG` environment variable:

```shell script
RUST_LOG=info ./json-exporter --base-url http://localhost:9200 elasticsearch_exporter.yaml
``` 

### Using docker

```shell script
docker pull kovalidis/json-exporter
docker run -it --rm -p 9114 -e RUST_LOG=info kovalidis/json-exporter --base-url http://localhost:9200 /app/elasticsearch_exporter.yaml
```
