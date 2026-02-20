# Dispute Status Alerts

This document describes the enhanced dispute monitoring capabilities implemented in issue #2.

## Overview

mostro-watchdog now monitors **all** dispute status changes, not just new disputes. This provides complete visibility into the dispute lifecycle for Mostro administrators.

## Dispute Status Types

The bot monitors these dispute status changes:

### 🚨 `initiated`
- **Description**: New dispute created, waiting for solver
- **Message**: Shows dispute ID, initiator (buyer/seller), and timestamp
- **Action needed**: Admin should take the dispute

### 🔄 `in-progress`  
- **Description**: Dispute taken by a solver/admin
- **Message**: Confirms dispute is being handled
- **Action needed**: None, informational

### 💰 `seller-refunded`
- **Description**: Dispute resolved by refunding the seller
- **Message**: Shows resolution and closure confirmation
- **Action needed**: None, dispute closed

### ✅ `settled`
- **Description**: Dispute resolved by paying the buyer
- **Message**: Shows payment resolution and closure
- **Action needed**: None, dispute closed

### 🔓 `released`
- **Description**: Dispute resolved when seller releases funds
- **Message**: Shows cooperative resolution
- **Action needed**: None, dispute closed

### 📡 `other`
- **Description**: Unknown or future status types
- **Message**: Generic status update message
- **Action needed**: May require investigation

## Configuration

### Alert Types (Optional)

You can enable/disable specific alert types in your `config.toml`:

```toml
```toml
[alerts]
initiated = true        # New disputes (recommended: true)
in_progress = true      # Dispute taken (recommended: true)
seller_refunded = true  # Seller refunded (recommended: true) 
settled = true          # Payment to buyer (recommended: true)
released = true         # Released by seller (recommended: true)
other = true           # Unknown statuses (recommended: true)
```

### Backward Compatibility

The `[alerts]` section is **optional**. If not present, all alert types default to enabled, maintaining backward compatibility.

## Alert Format Examples

### New Dispute (initiated)
```text
🚨 NEW DISPUTE

📋 Dispute ID: `abc123def456`
👤 Initiated by: buyer
⏰ Time: 2026-02-20 15:30:00 UTC

⚡ Please take this dispute in Mostrix or your admin client.
```

### Dispute In Progress
```text
🔄 DISPUTE IN PROGRESS

📋 Dispute ID: `abc123def456`
👨‍⚖️ Status: Taken by solver
⏰ Time: 2026-02-20 15:35:00 UTC

ℹ️ Dispute is now being handled.
```

### Dispute Resolved (settled)
```text
✅ DISPUTE RESOLVED

📋 Dispute ID: `abc123def456`
💸 Resolution: Payment to buyer
⏰ Time: 2026-02-20 16:00:00 UTC

✔️ Dispute closed: buyer receives payment.
```

## Benefits

1. **Complete visibility**: Track disputes from creation to resolution
2. **Reduced response time**: Immediate notifications for all status changes
3. **Better coordination**: Team knows when disputes are being handled
4. **Audit trail**: Full history of dispute progression
5. **Customizable**: Enable only the alerts you need

## Migration from v0.1.x

Existing configurations continue to work unchanged. The new status monitoring is automatic, and you can optionally add the `[alerts]` section for fine-grained control.

## Technical Details

- Monitors Nostr events (kind 38386) for all status values
- Parses `s` tag for status, `d` tag for dispute ID, `initiator` tag for who created dispute
- Uses different emoji and messaging for each status type
- Maintains backward compatibility with existing configurations