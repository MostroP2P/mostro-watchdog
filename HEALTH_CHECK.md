# Health Check / Heartbeat Notifications

This document describes the health monitoring and heartbeat functionality implemented in issue #6.

## Overview

mostro-watchdog includes comprehensive health monitoring to ensure the system is running properly and connected to all necessary services. This provides administrators with proactive alerts about system status.

## Features

### 🔔 Periodic Heartbeat Notifications

- **Purpose**: Confirm the bot is alive and processing events
- **Default interval**: 1 hour (3600 seconds) 
- **Content**: System uptime, events processed count, status confirmation
- **Configuration**: Can be enabled/disabled and interval adjusted

### 📊 Event Silence Monitoring

- **Purpose**: Alert if no dispute events are received for an extended period
- **Default threshold**: 2 hours (7200 seconds)
- **Intelligence**: Only alerts after the threshold period has passed since startup
- **Anti-spam**: Prevents duplicate alerts within the threshold period

### 🔌 Relay Connection Monitoring

- **Purpose**: Monitor Nostr relay connectivity and attempt automatic reconnection
- **Check interval**: Every 5 minutes
- **Actions**: Detects disconnected relays and attempts reconnection
- **Notifications**: None — issues are written to the application log only, so the
  Telegram channel stays limited to dispute events
- **Coverage**: Monitors all configured relays simultaneously

### 🌐 HTTP Health Endpoint (Optional)

- **Purpose**: External monitoring integration (uptime checkers, Kubernetes probes)
- **Endpoint**: `http://127.0.0.1:8080/health` (port configurable)
- **Format**: JSON response with system status
- **Data**: uptime, events processed, last event timestamp, version

## Configuration

All health monitoring features are configured in the `[health]` section of `config.toml`:

```toml
[health]
# Enable periodic heartbeat notifications (default: true)
heartbeat_enabled = true

# Send heartbeat every N seconds (default: 3600 = 1 hour)
heartbeat_interval = 3600

# Check Nostr relay connections periodically (default: true)
check_relays = true

# Relay connection timeout in seconds (default: 30)
relay_timeout = 30

# Alert if no events received for N seconds (default: 7200 = 2 hours)
# Set to 0 to disable event silence alerts
event_alert_threshold = 7200

# Enable HTTP health status endpoint (default: false)
# Useful for external monitoring systems
enable_http_endpoint = false

# HTTP endpoint port (default: 8080)
# Health endpoint will be available at http://localhost:8080/health
http_port = 8080
```

### Default Behavior

The `[health]` section is **optional**. If not present, all monitoring features are enabled with default values, except for the HTTP endpoint which is disabled by default.

## Alert Examples

### Heartbeat Notification
```text
💓 Health Check

✅ System: Online
⏰ Uptime: 2 hours 30 minutes
📊 Events processed: 15
🔔 Status: Monitoring active
```

### Event Silence Alert
```text
⚠️ Event Silence Alert

🔕 No dispute events received for 2 hours
⏰ System uptime: 4 hours 15 minutes
🔍 Please check:
• Mostro daemon status
• Nostr relay connections
• Network connectivity
```

## HTTP Health Endpoint

When `enable_http_endpoint = true`, the bot exposes a health status endpoint:

### Endpoint Details
- **URL**: `http://127.0.0.1:<port>/health`
- **Method**: GET
- **Content-Type**: application/json

### Response Format
```json
{
  "status": "healthy",
  "uptime_seconds": 7320,
  "events_processed": 42,
  "last_event_timestamp": 1708425600,
  "last_heartbeat_timestamp": 1708425580,
  "version": "0.1.2"
}
```

### Response Fields
- `status`: `"healthy"` or `"unhealthy"`
- `uptime_seconds`: Time since startup in seconds
- `events_processed`: Total number of dispute events processed
- `last_event_timestamp`: Unix timestamp of last received event (or `null`)
- `last_heartbeat_timestamp`: Unix timestamp of last sent heartbeat (or `null`)
- `version`: Application version

### Use Cases
- **Uptime monitoring**: External services like UptimeRobot
- **Kubernetes liveness probes**: Health checks for container orchestration
- **Nagios/Zabbix**: Integration with enterprise monitoring systems
- **Custom dashboards**: Programmatic access to system status

## Benefits

### For Administrators
1. **Proactive monitoring**: Know about issues before they become critical
2. **System confidence**: Regular heartbeats confirm everything is working
3. **Troubleshooting assistance**: Event silence alerts help identify upstream issues
4. **Network resilience**: Automatic relay reconnection maintains connectivity

### For Operations
1. **External integration**: HTTP endpoint enables monitoring system integration
2. **Minimal overhead**: All checks run in background without impacting event processing
3. **Configurable**: All features can be enabled/disabled/adjusted per environment
4. **Anti-spam design**: Intelligent alerting prevents notification flooding

## Technical Implementation

### Background Tasks
- All health monitoring runs in separate `tokio::spawn` tasks
- Uses `tokio::time::interval` with missed tick behavior for reliable scheduling
- Shared state managed through `Arc<RwLock<T>>` for thread safety

### Memory Efficiency
- Minimal state tracking (only essential timestamps and counters)
- No persistent storage requirements
- Counters grow monotonically; timestamps are overwritten on each update

### Error Handling
- Health task failures are logged but don't crash the main application
- Network timeouts are handled gracefully
- Telegram send failures are logged but don't stop health monitoring

## Startup Integration

The enhanced startup message now includes health monitoring status:

```text
🐕 mostro-watchdog is now online and monitoring for disputes.

📊 Health monitoring: enabled
⏰ Heartbeat interval: 3600 seconds
🔔 Event silence alert: 7200 seconds
```

This gives administrators immediate visibility into the monitoring configuration at startup.