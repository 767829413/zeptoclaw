//! Gateway command handler (multi-channel bot server).

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use futures::FutureExt;
use tokio::sync::{mpsc, watch};
use tracing::{error, info, warn};

use zeptoclaw::bus::message::{
    OutboundMessageKind, OUTBOUND_CUSTOM_NAME_KEY, OUTBOUND_CUSTOM_PAYLOAD_KEY,
    OUTBOUND_CUSTOM_SUMMARY_KEY,
};
use zeptoclaw::bus::{InboundInterceptor, MessageBus, OutboundMessage};
use zeptoclaw::channels::{register_configured_channels, ChannelManager};
use zeptoclaw::config::watcher::ConfigWatcher;
use zeptoclaw::config::{Config, ContainerAgentBackend};
use zeptoclaw::health::{
    health_port, start_health_server, start_health_server_legacy, start_periodic_usage_flush,
    HealthRegistry, UsageMetrics,
};
use zeptoclaw::heartbeat::{ensure_heartbeat_file, HeartbeatService};
use zeptoclaw::providers::{
    configured_provider_names, resolve_runtime_provider, RUNTIME_SUPPORTED_PROVIDERS,
};
use zeptoclaw::tools::approval::{
    ApprovalRequest, ApprovalResponse, DEFAULT_APPROVAL_TIMEOUT_SECS,
};
use zeptoclaw::tools::approval_broker::ApprovalBroker;

use super::common::create_agent;
use super::heartbeat::heartbeat_file_path;

/// Start multi-channel gateway.
pub(crate) async fn cmd_gateway(
    containerized_flag: Option<String>,
    tunnel_flag: Option<String>,
) -> Result<()> {
    println!("Starting ZeptoClaw Gateway...");

    // Load configuration
    let mut config = Config::load().with_context(|| "Failed to load configuration")?;

    // Startup guard — check for consecutive crash degradation
    let guard = if config.gateway.startup_guard.enabled {
        let g = zeptoclaw::StartupGuard::new(
            config.gateway.startup_guard.crash_threshold,
            config.gateway.startup_guard.window_secs,
        );
        match g.check() {
            Ok(true) => {
                warn!(
                    threshold = config.gateway.startup_guard.crash_threshold,
                    "Startup guard: consecutive crashes detected — entering degraded mode"
                );
                warn!("Degraded mode: shell and filesystem write tools disabled");
                config.tools.deny = vec![
                    "shell".to_string(),
                    "write_file".to_string(),
                    "edit_file".to_string(),
                ];
            }
            Ok(false) => {}
            Err(e) => warn!("Startup guard check failed (continuing normally): {}", e),
        }
        Some(g)
    } else {
        None
    };

    // --containerized [docker|apple] overrides config backend
    let containerized = containerized_flag.is_some();
    if let Some(ref b) = containerized_flag {
        if b != "auto" {
            config.container_agent.backend = match b.to_lowercase().as_str() {
                "docker" => ContainerAgentBackend::Docker,
                #[cfg(target_os = "macos")]
                "apple" => ContainerAgentBackend::Apple,
                "auto" => ContainerAgentBackend::Auto,
                other => {
                    #[cfg(target_os = "macos")]
                    return Err(anyhow::anyhow!(
                        "Unknown backend '{}'. Use: docker or apple",
                        other
                    ));
                    #[cfg(not(target_os = "macos"))]
                    return Err(anyhow::anyhow!("Unknown backend '{}'. Use: docker", other));
                }
            };
        }
    }

    // Start tunnel if requested
    let mut _tunnel: Option<Box<dyn zeptoclaw::tunnel::TunnelProvider>> = None;
    let tunnel_provider = tunnel_flag.or(config.tunnel.provider.clone());
    if let Some(ref provider) = tunnel_provider {
        let mut tunnel_config = config.tunnel.clone();
        tunnel_config.provider = Some(provider.clone());
        let mut t = zeptoclaw::tunnel::create_tunnel(&tunnel_config)
            .with_context(|| format!("Failed to create {} tunnel", provider))?;

        let gateway_port = config.gateway.port;
        let tunnel_url = t
            .start(gateway_port)
            .await
            .with_context(|| format!("Failed to start {} tunnel", provider))?;

        println!("Tunnel active: {}", tunnel_url);
        _tunnel = Some(t);
    }

    // Create message bus
    let bus = Arc::new(MessageBus::new());

    // Create usage metrics tracker
    let metrics = Arc::new(UsageMetrics::new());

    // Start legacy health check server (liveness + readiness via UsageMetrics)
    let hp = health_port();
    let health_handle = match start_health_server_legacy(hp, Arc::clone(&metrics)).await {
        Ok(handle) => {
            info!(
                port = hp,
                "Health endpoints available at /healthz and /readyz"
            );
            Some(handle)
        }
        Err(e) => {
            warn!(error = %e, "Failed to start health server (non-fatal)");
            None
        }
    };

    // Create HealthRegistry (shared between health server and channel supervisor)
    let health_registry = HealthRegistry::new();
    health_registry.set_metrics(Arc::clone(&metrics));

    // Start HealthRegistry-based server if config.health.enabled
    if config.health.enabled {
        let registry = health_registry.clone();
        let host = config.health.host.clone();
        let port = config.health.port;
        tokio::spawn(async move {
            match start_health_server(&host, port, registry).await {
                Ok(handle) => {
                    info!(
                        host = %host,
                        port = port,
                        "Named-check health server listening on /health and /ready"
                    );
                    let _ = handle.await;
                }
                Err(e) => {
                    tracing::error!("Named-check health server error: {}", e);
                }
            }
        });
        info!(
            "Health server enabled on {}:{}",
            config.health.host, config.health.port
        );
    }

    // Create shutdown watch channel for periodic usage flush
    let (usage_shutdown_tx, usage_shutdown_rx) = tokio::sync::watch::channel(false);
    let usage_flush_handle = start_periodic_usage_flush(Arc::clone(&metrics), usage_shutdown_rx);

    // Determine agent backend: containerized or in-process
    let mut proxy = None;
    let proxy_handle = if containerized {
        info!("Starting gateway with containerized agent mode");

        // Resolve backend (auto-detect or explicit from config)
        let backend = zeptoclaw::gateway::resolve_backend(&config.container_agent)
            .await
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        info!("Resolved container backend: {}", backend);

        // Validate the resolved backend
        match backend {
            zeptoclaw::gateway::ResolvedBackend::Docker => {
                validate_docker_available(configured_docker_binary(&config.container_agent))
                    .await?;
            }
            #[cfg(target_os = "macos")]
            zeptoclaw::gateway::ResolvedBackend::Apple => {
                validate_apple_available().await?;
            }
        }

        // Check image exists (Docker-specific)
        let image = &config.container_agent.image;
        if backend == zeptoclaw::gateway::ResolvedBackend::Docker {
            let docker_binary = configured_docker_binary(&config.container_agent);
            let image_check = tokio::process::Command::new(docker_binary)
                .args(["image", "inspect", image])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .await;

            if !image_check.map(|s| s.success()).unwrap_or(false) {
                eprintln!(
                    "Warning: Docker image '{}' not found (checked via '{}').",
                    image, docker_binary
                );
                eprintln!("Build it with: {} build -t {} .", docker_binary, image);
                return Err(anyhow::anyhow!(
                    "Docker image '{}' not found (checked via '{}')",
                    image,
                    docker_binary
                ));
            }
        }

        info!("Using container image: {} (backend={})", image, backend);

        let proxy_instance = Arc::new(zeptoclaw::gateway::ContainerAgentProxy::new(
            config.clone(),
            bus.clone(),
            backend,
        ));
        proxy_instance.set_usage_metrics(Arc::clone(&metrics));
        let proxy_for_task = Arc::clone(&proxy_instance);
        let proxy_metrics = Arc::clone(&metrics);
        let proxy_guard = guard.clone();
        proxy = Some(proxy_instance);

        Some(tokio::spawn(async move {
            let result = proxy_for_task.start().await;
            proxy_metrics.set_ready(false);
            match result {
                Err(e) => {
                    error!("Container agent proxy error: {}", e);
                    if let Some(ref g) = proxy_guard {
                        if let Err(re) = g.record_crash() {
                            warn!("Failed to record crash: {}", re);
                        }
                    }
                }
                Ok(()) => warn!("Container agent proxy stopped"),
            }
        }))
    } else {
        // Validate provider for in-process mode
        let runtime_provider_name = resolve_runtime_provider(&config).map(|provider| provider.name);
        if runtime_provider_name.is_none() {
            let configured = configured_provider_names(&config);
            if configured.is_empty() {
                error!("No AI provider configured. Set ZEPTOCLAW_PROVIDERS_ANTHROPIC_API_KEY");
                error!("or add your API key to {:?}", Config::path());
            } else {
                error!(
                    "Configured provider(s) are not supported by this runtime: {}",
                    configured.join(", ")
                );
                error!(
                    "Currently supported runtime providers: {}",
                    RUNTIME_SUPPORTED_PROVIDERS.join(", ")
                );
            }
            std::process::exit(1);
        }
        None
    };

    // Preflight: validate model-provider compatibility before starting anything.
    if !containerized {
        let model_diags = zeptoclaw::config::validate::validate_model_provider_compat(&config);
        for diag in &model_diags {
            match diag.level {
                zeptoclaw::config::validate::DiagnosticLevel::Error => {
                    error!("{}", diag);
                }
                zeptoclaw::config::validate::DiagnosticLevel::Warn => {
                    warn!("{}", diag);
                }
                zeptoclaw::config::validate::DiagnosticLevel::Ok => {
                    info!("{}", diag);
                }
            }
        }
        let has_errors = model_diags
            .iter()
            .any(|d| d.level == zeptoclaw::config::validate::DiagnosticLevel::Error);
        if has_errors {
            eprintln!();
            eprintln!("ERROR: Model-provider mismatch detected.");
            eprintln!(
                "  Fix your config ({:?}) or run 'zeptoclaw onboard'.",
                Config::path()
            );
            eprintln!("  Run 'zeptoclaw config check' for details.");
            eprintln!();
            std::process::exit(1);
        }
    }

    // Create in-process agent (only needed when not containerized)
    let mut agent = if !containerized {
        let agent = create_agent(config.clone(), bus.clone()).await?;
        agent.set_usage_metrics(Arc::clone(&metrics)).await;
        Some(agent)
    } else {
        None
    };

    // --- Tool-approval handler (generic, works for all channels) ----------
    let broker = Arc::new(ApprovalBroker::new());

    // Inbound interceptor: consume "yes/no" replies when an approval is pending.
    bus.set_inbound_interceptor({
        let broker = Arc::clone(&broker);
        Arc::new(move |msg: &zeptoclaw::bus::InboundMessage| {
            let Some((approved, all)) = parse_approval_reply(&msg.content) else {
                return false;
            };
            match msg.metadata.get("inject_kind").map(String::as_str) {
                Some("approval_response") => {
                    let Some(request_id) = msg.metadata.get("request_id").map(String::as_str)
                    else {
                        return false;
                    };
                    if all {
                        if !broker.has_pending_request(&msg.chat_id, request_id) {
                            return false;
                        }
                        broker.resolve_all(&msg.chat_id, approved) > 0
                    } else {
                        broker.resolve(&msg.chat_id, request_id, approved)
                    }
                }
                Some(_) => false,
                None if all => broker.resolve_all(&msg.chat_id, approved) > 0,
                None => broker.resolve_oldest(&msg.chat_id, approved),
            }
        }) as InboundInterceptor
    });

    let approval_timeout = resolve_approval_timeout(&config);
    if let Some(ref agent) = agent {
        register_approval_handler(agent, &broker, &bus, approval_timeout).await;
    }

    // Create channel manager with health supervision
    let mut channel_manager = ChannelManager::new(bus.clone(), config.clone());
    channel_manager.set_health_registry(health_registry.clone());

    // Register channels via factory.
    let channel_count = register_configured_channels(&channel_manager, bus.clone(), &config).await;
    if channel_count == 0 {
        warn!(
            "No channels configured. Enable channels in {:?}",
            Config::path()
        );
        warn!("The agent loop will still run but won't receive messages from external sources.");
    } else {
        info!("Registered {} channel(s)", channel_count);
    }

    // Start all channels
    channel_manager
        .start_all()
        .await
        .with_context(|| "Failed to start channels")?;

    let heartbeat_service = if config.heartbeat.enabled {
        let hb_path = heartbeat_file_path(&config);
        match ensure_heartbeat_file(&hb_path).await {
            Ok(true) => info!("Created heartbeat file template at {:?}", hb_path),
            Ok(false) => {}
            Err(e) => warn!("Failed to initialize heartbeat file {:?}: {}", hb_path, e),
        }

        let (hb_channel, hb_chat_id) = config
            .heartbeat
            .deliver_to
            .as_deref()
            .and_then(|s| {
                let parsed = parse_deliver_to(s);
                if parsed.is_none() {
                    warn!(
                        "heartbeat.deliver_to {:?} is not in 'channel:chat_id' format; \
                         falling back to pseudo-channel",
                        s
                    );
                }
                parsed
            })
            .unwrap_or_else(|| ("heartbeat".to_string(), "system".to_string()));

        let service = Arc::new(HeartbeatService::new(
            hb_path,
            config.heartbeat.interval_secs,
            bus.clone(),
            &hb_channel,
            &hb_chat_id,
        ));
        service.start().await?;
        Some(service)
    } else {
        None
    };

    // Start memory hygiene scheduler
    let _hygiene_handle = match zeptoclaw::memory::longterm::LongTermMemory::new() {
        Ok(ltm) => {
            let ltm = Arc::new(tokio::sync::Mutex::new(ltm));
            Some(zeptoclaw::memory::hygiene::start_hygiene_scheduler(
                ltm,
                config.memory.hygiene.clone(),
            ))
        }
        Err(e) => {
            warn!("Memory hygiene scheduler not started: {}", e);
            None
        }
    };

    // Start device service if configured
    // TODO: publish to MessageBus for channel delivery once InboundMessage wrapping is settled
    let _device_handle =
        zeptoclaw::devices::DeviceService::new(config.devices.enabled, config.devices.monitor_usb)
            .start()
            .map(|mut rx| {
                tokio::spawn(async move {
                    while let Some(event) = rx.recv().await {
                        tracing::info!("Device event: {}", event.format_message());
                    }
                })
            });

    // Start agent loop in background (only for in-process mode)
    let mut agent_handle = if let Some(ref agent) = agent {
        let agent_clone = Arc::clone(agent);
        let agent_metrics = Arc::clone(&metrics);
        let agent_guard = guard.clone();
        Some(tokio::spawn(async move {
            let result = agent_clone.start().await;
            agent_metrics.set_ready(false);
            match result {
                Err(e) => {
                    error!("Agent loop error: {}", e);
                    if let Some(ref g) = agent_guard {
                        if let Err(re) = g.record_crash() {
                            warn!("Failed to record crash: {}", re);
                        }
                    }
                }
                Ok(()) => warn!("Agent loop stopped"),
            }
        }))
    } else {
        None
    };

    // Mark gateway as ready for /readyz
    metrics.set_ready(true);

    // Record clean start (reset crash counter)
    if let Some(ref g) = guard {
        if let Err(e) = g.record_clean_start() {
            warn!("Failed to record clean start: {}", e);
        }
    }

    println!();
    if !config.tools.deny.is_empty() {
        println!("  WARNING: DEGRADED MODE — dangerous tools disabled after consecutive crashes");
        println!("  Fix the underlying issue and restart to clear.");
        println!();
    }
    if containerized {
        println!("Gateway is running (containerized mode). Press Ctrl+C to stop.");
    } else {
        println!("Gateway is running. Press Ctrl+C to stop.");
    }
    println!();

    // Config watcher (30s polling) for hot-reload.
    let (reload_tx, mut reload_rx) = mpsc::unbounded_channel::<Config>();
    let (reload_shutdown_tx, reload_shutdown_rx) = watch::channel(false);
    let watcher_handle = tokio::spawn(
        ConfigWatcher::default_path(Duration::from_secs(30)).watch(reload_tx, reload_shutdown_rx),
    );

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                break;
            }
            maybe_cfg = reload_rx.recv() => {
                let Some(new_config) = maybe_cfg else {
                    break;
                };

                let changed_sections = diff_hot_reload_sections(&config, &new_config);
                if changed_sections.is_empty() {
                    continue;
                }

                info!(
                    sections = %changed_sections.join(", "),
                    "Applying hot-reloaded config sections"
                );

                let old_config = config.clone();
                config = new_config;

                // Rebuild in-process agent to apply provider + safety changes.
                if !containerized {
                    if let Some(ref running_agent) = agent {
                        running_agent.stop();
                        running_agent.shutdown_mcp_clients().await;
                    }
                    if let Some(handle) = agent_handle.take() {
                        let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
                    }

                    match create_agent(config.clone(), bus.clone()).await {
                        Ok(new_agent) => {
                            new_agent.set_usage_metrics(Arc::clone(&metrics)).await;
                            register_approval_handler(&new_agent, &broker, &bus, approval_timeout).await;
                            let agent_clone = Arc::clone(&new_agent);
                            let agent_metrics = Arc::clone(&metrics);
                            let agent_guard = guard.clone();
                            agent_handle = Some(tokio::spawn(async move {
                                let result = agent_clone.start().await;
                                agent_metrics.set_ready(false);
                                match result {
                                    Err(e) => {
                                        error!("Agent loop error: {}", e);
                                        if let Some(ref g) = agent_guard {
                                            if let Err(re) = g.record_crash() {
                                                warn!("Failed to record crash: {}", re);
                                            }
                                        }
                                    }
                                    Ok(()) => warn!("Agent loop stopped"),
                                }
                            }));
                            agent = Some(new_agent);
                        }
                        Err(e) => {
                            config = old_config;
                            warn!("Hot-reload failed to rebuild agent, keeping prior config: {}", e);
                            continue;
                        }
                    }
                } else {
                    warn!("Config hot-reload for containerized mode is not yet supported");
                }

                // Rebuild channels only if channel config actually changed.
                if changed_sections.contains(&"channels") {
                    if let Err(e) = channel_manager.stop_all().await {
                        warn!("Failed to stop channels during hot-reload: {}", e);
                    }
                    let mut new_manager = ChannelManager::new(bus.clone(), config.clone());
                    new_manager.set_health_registry(health_registry.clone());
                    let count = register_configured_channels(&new_manager, bus.clone(), &config).await;
                    if count == 0 {
                        warn!("No channels configured after hot-reload");
                    }
                    if let Err(e) = new_manager.start_all().await {
                        config = old_config;
                        warn!("Failed to start channels after hot-reload, keeping previous config: {}", e);
                        continue;
                    }
                    channel_manager = new_manager;
                }
            }
        }
    }

    println!();
    println!("Shutting down...");

    // Mark not ready immediately
    metrics.set_ready(false);

    // Signal usage flush to emit final summary
    let _ = usage_shutdown_tx.send(true);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), usage_flush_handle).await;

    if let Some(service) = &heartbeat_service {
        service.stop().await;
    }

    // Stop agent or proxy
    if let Some(ref agent) = agent {
        agent.stop();
        agent.shutdown_mcp_clients().await;
    }
    if let Some(ref proxy) = proxy {
        proxy.stop();
    }

    // Stop all channels
    channel_manager
        .stop_all()
        .await
        .with_context(|| "Failed to stop channels")?;

    // Stop config watcher
    let _ = reload_shutdown_tx.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(2), watcher_handle).await;

    // Wait for agent/proxy to stop
    if let Some(handle) = agent_handle {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;
    }
    if let Some(handle) = proxy_handle {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;
    }

    // Stop health server
    if let Some(handle) = health_handle {
        handle.abort();
    }

    println!("Gateway stopped.");
    Ok(())
}

/// Validate that Docker is available.
async fn validate_docker_available(docker_binary: &str) -> Result<()> {
    if !zeptoclaw::gateway::is_docker_available_with_binary(docker_binary).await {
        return Err(anyhow::anyhow!(
            "Docker is not available via '{}'. Install Docker or run without --containerized.",
            docker_binary
        ));
    }
    Ok(())
}

fn configured_docker_binary(config: &zeptoclaw::config::ContainerAgentConfig) -> &str {
    config
        .docker_binary
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("docker")
}

/// Validate that Apple Container is available (macOS only).
#[cfg(target_os = "macos")]
async fn validate_apple_available() -> Result<()> {
    if !zeptoclaw::gateway::is_apple_container_available().await {
        return Err(anyhow::anyhow!(
            "Apple Container is not available. Requires macOS 15+ with `container` CLI installed."
        ));
    }
    Ok(())
}

/// Parse a `deliver_to` string in `"channel:chat_id"` format.
/// Returns `None` if the string is missing a colon or either part is empty.
fn parse_deliver_to(s: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = s.splitn(2, ':').collect();
    if parts.len() == 2 && !parts[0].is_empty() && !parts[1].is_empty() {
        Some((parts[0].to_string(), parts[1].to_string()))
    } else {
        None
    }
}

fn diff_hot_reload_sections(old: &Config, new: &Config) -> Vec<&'static str> {
    let mut changed = Vec::new();
    if section_changed(&old.providers, &new.providers) {
        changed.push("providers");
    }
    if section_changed(&old.channels, &new.channels) {
        changed.push("channels");
    }
    if section_changed(&old.safety, &new.safety) {
        changed.push("safety");
    }
    if section_changed(&old.agents, &new.agents) {
        changed.push("agents");
    }
    changed
}

fn section_changed<T: serde::Serialize>(old: &T, new: &T) -> bool {
    serde_json::to_value(old).ok() != serde_json::to_value(new).ok()
}

fn resolve_approval_timeout(config: &Config) -> u64 {
    let t = config.approval.approval_timeout_secs;
    if t > 0 {
        t
    } else {
        DEFAULT_APPROVAL_TIMEOUT_SECS
    }
}

fn parse_approval_reply(content: &str) -> Option<(bool, bool)> {
    let text = content.trim().to_lowercase();
    let all = text.ends_with(" all") || text == "all" || text == "全部";
    let normalized = text
        .trim_end_matches(" all")
        .trim_end_matches("全部")
        .trim();
    let approved = match normalized {
        "yes" | "y" | "approve" | "是" => true,
        "no" | "n" | "deny" | "否" => false,
        _ => return None,
    };
    Some((approved, all))
}

fn is_acp_channel(channel: &str) -> bool {
    channel == "acp" || channel == "acp_http"
}

fn approval_decision_label(outcome: &ApprovalResponse) -> Option<&'static str> {
    match outcome {
        ApprovalResponse::Approved => Some("approve"),
        ApprovalResponse::Denied(_) => Some("deny"),
        ApprovalResponse::TimedOut => None,
    }
}

async fn register_approval_handler(
    agent: &zeptoclaw::agent::AgentLoop,
    broker: &Arc<ApprovalBroker>,
    bus: &Arc<MessageBus>,
    timeout_secs: u64,
) {
    let broker = Arc::clone(broker);
    let bus = Arc::clone(bus);
    agent
        .set_approval_handler(move |request: ApprovalRequest| {
            let broker = Arc::clone(&broker);
            let bus = Arc::clone(&bus);
            async move {
                let chat_id = match request.chat_id.as_deref() {
                    Some(id) if !id.is_empty() => id.to_string(),
                    _ => return ApprovalResponse::Denied("No chat routing context for approval".into()),
                };
                let channel = match request.channel.as_deref() {
                    Some(ch) if !ch.is_empty() => ch.to_string(),
                    _ => return ApprovalResponse::Denied("No channel routing context for approval".into()),
                };

                let request_id = ulid::Ulid::new().to_string();
                let rx = broker.register(&chat_id, &request_id);

                // Format the approval prompt. Shell commands get a clean code-block
                // display; other tools show pretty-printed JSON arguments.
                let shell_command = request
                    .arguments
                    .get("command")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let shell_timeout_secs = request.arguments.get("timeout").and_then(|v| v.as_u64());
                let args_display = if request.tool_name == "shell" {
                    let cmd = shell_command.as_deref().unwrap_or("<no command>");
                    let timeout = shell_timeout_secs
                        .map(|t| format!("  timeout: {}s", t))
                        .unwrap_or_default();
                    format!(
                        "```sh\n{}\n```{}",
                        cmd,
                        if timeout.is_empty() {
                            String::new()
                        } else {
                            format!("\n{}", timeout)
                        }
                    )
                } else {
                    let pretty = serde_json::to_string_pretty(&request.arguments)
                        .unwrap_or_else(|_| request.arguments.to_string());
                    format!("```json\n{}\n```", pretty)
                };

                let pending = broker.pending_count(&chat_id);
                let batch_hint = if pending > 1 {
                    format!("\n({} more pending — reply **yes all** / **no all** to resolve all at once)", pending)
                } else {
                    String::new()
                };

                let prompt = format!(
                    "**[Approval Required]** — `{}`\n{}\nReply **yes** / **no** (or **yes all** / **no all**).{}",
                    request.tool_name,
                    args_display,
                    batch_hint,
                );
                if is_acp_channel(&channel) {
                    let mut payload = serde_json::json!({
                        "requestId": request_id.clone(),
                        "toolName": request.tool_name,
                        "arguments": request.arguments,
                        "pendingTotal": pending,
                    });
                    if let Some(cmd) = shell_command.as_deref() {
                        payload["shellCommand"] = serde_json::Value::String(cmd.to_string());
                    }
                    if let Some(timeout) = shell_timeout_secs {
                        payload["timeoutSecs"] = serde_json::Value::Number(timeout.into());
                    }
                    let payload_text = payload.to_string();
                    let msg = OutboundMessage::new(&channel, &chat_id, "")
                        .with_kind(OutboundMessageKind::Custom)
                        .with_metadata(OUTBOUND_CUSTOM_NAME_KEY, "ui:approval_request")
                        .with_metadata(OUTBOUND_CUSTOM_PAYLOAD_KEY, &payload_text)
                        .with_metadata(OUTBOUND_CUSTOM_SUMMARY_KEY, &prompt);
                    let _ = bus.publish_outbound(msg).await;
                } else {
                    let _ = bus
                        .publish_outbound(OutboundMessage::new(&channel, &chat_id, &prompt))
                        .await;
                }

                let outcome =
                    match tokio::time::timeout(Duration::from_secs(timeout_secs), rx).await {
                    Ok(Ok(true)) => ApprovalResponse::Approved,
                    Ok(Ok(false)) => ApprovalResponse::Denied("User denied".into()),
                    _ => ApprovalResponse::TimedOut,
                };

                if is_acp_channel(&channel) {
                    if let Some(decision) = approval_decision_label(&outcome) {
                        let payload = serde_json::json!({
                            "requestId": request_id,
                            "decision": decision,
                        });
                        let payload_text = payload.to_string();
                        let msg = OutboundMessage::new(&channel, &chat_id, "")
                            .with_kind(OutboundMessageKind::Custom)
                            .with_metadata(OUTBOUND_CUSTOM_NAME_KEY, "ui:approval_resolved")
                            .with_metadata(OUTBOUND_CUSTOM_PAYLOAD_KEY, &payload_text);
                        let _ = bus.publish_outbound(msg).await;
                    }
                }
                outcome
            }
            .boxed()
        })
        .await;
    info!(
        "Registered generic tool-approval handler (timeout={}s)",
        timeout_secs
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_deliver_to_valid() {
        assert_eq!(
            parse_deliver_to("telegram:123456789"),
            Some(("telegram".to_string(), "123456789".to_string()))
        );
    }

    #[test]
    fn test_parse_deliver_to_with_colon_in_chat_id() {
        // chat IDs with colons in them should still parse (splitn(2,...))
        assert_eq!(
            parse_deliver_to("discord:guild:123"),
            Some(("discord".to_string(), "guild:123".to_string()))
        );
    }

    #[test]
    fn test_parse_deliver_to_invalid() {
        assert_eq!(parse_deliver_to("no-colon"), None);
        assert_eq!(parse_deliver_to(":empty_channel"), None);
        assert_eq!(parse_deliver_to("empty_chat:"), None);
    }

    #[test]
    fn test_diff_hot_reload_sections() {
        let old = Config::default();
        let mut new = old.clone();
        new.safety.enabled = !new.safety.enabled;
        new.gateway.port += 1; // non-hot-reload section should be ignored

        let changed = diff_hot_reload_sections(&old, &new);
        assert!(changed.contains(&"safety"));
        assert!(!changed.contains(&"gateway"));
    }

    #[test]
    fn test_parse_approval_reply_variants() {
        assert_eq!(parse_approval_reply("yes"), Some((true, false)));
        assert_eq!(parse_approval_reply("no"), Some((false, false)));
        assert_eq!(parse_approval_reply("approve all"), Some((true, true)));
        assert_eq!(parse_approval_reply("deny all"), Some((false, true)));
        assert_eq!(parse_approval_reply("是"), Some((true, false)));
        assert_eq!(parse_approval_reply("否"), Some((false, false)));
        assert_eq!(parse_approval_reply("maybe"), None);
    }
}
