//! gRPC bidirectional transport candidate for V2-M1.
//!
//! This module intentionally keeps the existing signed V2 application messages
//! intact. gRPC/HTTP2 replaces the custom length-prefixed carrier first; the
//! security and execution semantics stay transport-neutral and can be migrated
//! to native protobuf fields independently later.

use crate::{
    v2_browser_runtime::BrowserBackendResult,
    v2_m0::{DeviceCommand, DeviceResult},
    v2_m0_transport::{AgentToHub, HubToAgent},
};
use std::fmt;

pub mod proto {
    tonic::include_proto!("cumg.v2");
}

use proto::{AgentFrame, HubFrame};

pub const HUB_AGENT_SCHEMA_INCOMPATIBLE_MESSAGE: &str = "Hub-Agent schema incompatible";
pub const MAX_GRPC_APPLICATION_MESSAGE_BYTES: usize = 64 * 1024;
pub const MAX_GRPC_LARGE_RESULT_APPLICATION_MESSAGE_BYTES: usize = 28 * 1024 * 1024;
// Tonic limits the encoded Protobuf message, which includes the bytes-field tag
// and varint length in addition to the signed application payload. Ordinary
// application messages remain capped at 64 KiB. Bounded image/UI observation
// results may use the larger allowance needed for a base64 PNG plus normalized
// window/UI metadata.
pub const MAX_GRPC_TRANSPORT_MESSAGE_BYTES: usize =
    MAX_GRPC_LARGE_RESULT_APPLICATION_MESSAGE_BYTES + 1024;

pub fn encode_agent_frame(message: &AgentToHub) -> Result<AgentFrame, GrpcCarrierError> {
    let bytes = serde_json::to_vec(message).map_err(GrpcCarrierError::Serialization)?;
    enforce_bound(bytes.len(), agent_message_limit(message))?;
    Ok(AgentFrame {
        signed_message_json: bytes,
    })
}

pub fn decode_agent_frame(frame: AgentFrame) -> Result<AgentToHub, GrpcCarrierError> {
    enforce_bound(
        frame.signed_message_json.len(),
        MAX_GRPC_LARGE_RESULT_APPLICATION_MESSAGE_BYTES,
    )?;
    let message: AgentToHub = serde_json::from_slice(&frame.signed_message_json)
        .map_err(GrpcCarrierError::Serialization)?;
    enforce_bound(
        frame.signed_message_json.len(),
        agent_message_limit(&message),
    )?;
    Ok(message)
}

pub fn encode_hub_frame(message: &HubToAgent) -> Result<HubFrame, GrpcCarrierError> {
    let bytes = serde_json::to_vec(message).map_err(GrpcCarrierError::Serialization)?;
    enforce_bound(bytes.len(), hub_message_limit(message))?;
    Ok(HubFrame {
        signed_message_json: bytes,
    })
}

pub fn decode_hub_frame(frame: HubFrame) -> Result<HubToAgent, GrpcCarrierError> {
    enforce_bound(
        frame.signed_message_json.len(),
        MAX_GRPC_LARGE_RESULT_APPLICATION_MESSAGE_BYTES,
    )?;
    let message: HubToAgent = serde_json::from_slice(&frame.signed_message_json)
        .map_err(GrpcCarrierError::Serialization)?;
    enforce_bound(frame.signed_message_json.len(), hub_message_limit(&message))?;
    Ok(message)
}

fn hub_message_limit(message: &HubToAgent) -> usize {
    match message {
        HubToAgent::Command(remote)
            if matches!(
                remote.command.command,
                DeviceCommand::StageBrowserUploadFile { .. }
            ) =>
        {
            MAX_GRPC_LARGE_RESULT_APPLICATION_MESSAGE_BYTES
        }
        _ => MAX_GRPC_APPLICATION_MESSAGE_BYTES,
    }
}

fn agent_message_limit(message: &AgentToHub) -> usize {
    match message {
        AgentToHub::Result(remote)
            if matches!(
                remote.result.result,
                DeviceResult::Screenshot { .. }
                    | DeviceResult::Windows { .. }
                    | DeviceResult::ApplicationLaunched { .. }
                    | DeviceResult::WindowSnapshot { .. }
                    | DeviceResult::UiStateVerification { .. }
                    | DeviceResult::ClipboardState { .. }
                    | DeviceResult::RegionCaptured { .. }
                    | DeviceResult::Browser {
                        result: BrowserBackendResult::Bound { .. }
                            | BrowserBackendResult::Snapshot { .. }
                            | BrowserBackendResult::DownloadCompleted { .. },
                    }
            ) =>
        {
            MAX_GRPC_LARGE_RESULT_APPLICATION_MESSAGE_BYTES
        }
        _ => MAX_GRPC_APPLICATION_MESSAGE_BYTES,
    }
}

fn enforce_bound(size: usize, limit: usize) -> Result<(), GrpcCarrierError> {
    if size > limit {
        Err(GrpcCarrierError::MessageTooLarge { size, limit })
    } else {
        Ok(())
    }
}

#[derive(Debug)]
pub enum GrpcCarrierError {
    Serialization(serde_json::Error),
    MessageTooLarge { size: usize, limit: usize },
}

impl fmt::Display for GrpcCarrierError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Serialization(error) => write!(f, "gRPC carrier serialization error: {error}"),
            Self::MessageTooLarge { size, limit } => write!(
                f,
                "gRPC application message {size} bytes exceeds permitted bound {limit}"
            ),
        }
    }
}

impl std::error::Error for GrpcCarrierError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2_m0::{CAPABILITY_SCHEMA_VERSION, CapabilityAdvertisement};
    use crate::v2_m0_transport::{
        AgentHello, HUB_AGENT_SCHEMA_VERSION, RemoteIndeterminateAck, RemoteIndeterminateCause,
    };
    use base64::Engine as _;
    use prost::Message;

    #[test]
    fn protobuf_carrier_round_trips_existing_signed_protocol_message() {
        let hello = AgentToHub::Hello(AgentHello {
            schema_version: HUB_AGENT_SCHEMA_VERSION,
            device_id: "dev-test".into(),
            agent_nonce: [7; 32],
            capabilities: CapabilityAdvertisement {
                backend: "agent-native".into(),
                backend_version: "1".into(),
                platform: "test".into(),
                capability_schema_version: CAPABILITY_SCHEMA_VERSION,
                revision: 1,
                supported: vec![],
            },
        });
        let decoded = decode_agent_frame(encode_agent_frame(&hello).unwrap()).unwrap();
        assert_eq!(decoded, hello);
    }

    #[test]
    fn indeterminate_timeout_ack_round_trips_the_bounded_carrier() {
        let message = AgentToHub::IndeterminateAck(RemoteIndeterminateAck {
            schema_version: HUB_AGENT_SCHEMA_VERSION,
            device_id: "dev-test".into(),
            device_generation: 7,
            operation_id: "op_timeout_0123456789abcdef".into(),
            cause: RemoteIndeterminateCause::BackendTimedOut,
            signature: vec![0; 64],
        });
        let frame = encode_agent_frame(&message).unwrap();
        assert!(frame.signed_message_json.len() < 1024);
        assert_eq!(decode_agent_frame(frame).unwrap(), message);
    }

    #[test]
    fn bounded_filesystem_result_fits_signed_json_carrier() {
        use crate::v2_m0::{CONTROL_SCHEMA_VERSION, CommandResultEnvelope, DeviceResult};
        use crate::v2_m0_transport::{AgentToHub, HUB_AGENT_SCHEMA_VERSION, RemoteResult};
        let result = AgentToHub::Result(RemoteResult {
            schema_version: HUB_AGENT_SCHEMA_VERSION,
            result: CommandResultEnvelope {
                schema_version: CONTROL_SCHEMA_VERSION,
                device_id: "dev-test".into(),
                device_generation: 1,
                capability_revision: 1,
                operation_id: "op-filesystem".into(),
                result: DeviceResult::FileContents {
                    bytes: vec![255; crate::v2_m1_filesystem::DEFAULT_MAX_FILE_BYTES],
                    truncated: true,
                    offset: 0,
                    next_offset: Some(crate::v2_m1_filesystem::DEFAULT_MAX_FILE_BYTES as u64),
                },
            },
            signature: vec![0; 64],
        });
        encode_agent_frame(&result).unwrap();
    }

    #[test]
    fn protobuf_carrier_transport_limit_includes_envelope_overhead() {
        let frame = AgentFrame {
            signed_message_json: vec![0; MAX_GRPC_APPLICATION_MESSAGE_BYTES],
        };
        assert!(frame.encoded_len() > MAX_GRPC_APPLICATION_MESSAGE_BYTES);
        assert!(frame.encoded_len() <= MAX_GRPC_TRANSPORT_MESSAGE_BYTES);
    }

    #[test]
    fn protobuf_carrier_keeps_ordinary_application_message_bound() {
        let size = MAX_GRPC_APPLICATION_MESSAGE_BYTES + 1;
        assert!(matches!(
            enforce_bound(size, MAX_GRPC_APPLICATION_MESSAGE_BYTES),
            Err(GrpcCarrierError::MessageTooLarge { size: got, limit })
                if got == size && limit == MAX_GRPC_APPLICATION_MESSAGE_BYTES
        ));
    }

    #[test]
    fn screenshot_carrier_allowance_covers_the_bounded_base64_payload() {
        let max_base64 = crate::v2_m0::MAX_SCREENSHOT_BYTES.div_ceil(3) * 4;
        assert!(max_base64 + 64 * 1024 < MAX_GRPC_LARGE_RESULT_APPLICATION_MESSAGE_BYTES);
    }

    #[test]
    fn large_result_allowance_covers_max_png_plus_bounded_ui_snapshot_metadata() {
        let max_base64 = crate::v2_m0::MAX_SCREENSHOT_BYTES.div_ceil(3) * 4;
        let per_element_budget =
            (crate::v2_m0::MAX_UI_TEXT_BYTES * 2) + (crate::v2_m0::MAX_UI_REF_BYTES * 2) + 512;
        let bounded_snapshot_budget =
            max_base64 + (crate::v2_m0::MAX_UI_ELEMENTS * per_element_budget) + 128 * 1024;
        assert!(bounded_snapshot_budget < MAX_GRPC_LARGE_RESULT_APPLICATION_MESSAGE_BYTES);
    }

    #[test]
    fn browser_observation_results_use_large_carrier_but_mutations_do_not() {
        use crate::v2_browser_runtime::{BrowserBackendResult, BrowserMutationEffect};
        use crate::v2_m0::{CONTROL_SCHEMA_VERSION, CommandResultEnvelope, DeviceResult};
        use crate::v2_m0_transport::{AgentToHub, HUB_AGENT_SCHEMA_VERSION, RemoteResult};

        let snapshot = AgentToHub::Result(RemoteResult {
            schema_version: HUB_AGENT_SCHEMA_VERSION,
            result: CommandResultEnvelope {
                schema_version: CONTROL_SCHEMA_VERSION,
                device_id: "dev-test".into(),
                device_generation: 1,
                capability_revision: 1,
                operation_id: "op-browser-snapshot".into(),
                result: DeviceResult::Browser {
                    result: BrowserBackendResult::Snapshot {
                        backend_snapshot_id: "snapshot".into(),
                        outline: "x".repeat(96 * 1024),
                        action_refs: vec![],
                        content_refs: vec![],
                        complete: true,
                        omitted: 0,
                        backend_continuation: None,
                        screenshot: None,
                    },
                },
            },
            signature: vec![0; 64],
        });
        let frame = encode_agent_frame(&snapshot).expect("browser snapshot uses large allowance");
        assert!(frame.signed_message_json.len() > MAX_GRPC_APPLICATION_MESSAGE_BYTES);
        assert_eq!(decode_agent_frame(frame).unwrap(), snapshot);

        let click = AgentToHub::Result(RemoteResult {
            schema_version: HUB_AGENT_SCHEMA_VERSION,
            result: CommandResultEnvelope {
                schema_version: CONTROL_SCHEMA_VERSION,
                device_id: "dev-test".into(),
                device_generation: 1,
                capability_revision: 1,
                operation_id: "op-browser-click".into(),
                result: DeviceResult::Browser {
                    result: BrowserBackendResult::ClickCompleted {
                        effect: BrowserMutationEffect::Unverifiable,
                    },
                },
            },
            signature: vec![0; 64],
        });
        assert_eq!(
            agent_message_limit(&click),
            MAX_GRPC_APPLICATION_MESSAGE_BYTES
        );
    }

    #[test]
    fn browser_snapshot_budget_fits_reviewed_large_result_allowance() {
        let max_base64 = crate::v2_browser_execute::MAX_BROWSER_SCREENSHOT_BYTES.div_ceil(3) * 4;
        let metadata = crate::v2_browser_normalize::MAX_BROWSER_STRUCTURED_METADATA_BYTES;
        let envelope_headroom = 512 * 1024;
        assert!(
            max_base64 + metadata + envelope_headroom
                < MAX_GRPC_LARGE_RESULT_APPLICATION_MESSAGE_BYTES
        );
    }

    #[test]
    fn bounded_clipboard_text_uses_the_large_result_allowance() {
        use crate::v2_m0::{CONTROL_SCHEMA_VERSION, CommandResultEnvelope, DeviceResult};
        use crate::v2_m0_transport::{AgentToHub, HUB_AGENT_SCHEMA_VERSION, RemoteResult};

        let result = AgentToHub::Result(RemoteResult {
            schema_version: HUB_AGENT_SCHEMA_VERSION,
            result: CommandResultEnvelope {
                schema_version: CONTROL_SCHEMA_VERSION,
                device_id: "dev-test".into(),
                device_generation: 1,
                capability_revision: 1,
                operation_id: "op-clipboard".into(),
                result: DeviceResult::ClipboardState {
                    types: vec!["public.utf8-plain-text".into()],
                    text: Some("x".repeat(crate::v2_m0::MAX_CLIPBOARD_TEXT_BYTES)),
                },
            },
            signature: vec![0; 64],
        });
        let frame = encode_agent_frame(&result)
            .expect("bounded clipboard text fits large result allowance");
        assert!(frame.signed_message_json.len() > MAX_GRPC_APPLICATION_MESSAGE_BYTES);
        assert!(frame.signed_message_json.len() <= MAX_GRPC_LARGE_RESULT_APPLICATION_MESSAGE_BYTES);
        assert_eq!(decode_agent_frame(frame).unwrap(), result);
    }

    #[test]
    fn bounded_region_capture_uses_the_large_result_allowance() {
        use crate::v2_m0::{CONTROL_SCHEMA_VERSION, CommandResultEnvelope, DeviceResult, UiImage};
        use crate::v2_m0_transport::{AgentToHub, HUB_AGENT_SCHEMA_VERSION, RemoteResult};

        let image_bytes = vec![7_u8; 96 * 1024];
        let result = AgentToHub::Result(RemoteResult {
            schema_version: HUB_AGENT_SCHEMA_VERSION,
            result: CommandResultEnvelope {
                schema_version: CONTROL_SCHEMA_VERSION,
                device_id: "dev-test".into(),
                device_generation: 1,
                capability_revision: 1,
                operation_id: "op-region".into(),
                result: DeviceResult::RegionCaptured {
                    image: UiImage {
                        data_base64: base64::engine::general_purpose::STANDARD.encode(image_bytes),
                        mime_type: "image/jpeg".into(),
                        width_pixels: 500,
                        height_pixels: 500,
                    },
                },
            },
            signature: vec![0; 64],
        });
        let frame =
            encode_agent_frame(&result).expect("bounded region capture fits large allowance");
        assert!(frame.signed_message_json.len() > MAX_GRPC_APPLICATION_MESSAGE_BYTES);
        assert_eq!(decode_agent_frame(frame).unwrap(), result);
    }

    #[test]
    fn typed_screenshot_result_has_a_separate_bounded_carrier_allowance() {
        use crate::v2_m0::{CONTROL_SCHEMA_VERSION, CommandResultEnvelope, DeviceResult};
        use crate::v2_m0_transport::{AgentToHub, HUB_AGENT_SCHEMA_VERSION, RemoteResult};

        let image_bytes = vec![7_u8; 96 * 1024];
        let result = AgentToHub::Result(RemoteResult {
            schema_version: HUB_AGENT_SCHEMA_VERSION,
            result: CommandResultEnvelope {
                schema_version: CONTROL_SCHEMA_VERSION,
                device_id: "dev-test".into(),
                device_generation: 1,
                capability_revision: 1,
                operation_id: "op-screenshot".into(),
                result: DeviceResult::Screenshot {
                    data_base64: base64::engine::general_purpose::STANDARD.encode(image_bytes),
                    mime_type: "image/png".into(),
                    width_pixels: 100,
                    height_pixels: 100,
                },
            },
            signature: vec![0; 64],
        });
        let frame =
            encode_agent_frame(&result).expect("typed screenshot fits screenshot allowance");
        assert!(frame.signed_message_json.len() > MAX_GRPC_APPLICATION_MESSAGE_BYTES);
        assert!(frame.signed_message_json.len() <= MAX_GRPC_LARGE_RESULT_APPLICATION_MESSAGE_BYTES);
        assert_eq!(decode_agent_frame(frame).unwrap(), result);
    }

    #[test]
    fn bounded_browser_download_fits_reviewed_large_result_allowance() {
        let max_base64 = crate::v2_browser::MAX_BROWSER_DOWNLOAD_BASE64_BYTES;
        let envelope_headroom = 512 * 1024;
        assert!(max_base64 + envelope_headroom < MAX_GRPC_LARGE_RESULT_APPLICATION_MESSAGE_BYTES);
    }

    #[test]
    fn max_workspace_mutation_command_fits_ordinary_signed_carrier() {
        use crate::v2_m0::{
            CONTROL_SCHEMA_VERSION, CapabilityClass, CommandEnvelope, DeviceCapability,
            GrantPayload, GrantToken, WorkspaceWritePath, WorkspaceWritePayload,
            WorkspaceWritePrecondition,
        };
        use crate::v2_m0_transport::{HUB_AGENT_SCHEMA_VERSION, RemoteCommand};
        use crate::v2_m1_workspace_mutation::{
            DEFAULT_MAX_WORKSPACE_PATH_BYTES, DEFAULT_MAX_WORKSPACE_WRITE_BYTES, sha256_hex,
        };
        use base64::{Engine as _, engine::general_purpose::STANDARD};

        let raw = vec![7_u8; DEFAULT_MAX_WORKSPACE_WRITE_BYTES];
        let payload = WorkspaceWritePayload::after_contract_validation(STANDARD.encode(&raw));
        let message = HubToAgent::Command(RemoteCommand {
            schema_version: HUB_AGENT_SCHEMA_VERSION,
            command: CommandEnvelope {
                schema_version: CONTROL_SCHEMA_VERSION,
                device_id: "dev-a".into(),
                device_generation: u64::MAX,
                capability_revision: u64::MAX,
                operation_id: "op_0123456789abcdef0123456789abcdef".into(),
                command: DeviceCommand::WriteWorkspaceFile {
                    path: WorkspaceWritePath::after_contract_validation(
                        "p".repeat(DEFAULT_MAX_WORKSPACE_PATH_BYTES),
                    ),
                    data_base64: payload,
                    expected_bytes: raw.len() as u64,
                    content_sha256: sha256_hex(&raw),
                    precondition: WorkspaceWritePrecondition::ExpectedSha256 {
                        sha256: "f".repeat(64),
                    },
                },
            },
            grant: GrantToken {
                payload: GrantPayload {
                    schema_version: CONTROL_SCHEMA_VERSION,
                    issuer_key_id: "i".repeat(128),
                    grant_id: "grant_0123456789abcdef0123456789abcdef".into(),
                    device_id: "dev-a".into(),
                    capability: CapabilityClass::Dangerous,
                    device_capability: Some(DeviceCapability::WriteWorkspaceFile),
                    issued_at_ms: u64::MAX - 1,
                    expires_at_ms: u64::MAX,
                },
                signature: vec![255; 64],
            },
            handoff: None,
            signature: vec![255; 64],
        });
        let frame = encode_hub_frame(&message)
            .expect("bounded workspace mutation must fit the ordinary carrier");
        assert!(frame.signed_message_json.len() < MAX_GRPC_APPLICATION_MESSAGE_BYTES);
    }

    #[test]
    fn staged_browser_upload_gets_only_the_reviewed_bounded_large_carrier() {
        use crate::v2_m0::{
            BrowserUploadPayload, CONTROL_SCHEMA_VERSION, CapabilityClass, CommandEnvelope,
            DeviceCapability, GrantPayload, GrantToken,
        };
        use crate::v2_m0_transport::RemoteCommand;
        let payload = BrowserUploadPayload::after_contract_validation("A".repeat(1024 * 1024));
        let message = HubToAgent::Command(RemoteCommand {
            schema_version: HUB_AGENT_SCHEMA_VERSION,
            command: CommandEnvelope {
                schema_version: CONTROL_SCHEMA_VERSION,
                device_id: "dev-a".into(),
                device_generation: 1,
                capability_revision: 1,
                operation_id: "op-test".into(),
                command: DeviceCommand::StageBrowserUploadFile {
                    context_id: "ctx_0123456789abcdef0123456789abcdef".into(),
                    file_name: "payload.bin".into(),
                    data_base64: payload,
                    expected_bytes: 3,
                },
            },
            grant: GrantToken {
                payload: GrantPayload {
                    schema_version: CONTROL_SCHEMA_VERSION,
                    issuer_key_id: "issuer".into(),
                    grant_id: "grant-test".into(),
                    device_id: "dev-a".into(),
                    capability: CapabilityClass::Dangerous,
                    device_capability: Some(DeviceCapability::BrowserUploadFile),
                    issued_at_ms: 1,
                    expires_at_ms: 2,
                },
                signature: vec![0; 64],
            },
            handoff: None,
            signature: vec![0; 64],
        });
        let frame = encode_hub_frame(&message).expect("upload staging uses bounded large carrier");
        assert!(frame.signed_message_json.len() > MAX_GRPC_APPLICATION_MESSAGE_BYTES);
        assert!(frame.signed_message_json.len() < MAX_GRPC_LARGE_RESULT_APPLICATION_MESSAGE_BYTES);
    }
}
