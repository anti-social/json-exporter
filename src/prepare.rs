use anyhow::{anyhow, bail, Error as AnyhowError};

use fehler::{throw, throws};

use jsonpath::{Selector, Match, Step};

use serde_json::Value;

use std::collections::HashMap;
use std::convert::TryFrom;

use url::Url;

use crate::config::{
    Config,
    Endpoint,
    Filter,
    GlobalLabels,
    Label,
    Metric,
    MetricType,
    UrlParts
};
use crate::filters::{
    self,
    Filter as PreparedFilter,
};
use crate::tmpl::{
    string_with_placeholders,
    Placeholder,
    Var,
};


#[derive(Clone)]
pub struct PreparedConfig {
    pub namespace: Option<String>,
    pub global_labels: Vec<PreparedGlobalLabels>,
    pub endpoints: Vec<PreparedEndpoint>,
}

impl PreparedConfig {
    #[throws(AnyhowError)]
    pub fn create_from(
        config: &Config,
        base_url: &Url,
        override_endpoint_urls: &HashMap<String, String>,
    ) -> Self {
        let mut prepared_global_labels = vec!();
        for global_labels in &config.global_labels {
            prepared_global_labels.push(PreparedGlobalLabels::create_from(global_labels, base_url)?);
        }
        let mut prepared_endpoints = vec!();
        for endpoint in &config.endpoints {
            // TODO: Check if there are unknown endpoint.id
            let override_endpoint_url = endpoint.id.as_ref().and_then(|endpoint_id| {
               override_endpoint_urls.get(endpoint_id)
            });
            prepared_endpoints.push(
                PreparedEndpoint::create_from(
                    endpoint, base_url, override_endpoint_url
                )?
            );
        }
        Self {
            namespace: config.namespace.clone(),
            global_labels: prepared_global_labels,
            endpoints: prepared_endpoints,
        }
    }
}

#[derive(Clone)]
pub struct PreparedGlobalLabels {
    pub url: Url,
    pub labels: PreparedLabels,
}

impl PreparedGlobalLabels {
    #[throws(AnyhowError)]
    fn create_from(global_labels: &GlobalLabels, base_url: &Url) -> Self {
        let mut url_patch = UrlPatch::default();
        url_patch.add_path_with_query(&global_labels.url);
        let url = url_patch.apply(&base_url)?;
        Self {
            url,
            labels: PreparedLabels::try_from(&global_labels.labels)?,
        }
    }
}

#[derive(Clone)]
pub struct PreparedLabel {
    pub name: String,
    pub value_processor: TemplateProcessor,
}

impl<'a> TryFrom<&'a Label> for PreparedLabel {
    type Error = AnyhowError;

    #[throws(AnyhowError)]
    fn try_from(label: &Label) -> Self {
        Self {
            name: label.name.clone(),
            value_processor: TemplateProcessor::create_from(&label.value)?,
        }
    }
}

#[derive(Clone)]
pub struct PreparedLabels {
    pub(crate) labels: Vec<PreparedLabel>,
}

impl<'a> TryFrom<&'a Vec<Label>> for PreparedLabels {
    type Error = AnyhowError;

    #[throws(AnyhowError)]
    fn try_from(labels: &'a Vec<Label>) -> Self {
        let mut prepared_labels = vec!();
        for label in labels {
            prepared_labels.push(PreparedLabel::try_from(label)?);
        }
        Self { labels: prepared_labels }
    }
}

#[derive(Clone)]
pub struct PreparedEndpoint {
    pub id: Option<String>,
    pub url: Url,
    pub metrics: PreparedMetrics,
}

impl PreparedEndpoint {
    #[throws(AnyhowError)]
    fn create_from(
        endpoint: &Endpoint,
        base_url: &Url,
        overriden_endpoint_url: Option<&String>,
    ) -> Self {
        let mut url_patch = UrlPatch::default();
        url_patch.add_endpoint_url(&endpoint.url, &endpoint.url_parts, true)?;
        if let Some(overriden_endpoint_url) = overriden_endpoint_url {
            url_patch.add_endpoint_url(&overriden_endpoint_url, &endpoint.url_parts, false)?;
        }
        let url = url_patch.apply(&base_url)?;
        Self {
            id: endpoint.id.clone(),
            url,
            metrics: PreparedMetrics::create_from(&endpoint.metrics, None)?
        }
    }
}

#[derive(Clone)]
pub struct PreparedMetrics(pub Vec<PreparedMetric>);

impl PreparedMetrics {
    #[throws(AnyhowError)]
    pub fn create_from(
        metrics: &[Metric],
        metric_type: Option<MetricType>
    ) -> Self {
        let mut prepared_metrics = vec!();
        for metric in metrics.iter() {
            prepared_metrics.push(PreparedMetric::create_from(metric, metric_type)?);
        }
        Self(prepared_metrics)
    }

    pub fn iter(&self) -> std::slice::Iter<PreparedMetric> {
        self.0.iter()
    }
}

pub struct PreparedMetric {
    pub selector: JsonSelector,
    pub metric_type: Option<MetricType>,
    pub name: Option<String>,
    pub name_processor: Option<TemplateProcessor>,
    pub filters: Vec<Box<dyn PreparedFilter + Send>>,
    pub labels: PreparedLabels,
    pub metrics: PreparedMetrics,
}

impl PreparedMetric {
    #[throws(AnyhowError)]
    fn create_from(
        metric: &Metric,
        parent_metric_type: Option<MetricType>,
    ) -> Self {
        let metric_type = metric.metric_type.or(parent_metric_type);
        // TODO: validate metric and label names
        let name = metric.name.clone();
        let name_processor = metric.name.as_ref().map(|n| TemplateProcessor::create_from(n))
            .transpose()?;
        let selector = JsonSelector::new(&metric.path)?;

        let mut prepared_filters = vec!();
        for filter in &metric.modifiers {
            prepared_filters.push(filter.prepare()?);
        }

        Self {
            metric_type,
            name,
            name_processor,
            selector,
            filters: prepared_filters,
            labels: PreparedLabels::try_from(&metric.labels)?,
            metrics: PreparedMetrics::create_from(&metric.metrics, metric_type)?,
        }
    }
}

impl Clone for PreparedMetric {
    fn clone(&self) -> Self {
        Self {
            selector: self.selector.clone(),
            metric_type: self.metric_type,
            name: self.name.clone(),
            name_processor: self.name_processor.clone(),
            filters: self.filters.iter()
                .map(|f| dyn_clone::clone_box(f.as_ref()))
                .collect(),
            labels: self.labels.clone(),
            metrics: self.metrics.clone(),
        }
    }
}

#[derive(Clone)]
pub struct JsonSelector {
    pub expression: String,
    selector: Selector,
}

impl JsonSelector {
    #[throws(AnyhowError)]
    fn new(expression: &str) -> Self {
        let expression = if expression.is_empty() {
            "$".to_string()
        } else {
            format!("$.{}", expression)
        };
        let selector = Selector::new(&expression)
            .map_err(|e| anyhow!(
                "Error when creating json selector [{}]: {}", expression, e
            ))?;

        Self {
            expression,
            selector,
        }
    }

    pub fn find<'a>(&'a self, root: &'a Value) -> impl Iterator<Item=Match<'_>> {
        self.selector.find(root)
    }
}

impl Filter {
    #[throws(AnyhowError)]
    fn prepare(&self) -> Box<dyn PreparedFilter + Send> {
        let create_filter = match self.name.as_str() {
            "mul" | "multiply" => filters::Multiply::create,
            "div" | "divide" => filters::Divide::create,
            _ => throw!(anyhow!("Unknown filter: {}", &self.name)),
        };
        create_filter(&self.args)?
    }
}

#[derive(Clone, Default)]
pub struct TemplateProcessor {
    tmpl: Vec<PreparedPlaceholder>,
}

impl TemplateProcessor {
    #[throws(AnyhowError)]
    fn create_from(tmpl: &str) -> Self {
        if tmpl.is_empty() {
            return Default::default();
        }
        let placeholders = string_with_placeholders(tmpl).map_err(|e| {
            e.map(|e| nom::Err::Error((e.input.to_string(), e.code)))
        })?.1;
        let prepared_placeholders = placeholders.iter()
            .map(PreparedPlaceholder::create_from)
            .collect::<Result<Vec<_>, _>>()?;
        Self {
            tmpl: prepared_placeholders,
        }
    }

    #[throws(AnyhowError)]
    pub fn apply(&self, found: &Match) -> String {
        use PreparedPlaceholder::*;

        let mut text = String::new();

        // TODO: benchmark specialized versions of template processor
        for placeholder in &self.tmpl {
            match placeholder {
                Text(t) => {
                    text.push_str(t);
                }
                VarIx(path_ix) => {
                    match found.path.get(*path_ix as usize + 1) {
                        Some(Step::Key(key)) => text.push_str(key),
                        Some(Step::Index(ix)) => text.push_str(&ix.to_string()),
                        Some(Step::Root) => throw!(anyhow!("Root element is not supported")),
                        None => throw!(anyhow!("Invalid path index: {}", path_ix)),
                    }
                }
                VarIdent(selector) => {
                    // TODO: Should we return an error when there are several
                    // matching values?
                    if let Some(v) = selector.find(found.value).next() {
                        match v.value {
                            Value::String(v) => text.push_str(&v),
                            Value::Bool(v) => text.push_str(&v.to_string()),
                            Value::Number(v) => text.push_str(&v.to_string()),
                            _ => {}
                        }
                    }
                }
            }
        }
        text
    }
}

#[derive(Clone)]
enum PreparedPlaceholder {
    Text(String),
    VarIx(u32),
    VarIdent(JsonSelector),
}

impl PreparedPlaceholder {
    #[throws(AnyhowError)]
    fn create_from(placeholder: &Placeholder) -> Self {
        match placeholder {
            Placeholder::Text(text) => {
                PreparedPlaceholder::Text(text.clone())
            },
            Placeholder::Var(Var::Ix(ix)) => {
                PreparedPlaceholder::VarIx(*ix)
            },
            Placeholder::Var(Var::Ident(ident)) => {
                let selector = JsonSelector::new(ident)?;
                PreparedPlaceholder::VarIdent(selector)
            }
        }
    }
}

#[derive(Default)]
struct UrlPatch {
    path_segments: Vec<String>,
    query_params: HashMap<String, String>,
}

impl UrlPatch {
    fn add_endpoint_url(
        &mut self, path_or_dsl: &str, url_parts: &UrlParts, is_path_mandatory: bool
    ) -> Result<(), AnyhowError> {
        let path_dsl = match path_or_dsl.strip_prefix("/") {
            Some(path_with_query) => {
                self.add_path_with_query(path_with_query);
                return Ok(());
            },
            None => PathDsl::parse(path_or_dsl),
        };

        let available_paths = &url_parts.paths;
        let available_params = &url_parts.params;
        match path_dsl.name.as_ref() {
            Some(path_key) => {
                match available_paths.get(path_key) {
                    Some(path) => {
                        self.path_segments = path.split('/').map(str::to_string).collect();
                    }
                    None => bail!(
                        "Unknown url path name: {:?}, valid paths: {:?}",
                        path_key, available_paths.keys().collect::<Vec<_>>()
                    )
                }
            },
            None if is_path_mandatory => bail!("Path is mandatory"),
            None => {},
        };
        if let Some(params) = path_dsl.params {
            self.query_params.clear();
            for param_key in &params {
                match available_params.get(param_key) {
                    Some(param) => {
                        self.query_params.insert(
                            param.name.clone(),
                            param.value.as_ref().map(String::clone)
                                .unwrap_or_else(|| "".to_string())
                        );
                    }
                    None => bail!(
                        "Unknown url parameter name: {:?}, valid params: {:?}",
                        param_key, available_params.keys().collect::<Vec<_>>()
                    )
                }
            }
        }
        Ok(())
    }

    fn add_path_with_query(&mut self, path_with_query: &str) {
        let mut path_and_query_parts = path_with_query.splitn(2, '?');
        if let Some(path) = path_and_query_parts.next() {
            self.path_segments = path.split('/').map(str::to_string).collect();
        }
        if let Some(query) = path_and_query_parts.next() {
            for param in query.split('&') {
                let mut param_split = param.splitn(2, '=');
                if let Some(param_name) = param_split.next() {
                    self.query_params.insert(
                        param_name.to_string(),
                        param_split.next().unwrap_or("").to_string()
                    );
                }
            }
        }
    }

    fn apply(&self, url: &Url) -> Result<Url, AnyhowError> {
        let mut url = url.clone();
        {
            match url.path_segments_mut() {
                Ok(mut path_segments) => {
                    path_segments.pop_if_empty();
                    for segment in &self.path_segments {
                        if !segment.is_empty() {
                            path_segments.push(segment);
                        }
                    }
                }
                Err(()) => bail!("Url cannot be base"),
            }
            let mut url_query_pairs = url.query_pairs_mut();
            for (name, value) in &self.query_params {
                url_query_pairs.append_pair(name, value);
            }
        }
        Ok(url)
    }
}

#[derive(Debug, PartialEq)]
struct PathDsl {
    name: Option<String>,
    params: Option<Vec<String>>,
}

impl PathDsl {
    fn parse(path_dsl: &str) -> Self {
        let url_parts = path_dsl.splitn(2, '?').collect::<Vec<_>>();
        let (name, params) = match &url_parts[..] {
            [""] => {
                (None, None)
            }
                [name] => {
            (Some(name.to_string()), None)
            }
            [name, params_dsl] => {
                (
                    if name.is_empty() {
                        None
                    } else {
                        Some(name.to_string())
                    },
                    if params_dsl.is_empty() {
                        Some(vec!())
                    } else {
                        Some(
                            params_dsl.split('&')
                                .filter(|p| !p.is_empty())
                                .map(str::to_string)
                                .collect::<Vec<_>>()
                        )
                    },
                )
            }
            _ => unreachable!()
        };
        Self { name, params }
    }
}

#[cfg(test)]
mod tests {
    use super::{PathDsl, UrlPatch};
    use crate::config::{UrlParts, QueryParam};
    use url::Url;
    use nom::lib::std::collections::HashMap;

    #[test]
    fn test_path_dsl_parsing() {
        assert_eq!(
            PathDsl::parse(""),
            PathDsl {
                name: None,
                params: None,
            }
        );
        assert_eq!(
            PathDsl::parse("?"),
            PathDsl {
                name: None,
                params: Some(vec!()),
            }
        );
        assert_eq!(
            PathDsl::parse("nodes"),
            PathDsl {
                name: Some("nodes".to_string()),
                params: None,
            }
        );
        assert_eq!(
            PathDsl::parse("nodes?"),
            PathDsl {
                name: Some("nodes".to_string()),
                params: Some(vec!()),
            }
        );
        assert_eq!(
            PathDsl::parse("nodes&indices"),
            PathDsl {
                name: Some("nodes&indices".to_string()),
                params: None,
            }
        );
        assert_eq!(
            PathDsl::parse("?groups"),
            PathDsl {
                name: None,
                params: Some(vec!("groups".to_string())),
            }
        );
        assert_eq!(
            PathDsl::parse("nodes?groups"),
            PathDsl {
                name: Some("nodes".to_string()),
                params: Some(vec!("groups".to_string())),
            }
        );
        assert_eq!(
            PathDsl::parse("nodes?groups&shards"),
            PathDsl {
                name: Some("nodes".to_string()),
                params: Some(vec!("groups".to_string(), "shards".to_string())),
            }
        );
        assert_eq!(
            PathDsl::parse("nodes?groups&shards?segments"),
            PathDsl {
                name: Some("nodes".to_string()),
                params: Some(vec!("groups".to_string(), "shards?segments".to_string())),
            }
        );
    }

    #[test]
    fn test_url_patch() {
        let bare_base_url = Url::parse("http://example.com").expect("valid url");
        let root_base_url = Url::parse("http://example.com/").expect("valid url");
        let file_base_url = Url::parse("http://example.com/test").expect("valid url");
        let dir_base_url = Url::parse("http://example.com/test/").expect("valid url");

        let mut url_patch = UrlPatch::default();
        let url_parts = UrlParts::default();
        url_patch.add_endpoint_url("/", &url_parts, false).unwrap();

        assert_eq!(
            url_patch.apply(&bare_base_url).unwrap().to_string(),
            // TODO: rid of hanging '?' sign
            "http://example.com/?"
        );
        assert_eq!(
            url_patch.apply(&root_base_url).unwrap().to_string(),
            "http://example.com/?"
        );
        assert_eq!(
            url_patch.apply(&file_base_url).unwrap().to_string(),
            "http://example.com/test?"
        );
        assert_eq!(
            url_patch.apply(&dir_base_url).unwrap().to_string(),
            "http://example.com/test?"
        );

        url_patch.add_endpoint_url("/help", &url_parts, false).unwrap();

        assert_eq!(
            url_patch.apply(&bare_base_url).unwrap().to_string(),
            "http://example.com/help?"
        );
        assert_eq!(
            url_patch.apply(&root_base_url).unwrap().to_string(),
            "http://example.com/help?"
        );
        assert_eq!(
            url_patch.apply(&file_base_url).unwrap().to_string(),
            "http://example.com/test/help?"
        );
        assert_eq!(
            url_patch.apply(&dir_base_url).unwrap().to_string(),
            "http://example.com/test/help?"
        );

        url_patch.add_endpoint_url("/?help=me", &url_parts, false).unwrap();

        assert_eq!(
            url_patch.apply(&bare_base_url).unwrap().to_string(),
            "http://example.com/?help=me"
        );
        assert_eq!(
            url_patch.apply(&root_base_url).unwrap().to_string(),
            "http://example.com/?help=me"
        );
        assert_eq!(
            url_patch.apply(&file_base_url).unwrap().to_string(),
            "http://example.com/test?help=me"
        );
        assert_eq!(
            url_patch.apply(&dir_base_url).unwrap().to_string(),
            "http://example.com/test?help=me"
        );

        let mut paths = HashMap::new();
        paths.insert("all".to_string(), "/_all".to_string());
        paths.insert("local".to_string(), "/_local".to_string());
        let mut params = HashMap::new();
        params.insert(
            "global".to_string(),
            QueryParam { name: "global".to_string(), value: None }
        );
        params.insert(
            "help".to_string(),
            QueryParam { name: "help".to_string(), value: Some("me".to_string()) }
        );
        let url_parts = UrlParts { paths, params };

        url_patch.add_endpoint_url("?help", &url_parts, false).unwrap();

        assert_eq!(
            url_patch.apply(&bare_base_url).unwrap().to_string(),
            "http://example.com/?help=me"
        );
        assert_eq!(
            url_patch.apply(&root_base_url).unwrap().to_string(),
            "http://example.com/?help=me"
        );
        assert_eq!(
            url_patch.apply(&file_base_url).unwrap().to_string(),
            "http://example.com/test?help=me"
        );
        assert_eq!(
            url_patch.apply(&dir_base_url).unwrap().to_string(),
            "http://example.com/test?help=me"
        );

        url_patch.add_endpoint_url("all?global", &url_parts, false).unwrap();

        assert_eq!(
            url_patch.apply(&bare_base_url).unwrap().to_string(),
            "http://example.com/_all?global="
        );
        assert_eq!(
            url_patch.apply(&root_base_url).unwrap().to_string(),
            "http://example.com/_all?global="
        );
        assert_eq!(
            url_patch.apply(&file_base_url).unwrap().to_string(),
            "http://example.com/test/_all?global="
        );
        assert_eq!(
            url_patch.apply(&dir_base_url).unwrap().to_string(),
            "http://example.com/test/_all?global="
        );

        url_patch.add_endpoint_url("local", &url_parts, false).unwrap();

        assert_eq!(
            url_patch.apply(&bare_base_url).unwrap().to_string(),
            "http://example.com/_local?global="
        );
        assert_eq!(
            url_patch.apply(&root_base_url).unwrap().to_string(),
            "http://example.com/_local?global="
        );
        assert_eq!(
            url_patch.apply(&file_base_url).unwrap().to_string(),
            "http://example.com/test/_local?global="
        );
        assert_eq!(
            url_patch.apply(&dir_base_url).unwrap().to_string(),
            "http://example.com/test/_local?global="
        );

        url_patch.add_endpoint_url("local?", &url_parts, false).unwrap();

        assert_eq!(
            url_patch.apply(&bare_base_url).unwrap().to_string(),
            "http://example.com/_local?"
        );
        assert_eq!(
            url_patch.apply(&root_base_url).unwrap().to_string(),
            "http://example.com/_local?"
        );
        assert_eq!(
            url_patch.apply(&file_base_url).unwrap().to_string(),
            "http://example.com/test/_local?"
        );
        assert_eq!(
            url_patch.apply(&dir_base_url).unwrap().to_string(),
            "http://example.com/test/_local?"
        );
    }
}
