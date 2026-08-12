//! gRPC bidirectional transport candidate for V2-M1.
//!
//! This module intentionally keeps the existing signed V2 application messages
//! intact. gRPC/HTTP2 replaces the custom length-prefixed carrier first; the
//! security and execution semantics stay transport-neutral and can be migrated
//! to native protobuf fields independently later.

use crate::v2_m0_transport::{AgentToHub, HubToAgent};
use std::fmt;

pub mod proto {
    tonic::include_proto!("cumg.v2");
}

use proto::{AgentFrame, HubFrame};

pub const MAX_GRPC_APPLICATION_MESSAGE_BYTES: usize = 64 * 1024;
// Tonic limits the encoded Protobuf message, which includes the bytes-field tag
// and varint length in addition to the signed application payload. Keep the
// semantic application bound at 64 KiB while allowing bounded carrier overhead.
pub const MAX_GRPC_TRANSPORT_MESSAGE_BYTES: usize = MAX_GRPC_APPLICATION_MESSAGE_BYTES + 1024;

pub fn encode_agent_frame(message: &AgentToHub) -> Result<AgentFrame, GrpcCarrierError> {
    let bytes = serde_json::to_vec(message).map_err(GrpcCarrierError::Serialization)?;
    enforce_bound(bytes.len())?;
    Ok(AgentFrame {
        signed_message_json: bytes,
    })
}

pub fn decode_agent_frame(frame: AgentFrame) -> Result<AgentToHub, GrpcCarrierError> {
    enforce_bound(frame.signed_message_json.len())?;
    serde_json::from_slice(&frame.signed_message_json).map_err(GrpcCarrierError::Serialization)
}

pub fn encode_hub_frame(message: &HubToAgent) -> Result<HubFrame, GrpcCarrierError> {
    let bytes = serde_json::to_vec(message).map_err(GrpcCarrierError::Serialization)?;
    enforce_bound(bytes.len())?;
    Ok(HubFrame {
        signed_message_json: bytes,
    })
}

pub fn decode_hub_frame(frame: HubFrame) -> Result<HubToAgent, GrpcCarrierError> {
    enforce_bound(frame.signed_message_json.len())?;
    serde_json::from_slice(&frame.signed_message_json).map_err(GrpcCarrierError::Serialization)
}

fn enforce_bound(size: usize) -> Result<(), GrpcCarrierError> {
    if size > MAX_GRPC_APPLICATION_MESSAGE_BYTES {
        Err(GrpcCarrierError::MessageTooLarge(size))
    } else {
        Ok(())
    }
}

#[derive(Debug)]
pub enum GrpcCarrierError {
    Serialization(serde_json::Error),
    MessageTooLarge(usize),
}

impl fmt::Display for GrpcCarrierError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Serialization(error) => write!(f, "gRPC carrier serialization error: {error}"),
            Self::MessageTooLarge(size) => write!(
                f,
                "gRPC application message {size} bytes exceeds {MAX_GRPC_APPLICATION_MESSAGE_BYTES}"
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
    fn protobuf_carrier_keeps_application_message_bound() {
        let frame = AgentFrame {
            signed_message_json: vec![0; MAX_GRPC_APPLICATION_MESSAGE_BYTES + 1],
        };
        assert!(matches!(
            decode_agent_frame(frame),
            Err(GrpcCarrierError::MessageTooLarge(size))
                if size == MAX_GRPC_APPLICATION_MESSAGE_BYTES + 1
        ));
    }
}
