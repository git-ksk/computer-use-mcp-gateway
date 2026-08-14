//! gRPC bidirectional transport candidate for V2-M1.
//!
//! This module intentionally keeps the existing signed V2 application messages
//! intact. gRPC/HTTP2 replaces the custom length-prefixed carrier first; the
//! security and execution semantics stay transport-neutral and can be migrated
//! to native protobuf fields independently later.

use crate::{
    v2_m0::DeviceResult,
    v2_m0_transport::{AgentToHub, HubToAgent},
};
use std::fmt;

pub mod proto {
    tonic::include_proto!("cumg.v2");
}

use proto::{AgentFrame, HubFrame};

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
    enforce_bound(bytes.len(), MAX_GRPC_APPLICATION_MESSAGE_BYTES)?;
    Ok(HubFrame {
        signed_message_json: bytes,
    })
}

pub fn decode_hub_frame(frame: HubFrame) -> Result<HubToAgent, GrpcCarrierError> {
    enforce_bound(
        frame.signed_message_json.len(),
        MAX_GRPC_APPLICATION_MESSAGE_BYTES,
    )?;
    serde_json::from_slice(&frame.signed_message_json).map_err(GrpcCarrierError::Serialization)
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
    use crate::v2_m0_transport::{AgentHello, HUB_AGENT_SCHEMA_VERSION};
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
}
