#[cfg(test)]
mod property_tests {
    use crate::audio::MockAudioBackend;
    use crate::ice_config::IceConfig;
    use crate::webrtc::*;
    use proptest::prelude::*;
    use shared::signaling::IceCandidate;
    use std::sync::{Arc, Mutex};

    fn test_ice_config() -> IceConfig {
        IceConfig {
            stun_urls: vec!["stun:stun.example.com:19302".to_string()],
            turn_urls: vec!["turn:turn.example.com:3478".to_string()],
            turn_username: "user".to_string(),
            turn_credential: "pass".to_string(),
        }
    }

    // Feature: p2p-voice, Property 6: All gathered ICE candidates are sent via signaling
    // **Validates: Requirements 3.6**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]
        #[test]
        fn all_gathered_ice_candidates_are_sent_via_signaling(
            candidates in prop::collection::vec(any::<IceCandidate>(), 1..20)
        ) {
            let audio = MockAudioBackend::new();
            let pc = MockPeerConnectionBackend::new();
            let manager = CallManager::new(audio, pc, test_ice_config());

            // Collect candidates forwarded by CallManager
            let forwarded: Arc<Mutex<Vec<IceCandidate>>> = Arc::new(Mutex::new(Vec::new()));
            let forwarded_clone = Arc::clone(&forwarded);
            manager.on_ice_candidate(move |c| {
                forwarded_clone.lock().unwrap().push(c);
            });

            // Start a call so handlers are installed
            let _ = manager.start_call().unwrap();

            // Simulate each ICE candidate being gathered
            for candidate in &candidates {
                manager.pc_backend.simulate_ice_candidate(candidate.clone());
            }

            let sent = forwarded.lock().unwrap();
            // Each candidate must be forwarded exactly once
            prop_assert_eq!(sent.len(), candidates.len());
            for (sent_c, orig_c) in sent.iter().zip(candidates.iter()) {
                prop_assert_eq!(sent_c, orig_c);
            }
        }
    }

    // Feature: p2p-voice, Property 7: All received ICE candidates are added to PeerConnection
    // **Validates: Requirements 3.7**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]
        #[test]
        fn all_received_ice_candidates_are_added_to_peer_connection(
            candidates in prop::collection::vec(any::<IceCandidate>(), 1..20)
        ) {
            let audio = MockAudioBackend::new();
            let pc = MockPeerConnectionBackend::new();
            let manager = CallManager::new(audio, pc, test_ice_config());

            // Start a call so we have an active PeerConnection
            let _ = manager.start_call().unwrap();

            // Feed each ICE candidate via add_ice_candidate (simulating signaling delivery)
            for candidate in &candidates {
                manager.add_ice_candidate(candidate).unwrap();
            }

            // Verify each candidate was added to the PeerConnection
            let calls = manager.pc_backend.calls();
            let added: Vec<&IceCandidate> = calls.iter().filter_map(|c| {
                if let MockPcCall::AddIceCandidate(ref ic) = c {
                    Some(ic)
                } else {
                    None
                }
            }).collect();

            prop_assert_eq!(added.len(), candidates.len());
            for (added_c, orig_c) in added.iter().zip(candidates.iter()) {
                prop_assert_eq!(*added_c, orig_c);
            }
        }
    }

    // Strategy: pick a random active call state to simulate before hangup
    fn active_call_state_strategy() -> impl Strategy<Value = ConnectionState> {
        prop_oneof![
            Just(ConnectionState::Checking),  // → Connecting
            Just(ConnectionState::Connected), // → Connected
            Just(ConnectionState::Failed),    // → Failed
        ]
    }

    // Feature: p2p-voice, Property 8: Call cleanup releases all resources
    // **Validates: Requirements 3.10, 5.4, 6.1**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]
        #[test]
        fn call_cleanup_releases_all_resources(
            conn_state in active_call_state_strategy()
        ) {
            let audio = MockAudioBackend::new();
            let pc = MockPeerConnectionBackend::new();
            let manager = CallManager::new(audio, pc, test_ice_config());

            // Start a call to get into Negotiating state
            let _ = manager.start_call().unwrap();

            // Simulate connection state to move into the target state
            // (Failed state triggers its own cleanup, so we handle that case)
            if conn_state != ConnectionState::Failed {
                manager.pc_backend.simulate_connection_state(conn_state);
            }

            // For non-Failed states, call hangup explicitly
            if conn_state == ConnectionState::Failed {
                // Failed triggers automatic cleanup via the state handler
                manager.pc_backend.simulate_connection_state(ConnectionState::Failed);
            } else {
                manager.hangup().unwrap();
            }

            // Verify: PeerConnection was closed
            let calls = manager.pc_backend.calls();
            let close_count = calls.iter().filter(|c| matches!(c, MockPcCall::Close)).count();
            prop_assert!(close_count >= 1, "PeerConnection must be closed, got {} Close calls", close_count);

            // Verify: audio.stop() was called
            let audio_calls = manager.audio.calls();
            let stop_count = audio_calls.iter().filter(|c| matches!(c, crate::audio::MockCall::Stop)).count();
            prop_assert!(stop_count >= 1, "audio.stop() must be called, got {} Stop calls", stop_count);

            // Verify: final state is Closed or Failed
            let final_state = manager.state();
            prop_assert!(
                final_state == CallState::Closed || final_state == CallState::Failed,
                "Final state must be Closed or Failed, got {:?}", final_state
            );
        }
    }
}

#[cfg(test)]
mod unit_tests {
    use crate::audio::{MockAudioBackend, MockCall};
    use crate::ice_config::IceConfig;
    use crate::webrtc::*;

    fn test_ice_config() -> IceConfig {
        IceConfig {
            stun_urls: vec!["stun:stun.example.com:19302".to_string()],
            turn_urls: vec!["turn:turn.example.com:3478".to_string()],
            turn_username: "user".to_string(),
            turn_credential: "pass".to_string(),
        }
    }

    // Requirements: 3.1, 3.2, 3.3, 3.5, 3.8, 3.10, 5.1
    #[test]
    fn happy_path_start_call_answer_connect_hangup() {
        let audio = MockAudioBackend::new();
        let pc = MockPeerConnectionBackend::new();
        let manager = CallManager::new(audio, pc, test_ice_config());

        // start_call: creates PC, captures mic, adds track, creates offer
        let offer = manager.start_call().unwrap();
        assert_eq!(offer, "mock-offer-sdp");
        assert_eq!(manager.state(), CallState::Negotiating);

        // set_answer: sets remote description
        manager.set_answer("remote-answer").unwrap();
        assert_eq!(manager.state(), CallState::Connecting);

        // ICE connected → audio playing
        manager
            .pc_backend
            .simulate_connection_state(ConnectionState::Connected);
        assert_eq!(manager.state(), CallState::Connected);

        // Verify play_remote was called
        let audio_calls = manager.audio.calls();
        assert!(audio_calls
            .iter()
            .any(|c| matches!(c, MockCall::PlayRemote(_))));

        // hangup → cleanup
        manager.hangup().unwrap();
        assert_eq!(manager.state(), CallState::Closed);

        let pc_calls = manager.pc_backend.calls();
        assert!(pc_calls.iter().any(|c| matches!(c, MockPcCall::Close)));
        let audio_calls = manager.audio.calls();
        assert!(audio_calls.iter().any(|c| matches!(c, MockCall::Stop)));
    }

    // Requirements: 3.4, 3.2, 3.8, 5.1
    #[test]
    fn accept_call_path() {
        let audio = MockAudioBackend::new();
        let pc = MockPeerConnectionBackend::new();
        let manager = CallManager::new(audio, pc, test_ice_config());

        let answer = manager.accept_call("remote-offer-sdp").unwrap();
        assert_eq!(answer, "mock-answer-sdp");
        assert_eq!(manager.state(), CallState::Negotiating);

        // ICE connected
        manager
            .pc_backend
            .simulate_connection_state(ConnectionState::Connected);
        assert_eq!(manager.state(), CallState::Connected);

        let audio_calls = manager.audio.calls();
        assert!(audio_calls
            .iter()
            .any(|c| matches!(c, MockCall::PlayRemote(_))));
    }

    // Requirements: 5.2
    #[test]
    fn mic_denied_aborts_call() {
        let audio = MockAudioBackend::new();
        audio.set_capture_result(Err("denied".to_string()));
        let pc = MockPeerConnectionBackend::new();
        let manager = CallManager::new(audio, pc, test_ice_config());

        let result = manager.start_call();
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), CallError::MicrophoneDenied));

        // PC was created but no offer should have been made
        let pc_calls = manager.pc_backend.calls();
        assert!(!pc_calls
            .iter()
            .any(|c| matches!(c, MockPcCall::CreateOffer)));
    }

    // Requirements: 3.9
    #[test]
    fn ice_failed_triggers_cleanup() {
        let audio = MockAudioBackend::new();
        let pc = MockPeerConnectionBackend::new();
        let manager = CallManager::new(audio, pc, test_ice_config());

        let _ = manager.start_call().unwrap();
        manager
            .pc_backend
            .simulate_connection_state(ConnectionState::Failed);

        assert_eq!(manager.state(), CallState::Failed);

        let pc_calls = manager.pc_backend.calls();
        assert!(pc_calls.iter().any(|c| matches!(c, MockPcCall::Close)));
        let audio_calls = manager.audio.calls();
        assert!(audio_calls.iter().any(|c| matches!(c, MockCall::Stop)));
    }

    // Requirements: 6.1
    #[test]
    fn peer_left_triggers_hangup() {
        let audio = MockAudioBackend::new();
        let pc = MockPeerConnectionBackend::new();
        let manager = CallManager::new(audio, pc, test_ice_config());

        let _ = manager.start_call().unwrap();
        manager
            .pc_backend
            .simulate_connection_state(ConnectionState::Connected);
        assert_eq!(manager.state(), CallState::Connected);

        // Simulate peer_left by calling hangup (the integration layer
        // routes peer_left → hangup)
        manager.hangup().unwrap();
        assert_eq!(manager.state(), CallState::Closed);

        let pc_calls = manager.pc_backend.calls();
        assert!(pc_calls.iter().any(|c| matches!(c, MockPcCall::Close)));
        let audio_calls = manager.audio.calls();
        assert!(audio_calls.iter().any(|c| matches!(c, MockCall::Stop)));
    }
}
