# Test Cases for Dispute Status Alerts

## Test Scenarios

### 1. New Dispute Alert (initiated)
- **Input**: Event with status "initiated", dispute_id "test123", initiator "buyer"
- **Expected**: Alert sent with 🚨 NEW DISPUTE message
- **Verification**: Message contains dispute ID and "Please take this dispute"

### 2. Dispute Taken Alert (in-progress)
- **Input**: Event with status "in-progress", dispute_id "test123"
- **Expected**: Alert sent with 🔄 DISPUTE IN PROGRESS message
- **Verification**: Message contains "Taken by solver"

### 3. Resolved Alert (settled)
- **Input**: Event with status "settled", dispute_id "test123"
- **Expected**: Alert sent with ✅ DISPUTE RESOLVED message
- **Verification**: Message contains "Payment to buyer"

### 4. Resolved Alert (seller-refunded)
- **Input**: Event with status "seller-refunded", dispute_id "test123"
- **Expected**: Alert sent with 💰 DISPUTE RESOLVED message
- **Verification**: Message contains "Seller refunded"

### 5. Released Alert (released)
- **Input**: Event with status "released", dispute_id "test123"
- **Expected**: Alert sent with 🔓 DISPUTE RESOLVED message
- **Verification**: Message contains "Released by seller"

### 6. Unknown Status Alert
- **Input**: Event with status "unknown-status", dispute_id "test123"
- **Expected**: Alert sent with 📡 DISPUTE STATUS UPDATE message
- **Verification**: Message contains the unknown status

### 7. Alert Configuration - Disabled Status
- **Input**: Event with status "in-progress", but in_progress = false in config
- **Expected**: No alert sent
- **Verification**: Log shows "Alert for status 'in-progress' is disabled"

### 8. Alert Configuration - Default Enabled
- **Input**: Event with any status, no [alerts] section in config
- **Expected**: Alert sent (defaults to enabled)
- **Verification**: Alert is processed normally

### 9. Malformed Event
- **Input**: Event without status tag
- **Expected**: Alert sent with status "unknown"
- **Verification**: Handled gracefully, no crash

### 10. Markdown Escaping
- **Input**: Dispute ID with special characters: "test_123-abc*def"
- **Expected**: Alert sent with properly escaped markdown
- **Verification**: Message displays correctly without formatting issues

## Integration Tests

### Test Bot Setup
```bash
# Use test bot token and chat ID
export TEST_BOT_TOKEN="your_test_bot_token"
export TEST_CHAT_ID="your_test_chat_id"
```

### Sample Test Config
```toml
[mostro]
pubkey = "test_pubkey"

[nostr]
relays = ["wss://test.relay"]

[telegram]
bot_token = "test_token"
chat_id = 123456789

[alerts]
initiated = true
in_progress = false
settled = true
other = false
```

### Manual Testing Commands

1. **Start watchdog with test config**
```bash
RUST_LOG=debug ./target/release/mostro-watchdog --config test-config.toml
```

2. **Send test Nostr events** (requires test relay setup)

3. **Verify alerts in test Telegram chat**

## Expected Behavior

- All enabled alerts should be sent immediately
- Disabled alerts should be logged but not sent
- Message formatting should be consistent
- Timestamps should display correctly
- Special characters should be escaped properly
- No crashes on malformed input