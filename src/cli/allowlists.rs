use crate::cli::cloud_json::JSONCloudGetAllowListResponse;
use crate::cli::util::{
    cluster_identifiers_from, cluster_not_found_error, json_parse_fail_error,
    unexpected_status_code_error, validate_is_cloud, NuValueMap,
};
use crate::client::CapellaRequest;
use crate::state::{CapellaEnvironment, State};
use log::debug;
use std::ops::Add;
use std::sync::{Arc, Mutex};
use tokio::time::Instant;

use nu_protocol::ast::Call;
use nu_protocol::engine::{Command, EngineState, Stack};
use nu_protocol::{
    Category, IntoPipelineData, PipelineData, ShellError, Signature, SyntaxShape, Value,
};

#[derive(Clone)]
pub struct AllowLists {
    state: Arc<Mutex<State>>,
}

impl AllowLists {
    pub fn new(state: Arc<Mutex<State>>) -> Self {
        Self { state }
    }
}

impl Command for AllowLists {
    fn name(&self) -> &str {
        "allowlists"
    }

    fn signature(&self) -> Signature {
        Signature::build("allowlists")
            .named(
                "clusters",
                SyntaxShape::String,
                "the clusters which should be contacted",
                None,
            )
            .category(Category::Custom("couchbase".into()))
    }

    fn usage(&self) -> &str {
        "Displays allow list for Capella cluster access"
    }

    fn run(
        &self,
        engine_state: &EngineState,
        stack: &mut Stack,
        call: &Call,
        input: PipelineData,
    ) -> Result<PipelineData, ShellError> {
        addresses(self.state.clone(), engine_state, stack, call, input)
    }
}

fn addresses(
    state: Arc<Mutex<State>>,
    engine_state: &EngineState,
    stack: &mut Stack,
    call: &Call,
    _input: PipelineData,
) -> Result<PipelineData, ShellError> {
    let ctrl_c = engine_state.ctrlc.as_ref().unwrap().clone();

    debug!("Running allowlists");

    let cluster_identifiers = cluster_identifiers_from(engine_state, stack, &state, call, true)?;
    let guard = state.lock().unwrap();
    let mut results = vec![];
    for identifier in cluster_identifiers {
        let active_cluster = match guard.clusters().get(&identifier) {
            Some(c) => c,
            None => {
                return Err(cluster_not_found_error(identifier, call.span()));
            }
        };

        validate_is_cloud(
            active_cluster,
            "allowlists can only be used with clusters registered to a Capella organisation",
        )?;

        let cloud = guard
            .capella_org_for_cluster(active_cluster.capella_org().unwrap())?
            .client();
        let cluster = cloud.find_cluster(
            identifier.clone(),
            Instant::now().add(active_cluster.timeouts().query_timeout()),
            ctrl_c.clone(),
        )?;

        if cluster.environment() == CapellaEnvironment::Hosted {
            return Err(ShellError::UnsupportedInput(
                "allowlists cannot be run against hosted Capella clusters".into(),
                call.span(),
            ));
        }

        let response = cloud.capella_request(
            CapellaRequest::GetAllowList {
                cluster_id: cluster.id(),
            },
            Instant::now().add(active_cluster.timeouts().query_timeout()),
            ctrl_c.clone(),
        )?;
        if response.status() != 200 {
            return Err(unexpected_status_code_error(
                response.status(),
                response.content(),
                Some(call.span()),
            ));
        };

        let content: Vec<JSONCloudGetAllowListResponse> = serde_json::from_str(response.content())
            .map_err(|e| json_parse_fail_error(e, Some(call.span())))?;

        let mut entries = content
            .into_iter()
            .map(|entry| {
                let mut collected = NuValueMap::default();
                collected.add_string("address", entry.address(), call.head);
                collected.add_string("type", entry.rule_type(), call.head);
                collected.add_string("state", entry.state(), call.head);
                collected.add_string(
                    "duration",
                    entry.duration().unwrap_or_else(|| "-".to_string()),
                    call.head,
                );
                collected.add_string("created", entry.created_at(), call.head);
                collected.add_string("updated", entry.updated_at(), call.head);
                collected.into_value(call.head)
            })
            .collect();

        results.append(&mut entries);
    }

    Ok(Value::List {
        vals: results,
        span: call.head,
    }
    .into_pipeline_data())
}