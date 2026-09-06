//! One async owner per MCP server, shared by all shell entry points.
use super::*;
use anyhow::bail;
use rmcp::{RoleClient, service::RunningService};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Default)]
pub(super) struct Notifications(Arc<AtomicBool>);
impl rmcp::ClientHandler for Notifications {
    async fn on_tool_list_changed(&self, _context: rmcp::service::NotificationContext<RoleClient>) {
        self.0.store(true, Ordering::Relaxed);
    }
}
type Service = RunningService<RoleClient, Notifications>;
struct Request {
    operation: Value,
    result: std::sync::mpsc::Sender<Result<Value>>,
    cancel: Arc<AtomicBool>,
}
pub(super) struct Connection {
    sender: tokio::sync::mpsc::UnboundedSender<Request>,
    changed: Arc<AtomicBool>,
}
impl Connection {
    pub fn new(transport: McpTransport) -> Self {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<Request>();
        let changed = Arc::new(AtomicBool::new(false));
        let notifications = Notifications(changed.clone());
        std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(async move {
                let mut service:Option<Service>=None;
                while let Some(request)=receiver.recv().await {
                    if request.cancel.load(Ordering::Relaxed) {continue;}
                    let operation=async {
                        if service.as_ref().is_some_and(Service::is_closed) {service=None;}
                        if service.is_none() {service=Some(open_with_notifications(transport.clone(), notifications.clone()).await?);}
                        let connected=service.as_ref().expect("connected");
                        match request.operation["kind"].as_str() {
                            Some("list") => Ok(serde_json::to_value(connected.list_all_tools().await?)?),
                            Some("task_get")=>Ok(serde_json::to_value(connected.get_task(serde_json::from_value(request.operation["params"].clone())?).await?)?),
                            Some("task_cancel")=>{connected.cancel_task(serde_json::from_value(request.operation["params"].clone())?).await?;Ok(json!({"cancellation_requested":true}))},
                            Some("task_update")=>{connected.update_task(serde_json::from_value(request.operation["params"].clone())?).await?;Ok(json!({"input_delivered":true}))},
                            _=>{
                                let params:CallToolRequestParams=serde_json::from_value(request.operation["params"].clone())?;
                                if request.operation["tasks"]==true {
                                    match connected.call_tool_once(params).await? {
                                        rmcp::model::CallToolResponse::Complete(result)=>Ok(serde_json::to_value(result)?),
                                        rmcp::model::CallToolResponse::Task(task)=>Ok(serde_json::to_value(task)?),
                                        rmcp::model::CallToolResponse::InputRequired(input)=>Ok(serde_json::to_value(input)?),
                                        _=>bail!("unsupported MCP result; outcome unknown"),
                                    }
                                } else {Ok(serde_json::to_value(connected.call_tool(params).await?)?)}
                            }
                        }
                    };
                    let result=tokio::select! {
                        result=tokio::time::timeout(DEFAULT_TOOL_TIMEOUT,operation)=>result.map_err(|_|anyhow::anyhow!("MCP operation timed out; outcome unknown")).and_then(|r|r),
                        _=async {while !request.cancel.load(Ordering::Relaxed){tokio::time::sleep(Duration::from_millis(20)).await;}}=>Err(anyhow::anyhow!("MCP wait cancelled; outcome unknown")),
                    };
                    if result.is_err() && let Some(service)=service.take(){let _=tokio::time::timeout(Duration::from_secs(1),service.cancel()).await;}
                    let _=request.result.send(result.map_err(|error| error.context("MCP operation failed; outcome unknown")));
                }
                if let Some(service)=service {let _=tokio::time::timeout(Duration::from_secs(1),service.cancel()).await;}
            });
        });
        Self { sender, changed }
    }
    pub fn tools_changed(&self) -> bool {
        self.changed.load(Ordering::Relaxed)
    }
    pub fn mark_refreshed(&self) {
        self.changed.store(false, Ordering::Relaxed);
    }
    pub fn request(&self, operation: Value, cancelled: &dyn Fn() -> bool) -> Result<Value> {
        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let started = std::time::Instant::now();
        self.sender
            .send(Request {
                operation,
                result: tx,
                cancel: cancel.clone(),
            })
            .map_err(|_| anyhow::anyhow!("MCP worker closed"))?;
        loop {
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(result) => return result,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    bail!("MCP worker closed; outcome unknown")
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if cancelled() || started.elapsed() >= DEFAULT_TOOL_TIMEOUT {
                        cancel.store(true, Ordering::Relaxed);
                        bail!("MCP wait cancelled or deadline exceeded; outcome unknown");
                    }
                }
            }
        }
    }
}

pub(super) async fn open(transport: McpTransport) -> Result<Service> {
    open_with_notifications(transport, Notifications::default()).await
}
async fn open_with_notifications(
    transport: McpTransport,
    handler: Notifications,
) -> Result<Service> {
    match transport {
        McpTransport::Stdio {
            command,
            args,
            env,
            cwd,
        } => {
            let mut cmd = Command::new(&command);
            cmd.args(args).env_clear();
            for key in ["PATH", "HOME", "LANG", "LC_ALL", "TMPDIR"] {
                if let Some(value) = std::env::var_os(key) {
                    cmd.env(key, value);
                }
            }
            cmd.envs(env);
            if let Some(cwd) = cwd {
                cmd.current_dir(cwd);
            }
            Ok(handler
                .serve(spawn_mcp_stdio_transport(cmd, &command)?)
                .await?)
        }
        McpTransport::Http {
            url,
            auth_header,
            allow_stateless,
        } => {
            let mut config = StreamableHttpClientTransportConfig::with_uri(Arc::from(url.as_str()));
            if let Some(header) = auth_header {
                config = config.auth_header(header);
            }
            if let Some(allow) = allow_stateless {
                config.allow_stateless = allow;
            }
            Ok(handler
                .serve(StreamableHttpClientTransport::from_config(config))
                .await?)
        }
        McpTransport::Sse { .. } => anyhow::bail!(LEGACY_SSE_UNSUPPORTED_MESSAGE),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stdio_reuses_process_and_does_not_replay_cancelled_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("server.py");
        let counter = dir.path().join("calls");
        std::fs::write(&script, r#"
import json, os, sys, time
for line in sys.stdin:
    request = json.loads(line)
    if 'id' not in request: continue
    method = request['method']
    if method == 'initialize':
        result = {'protocolVersion':request['params']['protocolVersion'], 'capabilities':{'tools':{}}, 'serverInfo':{'name':'fixture','version':'1'}}
    elif method == 'tools/list':
        result = {'tools':[{'name':'counter','inputSchema':{'type':'object'}}]}
    elif method == 'tools/call':
        with open(sys.argv[1], 'a') as out: out.write('called\n')
        if request['params'].get('arguments', {}).get('slow'): time.sleep(10)
        result = {'content':[{'type':'text','text':str(os.getpid())}]}
    else:
        result = {}
    print(json.dumps({'jsonrpc':'2.0','id':request['id'],'result':result}), flush=True)
"#).unwrap();
        let connection = Connection::new(McpTransport::Stdio {
            command: "python3".into(),
            args: vec![
                script.to_string_lossy().into(),
                counter.to_string_lossy().into(),
            ],
            env: Default::default(),
            cwd: None,
        });
        let call = json!({"kind":"call", "params":{"name":"counter","arguments":{}}});
        let first = connection.request(call.clone(), &|| false).unwrap();
        let second = connection.request(call, &|| false).unwrap();
        assert_eq!(first["content"][0]["text"], second["content"][0]["text"]);
        let started = std::time::Instant::now();
        let error = connection
            .request(
                json!({"kind":"call","params":{"name":"counter","arguments":{"slow":true}}}),
                &|| started.elapsed() > Duration::from_millis(200),
            )
            .unwrap_err();
        assert!(error.to_string().contains("outcome unknown"));
        assert_eq!(std::fs::read_to_string(counter).unwrap().lines().count(), 3);
    }
}
