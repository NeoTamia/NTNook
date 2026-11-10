//! Local stdio adapter. Never routes requests through the reconciling CLI.
use crate::{api::*, operation::Budget};
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt, model::*, service::RequestContext};
use schemars::{JsonSchema, schema_for};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::{borrow::Cow, collections::BTreeSet, sync::Arc, time::Duration};
use tokio::sync::Semaphore;

pub(crate) enum Request {
    Profiles,
    List(ListInput),
    Url(UrlInput),
    Status(StatusInput),
    Set(SetInput),
    Remove(RemoveInput),
}
impl Request {
    pub(crate) fn parse(name: &str, arguments: Value) -> Result<Self, ErrorData> {
        fn decode<T: DeserializeOwned>(value: Value) -> Result<T, ErrorData> {
            serde_json::from_value(value)
                .map_err(|error| ErrorData::invalid_params(error.to_string(), None))
        }
        let request = match name {
            "nook_tools_profiles" => {
                let _: Empty = decode(arguments)?;
                Self::Profiles
            }
            "nook_list" => {
                let args: ListInput = decode(arguments)?;
                if args.limit.is_some_and(|v| !(1..=200).contains(&v)) {
                    return Err(ErrorData::invalid_params("limit must be 1..200", None));
                }
                Self::List(args)
            }
            "nook_url" => {
                let args: UrlInput = decode(arguments)?;
                if args.hostname.is_some()
                    && (args.app.is_some() || args.mode == UrlMode::Configured)
                {
                    return Err(ErrorData::invalid_params(
                        "hostname excludes app and configured mode",
                        None,
                    ));
                }
                Self::Url(args)
            }
            "nook_status" => Self::Status(decode(arguments)?),
            "nook_alias_set" => {
                let args: SetInput = decode(arguments)?;
                if matches!(args.target, Target::Port(0)) {
                    return Err(ErrorData::invalid_params(
                        "target port must be 1..65535",
                        None,
                    ));
                }
                Self::Set(args)
            }
            "nook_alias_remove" => {
                let args: RemoveInput = decode(arguments)?;
                if uuid::Uuid::parse_str(&args.expected_owner_id).is_err() {
                    return Err(ErrorData::invalid_params(
                        "expected_owner_id must be a UUID",
                        None,
                    ));
                }
                Self::Remove(args)
            }
            _ => return Err(ErrorData::invalid_params("Unknown or disabled tool", None)),
        };
        let hostname = match &request {
            Self::List(a) => a.hostname.as_deref(),
            Self::Url(a) => a.hostname.as_deref(),
            Self::Set(a) => Some(a.hostname.as_str()),
            Self::Remove(a) => Some(a.hostname.as_str()),
            _ => None,
        };
        if let Some(hostname) = hostname {
            full_hostname(hostname).map_err(|e| ErrorData::invalid_params(e.message, None))?;
        }
        if !(100..=30000).contains(&request.timeout()) {
            return Err(ErrorData::invalid_params(
                "timeout_ms must be 100..30000",
                None,
            ));
        }
        Ok(request)
    }
    fn timeout(&self) -> u64 {
        match self {
            Self::Profiles => 5000,
            Self::List(a) => a.timeout_ms.unwrap_or(5000),
            Self::Url(a) => a.timeout_ms.unwrap_or(5000),
            Self::Status(a) => a.timeout_ms.unwrap_or(15000),
            Self::Set(a) => a.timeout_ms.unwrap_or(15000),
            Self::Remove(a) => a.timeout_ms.unwrap_or(15000),
        }
    }
    fn mutation(&self) -> bool {
        matches!(self, Self::Set(_) | Self::Remove(_))
    }
    pub(crate) fn execute(self, context: &Context, budget: &Budget) -> Value {
        match self {
            Self::Profiles => context.reply(Ok(context.profiles())),
            Self::List(a) => context.reply(context.list(a, budget)),
            Self::Url(a) => context.reply(context.url(a, budget)),
            Self::Status(a) => context.reply(context.status(a, budget)),
            Self::Set(a) => context.reply(context.set_alias(a, budget)),
            Self::Remove(a) => context.reply(context.remove_alias(a, budget)),
        }
    }
}
fn schema<T: JsonSchema>() -> serde_json::Map<String, Value> {
    serde_json::to_value(schema_for!(T))
        .unwrap()
        .as_object()
        .unwrap()
        .clone()
}
fn tool<I: JsonSchema, O: JsonSchema>(
    name: &'static str,
    description: &'static str,
    mutation: bool,
    destructive: bool,
    open: bool,
) -> Tool {
    let mut annotations = ToolAnnotations::default();
    annotations.read_only_hint = Some(!mutation);
    annotations.destructive_hint = Some(destructive);
    annotations.idempotent_hint = Some(true);
    annotations.open_world_hint = Some(open);
    let mut output = schema::<Reply<O>>();
    output.insert("type".into(), json!("object"));
    if let Some(variants) = output.get_mut("anyOf").and_then(Value::as_array_mut) {
        for variant in variants {
            let success = variant["properties"].get("data").is_some();
            variant["properties"]["ok"] = json!({"const":success});
            variant["properties"]["schema_version"] = json!({"const":1});
        }
    }
    Tool::new(name, description, schema::<I>())
        .with_raw_output_schema(Arc::new(output))
        .with_annotations(annotations)
}
pub(crate) fn catalog(aliases: bool) -> Vec<Tool> {
    let mut tools = vec![
        tool::<Empty, ProfilesData>(
            "nook_tools_profiles",
            "Show fixed profiles, scope and alias grants. Changing them requires restarting nook mcp.",
            false,
            false,
            false,
        ),
        tool::<ListInput, ListData>(
            "nook_list",
            "Read registered routes in scope without reconciliation. Readiness is recorded state, not an HTTP health check.",
            false,
            false,
            false,
        ),
        tool::<UrlInput, UrlData>(
            "nook_url",
            "Look up a registered URL or calculate a configured URL. A configured URL does not prove an application is running.",
            false,
            false,
            false,
        ),
        tool::<StatusInput, StatusData>(
            "nook_status",
            "Observe registry, process identity and existing Caddy. full additionally checks local CA trust. Never repairs or starts services.",
            false,
            false,
            true,
        ),
    ];
    // Keep the discoverable selector contract aligned with runtime validation.
    let schema = Arc::make_mut(&mut tools[2].input_schema);
    schema.insert("allOf".into(), json!([
        {"not":{"required":["hostname","app"],"properties":{"hostname":{"type":"string"},"app":{"type":"string"}}}},
        {"not":{"required":["hostname","mode"],"properties":{"hostname":{"type":"string"},"mode":{"const":"configured"}}}}
    ]));
    if aliases {
        tools.extend([
        tool::<SetInput, SetData>("nook_alias_set", "Create or repair an explicitly authorized persistent alias. Identical aliases retain ownership; different aliases, runs and foreign routes are never replaced.", true,false,true),
        tool::<RemoveInput, RemoveData>("nook_alias_remove", "Remove an authorized alias only if its owner UUID still matches. No process or Caddy service is stopped.", true,true,true),
    ]);
    }
    tools
}
struct Server {
    context: Arc<Context>,
    reads: Arc<Semaphore>,
    mutation: Arc<Semaphore>,
}
impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("nook", env!("CARGO_PKG_VERSION")))
            .with_instructions("Nook observes an existing Caddy. Launch applications with nook run in your shell. It never starts Caddy, installs trust, or automatically prunes resources.")
    }
    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Owned(
            ["2026-07-28", "2025-11-25", "2025-06-18"]
                .into_iter()
                .map(|s| serde_json::from_value(json!(s)).unwrap())
                .collect(),
        )
    }
    fn get_tool(&self, name: &str) -> Option<Tool> {
        catalog(self.context.aliases)
            .into_iter()
            .find(|t| t.name == name)
    }
    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        if request.is_some_and(|r| r.cursor.is_some()) {
            return Err(ErrorData::invalid_params(
                "Invalid tool catalog cursor",
                None,
            ));
        }
        Ok(ListToolsResult {
            tools: catalog(self.context.aliases),
            ..Default::default()
        })
    }
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        rpc: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        if self.get_tool(&request.name).is_none() {
            return Err(ErrorData::invalid_params("Unknown or disabled tool", None));
        }
        let request = Request::parse(
            &request.name,
            Value::Object(request.arguments.unwrap_or_default()),
        )?;
        let cancellation = rpc.ct.child_token();
        let _cancel_on_drop = cancellation.clone().drop_guard();
        let budget = Budget::new(request.timeout(), cancellation.clone());
        let mutation = request.mutation();
        let semaphore = if request.mutation() {
            &self.mutation
        } else {
            &self.reads
        };
        let permit = match semaphore.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                return result(self.context.reply::<Value>(Err(Failure::new(
                    Code::StateBusy,
                    "Concurrent operation limit reached; retry later",
                ))));
            }
        };
        let context = self.context.clone();
        let timeout = Duration::from_millis(request.timeout());
        let mut work = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            request.execute(&context, &budget)
        });
        let value = tokio::select! {
            answer = &mut work => answer.unwrap_or_else(|_| self.context.reply::<Value>(Err(Failure::new(Code::Internal, "Nook operation failed")))),
            _ = rpc.ct.cancelled() => return Err(ErrorData::internal_error("Operation cancelled", None)),
            _ = tokio::time::sleep(timeout) => {
                cancellation.cancel();
                let mut error = Failure::new(Code::Timeout, "Operation deadline exceeded; inspect status before retrying mutations");
                error.effect = if mutation { Effect::Unknown } else { Effect::None };
                self.context.reply::<Value>(Err(error))
            }
        };
        result(value)
    }
}
fn result(value: Value) -> Result<CallToolResponse, ErrorData> {
    let text = serde_json::to_string(&value).unwrap();
    if text.len() > 4 * 1024 * 1024 {
        return Err(ErrorData::internal_error(
            "Response exceeds 4 MiB; narrow the query",
            None,
        ));
    }
    let mut result = CallToolResult::success(vec![ContentBlock::text(text)]);
    result.is_error = Some(value["ok"] != true);
    result.structured_content = Some(value);
    Ok(result.into())
}
pub(crate) fn run(args: McpArgs, socket: Option<String>) -> crate::Result<i32> {
    let profiles: BTreeSet<_> = args.tools.split(',').collect();
    let invalid =
        |message: &str| clap::Error::raw(clap::error::ErrorKind::ValueValidation, message);
    if !profiles.contains("read") || profiles.iter().any(|p| !["read", "aliases"].contains(p)) {
        return Err(invalid("--tools must be read or read,aliases").into());
    }
    let aliases = profiles.contains("aliases");
    let allowed = args
        .allow_alias
        .iter()
        .map(|s| full_hostname(s).map_err(|e| invalid(&e.message)))
        .collect::<Result<BTreeSet<_>, _>>()?;
    if aliases && allowed.is_empty() {
        return Err(invalid("--tools read,aliases requires --allow-alias HOST").into());
    }
    let context = Context::new(args.context, args.scope, aliases, allowed, socket)
        .map_err(|e| invalid(&e.to_string()))?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let outcome = runtime.block_on(async {
        let server = Server {
            context: Arc::new(context),
            reads: Arc::new(Semaphore::new(4)),
            mutation: Arc::new(Semaphore::new(1)),
        };
        let (read, write) = rmcp::transport::stdio_pipes();
        let transport = rmcp::transport::async_rw::AsyncRwTransport::new_server(read, write)
            .with_max_line_length(1024 * 1024);
        let service = server
            .serve(transport)
            .await
            .map_err(std::io::Error::other)?;
        service.waiting().await.map_err(std::io::Error::other)?;
        Ok::<_, std::io::Error>(())
    });
    runtime.shutdown_timeout(Duration::from_secs(5));
    outcome?;
    Ok(0)
}
