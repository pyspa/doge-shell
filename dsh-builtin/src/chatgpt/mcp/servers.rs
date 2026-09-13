//! The server set's lifecycle: building it from the configured servers,
//! adding, replacing and removing one, syncing the whole list against a
//! desired state, discovering each server's tools, and the blocking bridge
//! that runs all of this from non-async callers.
use super::*;

impl McpManager {
    pub(super) async fn build_from_servers(runtime_servers: Vec<McpServerConfig>) -> Result<Self> {
        let mut servers = Vec::new();
        let mut labels = HashSet::new();
        let mut warnings = Vec::new();

        // Dedup and validate first
        let mut valid_configs = Vec::new();
        for server in runtime_servers {
            if server.label.trim().is_empty() {
                warnings.push("skipped MCP server with empty label".to_string());
                continue;
            }
            if !labels.insert(server.label.clone()) {
                warnings.push(format!(
                    "skipped MCP server `{}` because the label is duplicated",
                    server.label
                ));
                continue;
            }
            valid_configs.push(server);
        }

        let mut cache = McpToolCache::load();
        let mut cache_changed = false;

        // Prepare futures for parallel execution
        let mut futures = Vec::new();

        for config in valid_configs {
            let hash = hash_server_config(&config);
            let cached_tools: Option<Vec<Tool>> =
                if let Some(entry) = cache.entries.get(&config.label) {
                    if entry.config_hash == hash
                        && chrono::Utc::now()
                            .timestamp()
                            .saturating_sub(entry.timestamp)
                            < 300
                    {
                        debug!("Loaded tools for {} from cache", config.label);
                        Some(entry.tools.clone())
                    } else {
                        None
                    }
                } else {
                    None
                };

            let probes_transport = cached_tools.is_none();
            futures.push((probes_transport, async move {
                if let Some(tools) = cached_tools {
                    Ok((config, tools, false)) // false = not new
                } else {
                    match list_tools_via_transport(config.transport.clone()).await {
                        Ok(tools) => Ok((config, tools, true)), // true = new/refreshed
                        Err(e) => Err((config.label, e)),
                    }
                }
            }));
        }

        // Each future is awaited to completion before the next starts, and
        // `list_tools_via_transport` cancels its service before returning, so
        // only one probe process is ever alive -- the delay is not what
        // enforces that. What it buys is a pause between consecutive child
        // spawns so a long server list does not monopolise CPU/IO while the
        // shell is still coming up. It is therefore paid only between real
        // probes: a cache hit spawns nothing, and making it wait was adding
        // 200ms per configured server to startup for no reason.
        //
        // If startup latency matters more than the pause, the answer is to run
        // the probes concurrently under a real limit, not to shorten this.
        let mut results = Vec::new();
        let mut probed_before = false;
        for (probes_transport, future) in futures {
            if probes_transport && probed_before {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            probed_before |= probes_transport;
            results.push(future.await);
        }

        for res in results {
            match res {
                Ok((config, tools, updated)) => {
                    if updated {
                        let hash = hash_server_config(&config);
                        cache.entries.insert(
                            config.label.clone(),
                            ToolCacheEntry {
                                config_hash: hash,
                                timestamp: chrono::Utc::now().timestamp(),
                                tools: tools.clone(),
                            },
                        );
                        cache_changed = true;
                    }

                    debug!(
                        server = config.label.as_str(),
                        tool_count = tools.len(),
                        "registered MCP server"
                    );

                    servers.push(McpServer {
                        label: config.label,
                        description: config.description,
                        transport: config.transport,
                        tools,
                    });
                }
                Err((label, err)) => {
                    warnings.push(format!("failed to load MCP server `{}`: {}", label, err));
                }
            }
        }

        if cache_changed {
            cache.save();
        }

        let mut bindings = BTreeMap::new();

        for server in &servers {
            for tool in &server.tools {
                let base_name = format!(
                    "mcp__{}__{}",
                    sanitize_identifier(&server.label),
                    sanitize_identifier(tool.name.as_ref())
                );
                let function_name = stable_name(&base_name, &server.label, tool.name.as_ref());
                bindings.insert(
                    function_name.clone(),
                    ToolBinding {
                        server_label: server.label.clone(),
                        tool_name: tool.name.to_string(),
                        function_name,
                    },
                );
            }
        }

        Ok(Self {
            servers,
            bindings,
            warnings,
            session_meta: RwLock::new(HashMap::new()),
            connection_errors: RwLock::new(HashMap::new()),
            disabled: RwLock::new(HashSet::new()),
            connections: parking_lot::Mutex::new(HashMap::new()),
            tools_refreshed: Instant::now(),
        })
    }

    pub async fn add_server(&mut self, config: McpServerConfig) -> Result<()> {
        let (server_struct, tools) = Self::load_server_tools(config).await?;
        self.register_server(server_struct, tools)?;
        Ok(())
    }

    pub fn add_server_blocking(&mut self, config: McpServerConfig) -> Result<()> {
        let config_clone = config.clone();
        let (server_struct, tools) = Self::execute_async_with_loader(move || async move {
            Self::load_server_tools(config_clone).await
        })?;
        self.register_server(server_struct, tools)?;
        Ok(())
    }

    /// Remove a registered MCP server and its associated metadata.
    pub fn remove_server(&mut self, label: &str) -> bool {
        self.connections.lock().remove(label);
        let before = self.servers.len();
        self.servers.retain(|server| server.label != label);
        let removed = self.servers.len() != before;
        if removed {
            self.bindings
                .retain(|_, binding| binding.server_label != label);
            self.session_meta_write().remove(label);
            self.connection_errors_write().remove(label);
            self.disabled_write().remove(label);
        }
        removed
    }

    pub(super) fn replace_server_blocking(&mut self, config: McpServerConfig) -> Result<()> {
        let label = config.label.clone();
        let config_clone = config.clone();
        let (server_struct, tools) = Self::execute_async_with_loader(move || async move {
            Self::load_server_tools(config_clone).await
        })?;
        self.remove_server(&label);
        self.register_server(server_struct, tools)?;
        Ok(())
    }

    /// Synchronize the registered servers to the desired configuration set.
    ///
    /// This applies add/update/remove as a diff by label:
    /// - removed: label exists currently but not in desired
    /// - updated: label exists in both, but config changed
    /// - added: label exists only in desired
    pub fn sync_servers_blocking(&mut self, desired_servers: Vec<McpServerConfig>) -> McpSyncStats {
        let current_by_label: HashMap<String, McpServerConfig> = self
            .server_configs()
            .into_iter()
            .map(|config| (config.label.clone(), config))
            .collect();

        let mut desired_by_label: HashMap<String, McpServerConfig> = HashMap::new();
        for server in desired_servers {
            if server.label.trim().is_empty() {
                warn!("skipped MCP server with empty label during sync");
                continue;
            }
            if desired_by_label.contains_key(&server.label) {
                warn!(
                    "skipped duplicated MCP server label `{}` during sync",
                    server.label
                );
                continue;
            }
            desired_by_label.insert(server.label.clone(), server);
        }

        let mut stats = McpSyncStats::default();

        let mut labels_to_remove: Vec<String> = current_by_label
            .keys()
            .filter(|label| !desired_by_label.contains_key(*label))
            .cloned()
            .collect();
        labels_to_remove.sort_unstable();
        for label in labels_to_remove {
            if self.remove_server(&label) {
                stats.removed += 1;
            }
        }

        let mut desired_labels: Vec<String> = desired_by_label.keys().cloned().collect();
        desired_labels.sort_unstable();
        for label in desired_labels {
            let desired = match desired_by_label.remove(&label) {
                Some(server) => server,
                None => continue,
            };
            match current_by_label.get(&label) {
                Some(current) if current == &desired => {
                    stats.unchanged += 1;
                }
                Some(_) => match self.replace_server_blocking(desired) {
                    Ok(_) => stats.updated += 1,
                    Err(err) => {
                        warn!(server = label, "failed to update MCP server: {err}");
                    }
                },
                None => match self.add_server_blocking(desired) {
                    Ok(_) => stats.added += 1,
                    Err(err) => {
                        warn!(server = label, "failed to add MCP server: {err}");
                    }
                },
            }
        }

        stats
    }

    pub(super) async fn load_server_tools(
        config: McpServerConfig,
    ) -> Result<(McpServer, Vec<Tool>)> {
        let McpServerConfig {
            label,
            description,
            transport,
        } = config;

        let tools = list_tools_via_transport(transport.clone())
            .await
            .map_err(|err| anyhow::anyhow!("failed to load MCP server `{}`: {}", label, err))?;

        debug!(
            server = label.as_str(),
            tool_count = tools.len(),
            "registered MCP server"
        );

        let server_struct = McpServer {
            label: label.clone(),
            description,
            transport,
            tools: tools.clone(),
        };

        Ok((server_struct, tools))
    }

    pub(super) fn register_server(&mut self, server: McpServer, tools: Vec<Tool>) -> Result<()> {
        if self.servers.iter().any(|s| s.label == server.label) {
            return Err(anyhow::anyhow!(
                "MCP server `{}` already exists",
                server.label
            ));
        }

        // Update bindings
        for tool in &tools {
            let base_name = format!(
                "mcp__{}__{}",
                sanitize_identifier(&server.label),
                sanitize_identifier(tool.name.as_ref())
            );
            let function_name = stable_name(&base_name, &server.label, tool.name.as_ref());
            self.bindings.insert(
                function_name.clone(),
                ToolBinding {
                    server_label: server.label.clone(),
                    tool_name: tool.name.to_string(),
                    function_name,
                },
            );
        }

        // A re-registered label starts in service again: the disconnect the
        // user asked for applied to the server that has just been replaced.
        self.disabled_write().remove(&server.label);
        self.servers.push(server);
        Ok(())
    }

    pub(super) fn execute_async_with_loader<F, Fut, T>(f: F) -> Result<T>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<T>> + Send + 'static,
        T: Send + 'static,
    {
        // Always execute on a dedicated runtime in a dedicated thread.
        // This avoids nested-runtime panics from Handle::block_on when callers
        // are already inside a Tokio runtime.
        let (tx, rx) = std::sync::mpsc::channel();

        std::thread::spawn(move || {
            let res = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt.block_on(f()),
                Err(e) => Err(anyhow::anyhow!("failed to create async runtime: {}", e)),
            };
            let _ = tx.send(res);
        })
        .join()
        .map_err(|_| anyhow::anyhow!("MCP worker thread panicked"))?;

        rx.recv()
            .map_err(|e| anyhow::anyhow!("failed to receive result from MCP worker: {}", e))?
    }
}
