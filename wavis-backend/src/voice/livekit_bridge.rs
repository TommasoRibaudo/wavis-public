use async_trait::async_trait;
use livekit_api::services::room::{CreateRoomOptions, RoomClient};

use crate::voice::sfu_bridge::{SfuError, SfuHealth, SfuRoomHandle, SfuRoomManager};

/// LiveKit SFU bridge. Implements `SfuRoomManager` only — LiveKit clients
/// connect directly to the LiveKit server for WebRTC negotiation, so
/// `SfuSignalingProxy` is not needed.
pub struct LiveKitSfuBridge {
    room_client: RoomClient,
}

impl LiveKitSfuBridge {
    /// Construct from explicit credentials.
    /// Returns `SfuError::InvalidInput` if any argument is empty.
    pub fn from_env(api_key: &str, api_secret: &str, host: &str) -> Result<Self, SfuError> {
        if api_key.is_empty() || api_secret.is_empty() || host.is_empty() {
            return Err(SfuError::InvalidInput(
                "api_key, api_secret, and host must all be non-empty".to_string(),
            ));
        }
        // LiveKit clients connect via wss://, but the Twirp API (RoomClient)
        // needs https://. Convert the scheme so callers can use the same
        // LIVEKIT_HOST value for both purposes.
        let api_host = host
            .replace("wss://", "https://")
            .replace("ws://", "http://");
        Ok(Self {
            room_client: RoomClient::with_api_key(&api_host, api_key, api_secret),
        })
    }

    /// Validate that a room_id or participant_id is LiveKit-compatible:
    /// ASCII alphanumeric + hyphens + underscores, 1–128 chars.
    pub fn validate_identifier(value: &str, label: &str) -> Result<(), SfuError> {
        if value.is_empty() || value.len() > 128 {
            return Err(SfuError::InvalidInput(format!(
                "{label} must be 1–128 characters"
            )));
        }
        if !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(SfuError::InvalidInput(format!(
                "{label} must contain only ASCII alphanumeric, hyphens, underscores"
            )));
        }
        Ok(())
    }
}

#[async_trait]
impl SfuRoomManager for LiveKitSfuBridge {
    /// Create a LiveKit room. Idempotent — succeeds if the room already exists.
    async fn create_room(&self, room_id: &str) -> Result<SfuRoomHandle, SfuError> {
        Self::validate_identifier(room_id, "room_id")?;
        self.room_client
            .create_room(room_id, CreateRoomOptions::default())
            .await
            .map(|_| SfuRoomHandle(room_id.to_string()))
            .map_err(|e| SfuError::Unavailable(e.to_string()))
    }

    /// Delete a LiveKit room. Best-effort — succeeds silently if room doesn't exist.
    async fn destroy_room(&self, handle: &SfuRoomHandle) -> Result<(), SfuError> {
        // Ignore errors: room may already be gone
        let _ = self.room_client.delete_room(&handle.0).await;
        Ok(())
    }

    /// Validate participant_id format. LiveKit participants join via token —
    /// there is no explicit "add participant" API call needed.
    async fn add_participant(
        &self,
        _handle: &SfuRoomHandle,
        participant_id: &str,
    ) -> Result<(), SfuError> {
        Self::validate_identifier(participant_id, "participant_id")
    }

    /// Remove a participant from a LiveKit room.
    async fn remove_participant(
        &self,
        handle: &SfuRoomHandle,
        participant_id: &str,
    ) -> Result<(), SfuError> {
        self.room_client
            .remove_participant(&handle.0, participant_id)
            .await
            .map_err(|e| SfuError::ParticipantError(e.to_string()))
    }

    /// Probe LiveKit connectivity by listing rooms.
    async fn health_check(&self) -> Result<SfuHealth, SfuError> {
        match self.room_client.list_rooms(vec![]).await {
            Ok(_) => Ok(SfuHealth::Available),
            Err(e) => Ok(SfuHealth::Unavailable(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // --- Property 2: Identifier validation accepts valid and rejects invalid IDs ---
    // Validates: Requirements 3A.1, 3A.2

    #[test]
    fn empty_identifier_rejected() {
        assert!(LiveKitSfuBridge::validate_identifier("", "id").is_err());
    }

    #[test]
    fn exactly_128_chars_accepted() {
        let value = "a".repeat(128);
        assert!(LiveKitSfuBridge::validate_identifier(&value, "id").is_ok());
    }

    #[test]
    fn exactly_129_chars_rejected() {
        let value = "a".repeat(129);
        assert!(LiveKitSfuBridge::validate_identifier(&value, "id").is_err());
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn prop_valid_identifiers_accepted(
            value in "[a-zA-Z0-9_-]{1,128}",
        ) {
            let result = LiveKitSfuBridge::validate_identifier(&value, "id");
            prop_assert!(result.is_ok(), "valid identifier '{value}' should be accepted");
        }

        #[test]
        fn prop_too_long_identifier_rejected(
            suffix in "[a-zA-Z0-9]{1,50}",
        ) {
            let long_value = "a".repeat(128) + &suffix;
            let result = LiveKitSfuBridge::validate_identifier(&long_value, "id");
            prop_assert!(result.is_err(), "identifier longer than 128 chars should be rejected");
        }

        #[test]
        fn prop_invalid_chars_rejected(
            prefix in "[a-zA-Z0-9_-]{0,10}",
            invalid_char in prop_oneof![
                Just(' '),
                Just('.'),
                Just('/'),
                Just('@'),
                Just('!'),
                Just('\n'),
                Just('\t'),
            ],
            suffix in "[a-zA-Z0-9_-]{0,10}",
        ) {
            let value = format!("{prefix}{invalid_char}{suffix}");
            let result = LiveKitSfuBridge::validate_identifier(&value, "id");
            prop_assert!(result.is_err(), "identifier with invalid char '{invalid_char}' should be rejected");
        }
    }
}
