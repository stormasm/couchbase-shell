use crate::cli::cloud_json::{JSONCloudsProjectsResponse, JSONCloudsResponse};
use crate::client::{CapellaClient, CapellaRequest};
use crate::state::{RemoteCluster, State};
use nu_engine::CallExt;
use nu_protocol::ast::{Call, PathMember};
use nu_protocol::engine::{EngineState, Stack};
use nu_protocol::{IntoPipelineData, PipelineData, ShellError, Span, Value};
use num_traits::cast::ToPrimitive;
use regex::Regex;
use serde_json::Error;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::Instant;

pub fn convert_row_to_nu_value(
    v: &serde_json::Value,
    span: Span,
    cluster_identifier: String,
) -> Result<Value, ShellError> {
    match v {
        serde_json::Value::Object(o) => {
            let mut cols = vec![];
            let mut vals = vec![];
            for (k, v) in o.iter() {
                cols.push(k.clone());
                vals.push(convert_json_value_to_nu_value(v, span.clone())?);
            }
            cols.push("cluster".into());
            vals.push(Value::String {
                val: cluster_identifier,
                span,
            });

            Ok(Value::Record { vals, cols, span })
        }
        _ => Err(ShellError::GenericError(
            "Malformed response".into(),
            format!("row not an object - {}", v),
            Some(span),
            None,
            Vec::new(),
        )),
    }
}

pub fn convert_json_value_to_nu_value(
    v: &serde_json::Value,
    span: Span,
) -> Result<Value, ShellError> {
    let result = match v {
        serde_json::Value::Null => Value::Nothing { span },
        serde_json::Value::Bool(b) => Value::Bool { val: *b, span },
        serde_json::Value::Number(n) => {
            if n.is_i64() {
                if let Some(val) = n.as_i64() {
                    Value::Int { val, span }
                } else {
                    return Err(ShellError::CantConvert(
                        "i64 sized integer".into(),
                        "larger than i64".into(),
                        span,
                        None,
                    ));
                }
            } else if let Some(val) = n.as_f64() {
                Value::Float { val, span }
            } else {
                return Err(ShellError::GenericError(
                    "Unexpected number value".into(),
                    format!("Cannot convert {} into i64 or f64", n),
                    Some(span),
                    None,
                    Vec::new(),
                ));
            }
        }
        serde_json::Value::String(val) => Value::String {
            val: val.clone(),
            span,
        },
        serde_json::Value::Array(a) => {
            let t = a
                .iter()
                .map(|x| convert_json_value_to_nu_value(x, span))
                .collect::<Result<Vec<Value>, ShellError>>()?;
            Value::List { vals: t, span }
        }
        serde_json::Value::Object(o) => {
            let mut cols = vec![];
            let mut vals = vec![];

            for (k, v) in o.iter() {
                cols.push(k.clone());
                vals.push(convert_json_value_to_nu_value(v, span)?);
            }

            Value::Record { cols, vals, span }
        }
    };

    Ok(result)
}

// Adapted from https://github.com/nushell/nushell/blob/main/crates/nu-command/src/commands/formats/to/json.rs
pub fn convert_nu_value_to_json_value(
    v: &Value,
    span: Span,
) -> Result<serde_json::Value, ShellError> {
    Ok(match v {
        Value::Bool { val, .. } => serde_json::Value::Bool(*val),
        Value::Filesize { val, .. } => serde_json::Value::Number(serde_json::Number::from(
            val.to_u64().expect("What about really big numbers"),
        )),
        Value::Duration { val, .. } => serde_json::Value::String(val.to_string()),
        Value::Date { val, .. } => serde_json::Value::String(val.to_string()),
        Value::Float { val, .. } => {
            if let Some(num) = serde_json::Number::from_f64(*val) {
                serde_json::Value::Number(num)
            } else {
                return Err(ShellError::GenericError(
                    "Unexpected number value".into(),
                    format!("Cannot convert {} from f64", val),
                    Some(span),
                    None,
                    Vec::new(),
                ));
            }
        }
        Value::Int { val, .. } => serde_json::Value::Number(serde_json::Number::from(*val)),
        Value::Nothing { .. } => serde_json::Value::Null,
        Value::String { val, .. } => serde_json::Value::String(val.clone()),
        Value::CellPath { val, .. } => serde_json::Value::Array(
            val.members
                .iter()
                .map(|x| match &x {
                    PathMember::String { val, .. } => Ok(serde_json::Value::String(val.clone())),
                    PathMember::Int { val, .. } => Ok(serde_json::Value::Number(
                        serde_json::Number::from(*val as u64),
                    )),
                })
                .collect::<Result<Vec<serde_json::Value>, ShellError>>()?,
        ),
        Value::List { vals, .. } => serde_json::Value::Array(json_list(vals, span)?),
        Value::Error { error } => return Err(error.clone()),
        Value::Block { .. } => serde_json::Value::Null,
        Value::Binary { val, .. } => serde_json::Value::Array(
            val.iter()
                .map(|x| {
                    Ok(serde_json::Value::Number(serde_json::Number::from(
                        *x as u64,
                    )))
                })
                .collect::<Result<Vec<serde_json::Value>, ShellError>>()?,
        ),
        Value::Record { cols, vals, .. } => {
            let mut m = serde_json::Map::new();
            for (k, v) in cols.iter().zip(vals) {
                m.insert(k.clone(), convert_nu_value_to_json_value(v, span)?);
            }
            serde_json::Value::Object(m)
        }
        Value::CustomValue { .. } => serde_json::Value::Null,
        Value::Range { .. } => serde_json::Value::Null,
    })
}

fn json_list(input: &[Value], span: Span) -> Result<Vec<serde_json::Value>, ShellError> {
    let mut out = vec![];

    for value in input {
        out.push(convert_nu_value_to_json_value(value, span)?);
    }

    Ok(out)
}

pub fn cluster_identifiers_from(
    engine_state: &EngineState,
    stack: &mut Stack,
    state: &Arc<Mutex<State>>,
    args: &Call,
    default_active: bool,
) -> Result<Vec<String>, ShellError> {
    let state = state.lock().unwrap();
    let identifier_arg: String = match args.get_flag(&engine_state, stack, "clusters")? {
        Some(arg) => arg,
        None => {
            if default_active {
                return Ok(vec![state.active()]);
            }
            "".into()
        }
    };

    let re = match Regex::new(identifier_arg.as_str()) {
        Ok(v) => v,
        Err(e) => {
            return Err(ShellError::GenericError(
                "Could not parse regex".into(),
                e.to_string(),
                Some(args.span()),
                None,
                Vec::new(),
            ));
        }
    };
    let clusters: Vec<String> = state
        .clusters()
        .keys()
        .filter(|k| re.is_match(k))
        .cloned()
        .collect();
    if clusters.is_empty() {
        return Err(cluster_not_found_error(identifier_arg, args.span()));
    }

    Ok(clusters)
}

pub fn namespace_from_args(
    bucket_flag: Option<String>,
    scope_flag: Option<String>,
    collection_flag: Option<String>,
    active_cluster: &RemoteCluster,
    span: Span,
) -> Result<(String, String, String), ShellError> {
    let bucket = match bucket_flag.or_else(|| active_cluster.active_bucket()) {
        Some(v) => Ok(v),
        None => Err(no_active_bucket_error(span)),
    }?;

    let scope = match scope_flag {
        Some(s) => s,
        None => match active_cluster.active_scope() {
            Some(s) => s,
            None => "".into(),
        },
    };

    let collection = match collection_flag {
        Some(c) => c,
        None => match active_cluster.active_collection() {
            Some(c) => c,
            None => "".into(),
        },
    };

    Ok((bucket, scope, collection))
}

pub fn validate_is_cloud(cluster: &RemoteCluster, err_msg: &str) -> Result<(), ShellError> {
    if cluster.capella_org().is_none() {
        return Err(ShellError::GenericError(
            "Not a Capella cluster".into(),
            err_msg.into(),
            None,
            None,
            Vec::new(),
        ));
    }

    Ok(())
}

pub fn validate_is_not_cloud(cluster: &RemoteCluster, err_msg: &str) -> Result<(), ShellError> {
    if cluster.capella_org().is_some() {
        return Err(ShellError::GenericError(
            "Cannot run against Capella".into(),
            err_msg.into(),
            None,
            None,
            Vec::new(),
        ));
    }

    Ok(())
}

pub(crate) fn find_project_id(
    ctrl_c: Arc<AtomicBool>,
    name: String,
    client: &Arc<CapellaClient>,
    deadline: Instant,
) -> Result<String, ShellError> {
    let response = client.capella_request(CapellaRequest::GetProjects {}, deadline, ctrl_c)?;
    if response.status() != 200 {
        return Err(generic_unspanned_error(
            "Failed to fetch project id",
            format!("Failed to fetch project id {}", response.content()),
        ));
    };
    let content: JSONCloudsProjectsResponse = serde_json::from_str(response.content())
        .map_err(map_serde_deserialize_error_to_shell_error)?;

    for p in content.items() {
        if p.name() == name.clone() {
            return Ok(p.id().to_string());
        }
    }

    Err(generic_unspanned_error(
        "Project could not be found",
        format!(
            "Project named {} was not found on the Capella organization",
            name
        ),
    ))
}

pub(crate) fn find_cloud_id(
    ctrl_c: Arc<AtomicBool>,
    name: String,
    client: &Arc<CapellaClient>,
    deadline: Instant,
) -> Result<String, ShellError> {
    let response = client.capella_request(CapellaRequest::GetClouds {}, deadline, ctrl_c)?;
    if response.status() != 200 {
        return Err(generic_unspanned_error(
            "Failed to fetch cloud id",
            format!("Failed to fetch cloud id {}", response.content()),
        ));
    };
    let clouds: JSONCloudsResponse = serde_json::from_str(response.content())
        .map_err(map_serde_deserialize_error_to_shell_error)?;

    for c in clouds.items() {
        if c.name() == name {
            return Ok(c.id().to_string());
        }
    }

    Err(generic_unspanned_error(
        "Cloud could not be found",
        format!(
            "Cloud named {} was not found on the Capella organization",
            name
        ),
    ))
}

// duration_to_golang_string creates a golang formatted string to use with timeouts. Unlike Golang
// strings it does not deal with fracational seconds, we do not need that accuracy.
pub fn duration_to_golang_string(duration: Duration) -> String {
    let mut total_secs = duration.as_secs();
    let secs = total_secs % 60;
    total_secs = total_secs / 60;
    let mut golang_string = format!("{}s", secs);
    if total_secs > 0 {
        let minutes = total_secs % 60;
        total_secs = total_secs / 60;
        golang_string = format!("{}m{}", minutes, golang_string);
        if total_secs > 0 {
            golang_string = format!("{}h{}", total_secs, golang_string)
        }
    }

    golang_string
}

#[derive(Clone, Debug, Default)]
pub struct NuValueMap {
    cols: Vec<String>,
    vals: Vec<Value>,
}

impl NuValueMap {
    pub fn add(&mut self, name: impl Into<String>, val: Value) {
        self.cols.push(name.into());
        self.vals.push(val);
    }

    pub fn add_i64(&mut self, name: impl Into<String>, val: i64, span: Span) {
        self.cols.push(name.into());
        self.vals.push(Value::Int {
            val: val.into(),
            span,
        });
    }

    pub fn add_string(&mut self, name: impl Into<String>, val: impl Into<String>, span: Span) {
        self.cols.push(name.into());
        self.vals.push(Value::String {
            val: val.into(),
            span,
        });
    }

    pub fn add_bool(&mut self, name: impl Into<String>, val: bool, span: Span) {
        self.cols.push(name.into());
        self.vals.push(Value::Bool { val, span });
    }

    pub fn into_value(self, span: Span) -> Value {
        Value::Record {
            cols: self.cols,
            vals: self.vals,
            span,
        }
    }

    pub fn into_pipeline_data(self, span: Span) -> PipelineData {
        Value::Record {
            cols: self.cols,
            vals: self.vals,
            span,
        }
        .into_pipeline_data()
    }
}

pub fn no_active_cluster_error() -> ShellError {
    ShellError::GenericError(
        "No active cluster".into(),
        "".into(),
        None,
        Some("An active cluster must be set".into()),
        Vec::new(),
    )
}

pub fn cluster_not_found_error(name: String, span: Span) -> ShellError {
    ShellError::GenericError(
        "Cluster not found".into(),
        "".into(),
        Some(span),
        Some(format!("Cluster named {} is not known", name)),
        Vec::new(),
    )
}

pub fn generic_unspanned_error(msg: impl Into<String>, help: impl Into<String>) -> ShellError {
    ShellError::GenericError(msg.into(), "".into(), None, Some(help.into()), Vec::new())
}

pub fn map_serde_deserialize_error_to_shell_error(e: serde_json::Error) -> ShellError {
    ShellError::GenericError(
        "Failed to deserialize response".into(),
        "".into(),
        None,
        Some(e.to_string()),
        Vec::new(),
    )
}

pub fn map_serde_serialize_error_to_shell_error(e: serde_json::Error) -> ShellError {
    ShellError::GenericError(
        "Failed to serialize value".into(),
        "".into(),
        None,
        Some(e.to_string()),
        Vec::new(),
    )
}

pub fn cant_run_against_hosted_capella_error() -> ShellError {
    ShellError::GenericError(
        "Cannot run command against Hosted Capella".into(),
        "".into(),
        None,
        Some("This command is currently only support against in-vpc versions of Capella".into()),
        Vec::new(),
    )
}

pub fn json_parse_fail_error(e: serde_json::Error, span: Option<Span>) -> ShellError {
    ShellError::GenericError(
        "Failed to parse response content as JSON".into(),
        "".into(),
        span,
        Some(e.to_string()),
        Vec::new(),
    )
}

pub fn unexpected_status_code_error(
    status_code: u16,
    content: &str,
    span: Option<Span>,
) -> ShellError {
    ShellError::GenericError(
        format!("Unexpected response status code: {}", status_code),
        "".into(),
        span,
        Some(content.into()),
        Vec::new(),
    )
}

pub fn no_active_bucket_error(span: Span) -> ShellError {
    ShellError::MissingParameter(
        "Could not auto-select a bucket, use --bucket or set an active bucket".into(),
        span,
    )
}

#[cfg(test)]
mod tests {
    use crate::cli::util::duration_to_golang_string;
    use std::time::Duration;

    #[test]
    fn duration_to_golang_string_some_seconds() {
        assert_eq!(
            "2s".to_string(),
            duration_to_golang_string(Duration::from_secs(2))
        );
    }

    #[test]
    fn duration_to_golang_string_some_seconds_and_minutes() {
        assert_eq!(
            "5m2s".to_string(),
            duration_to_golang_string(Duration::from_secs(302))
        );
    }

    #[test]
    fn duration_to_golang_string_some_seconds_and_minutes_and_hours() {
        assert_eq!(
            "1h5m2s".to_string(),
            duration_to_golang_string(Duration::from_secs(3902))
        );
    }
}
