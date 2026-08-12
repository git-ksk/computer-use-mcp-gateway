//! Minimal fixed-set multi-device composition for the V2 P1 invariant proof.
//!
//! This module deliberately does not implement discovery, mutable enrollment,
//! fleet UX, or a generic device registry. Each provisioned device remains an
//! ordinary `SingleDeviceHub` with the P0 authoritative operation state machine,
//! persistence boundary, queue, session limits, and generation fencing intact.

use crate::v2_m1_hub::{
    HubHandle, HubProvisionedMaterial, HubServiceConfig, HubServiceError, SingleDeviceHub,
};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone)]
pub struct FixedMultiDeviceHub {
    services: Arc<HashMap<String, SingleDeviceHub>>,
    handles: Arc<HashMap<String, HubHandle>>,
}

impl FixedMultiDeviceHub {
    /// Build an immutable set of explicitly provisioned devices.
    ///
    /// Every device retains a distinct state directory and its own P0
    /// `SingleDeviceHub`. There is intentionally no cross-device shared queue or
    /// admission controller that could bypass one device's ownership fence.
    pub fn new(
        devices: Vec<(HubServiceConfig, HubProvisionedMaterial)>,
    ) -> Result<Self, FixedMultiDeviceHubError> {
        if devices.is_empty() {
            return Err(FixedMultiDeviceHubError::EmptyProvisioning);
        }

        let mut state_dirs = HashSet::<PathBuf>::new();
        let mut services = HashMap::new();
        let mut handles = HashMap::new();
        for (config, material) in devices {
            if !state_dirs.insert(config.state_dir.clone()) {
                return Err(FixedMultiDeviceHubError::DuplicateStateDirectory(
                    config.state_dir,
                ));
            }
            let (service, handle) = SingleDeviceHub::new(config, material)?;
            let device_id = service.device_id().to_owned();
            if services.insert(device_id.clone(), service).is_some() {
                return Err(FixedMultiDeviceHubError::DuplicateDeviceId(device_id));
            }
            handles.insert(device_id, handle);
        }

        Ok(Self {
            services: Arc::new(services),
            handles: Arc::new(handles),
        })
    }

    /// Select the pre-provisioned gRPC service for one exact stable device ID.
    pub fn service_for_device(&self, device_id: &str) -> Option<SingleDeviceHub> {
        self.services.get(device_id).cloned()
    }

    /// Select the northbound handle for one exact stable device ID.
    pub fn handle_for_device(&self, device_id: &str) -> Option<HubHandle> {
        self.handles.get(device_id).cloned()
    }

    pub fn provisioned_device_count(&self) -> usize {
        self.services.len()
    }
}

#[derive(Debug)]
pub enum FixedMultiDeviceHubError {
    EmptyProvisioning,
    DuplicateDeviceId(String),
    DuplicateStateDirectory(PathBuf),
    Hub(HubServiceError),
}

impl From<HubServiceError> for FixedMultiDeviceHubError {
    fn from(error: HubServiceError) -> Self {
        Self::Hub(error)
    }
}

impl fmt::Display for FixedMultiDeviceHubError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyProvisioning => write!(f, "at least one fixed device must be provisioned"),
            Self::DuplicateDeviceId(device_id) => {
                write!(f, "duplicate fixed device id: {device_id}")
            }
            Self::DuplicateStateDirectory(path) => {
                write!(f, "fixed devices must not share state directory: {}", path.display())
            }
            Self::Hub(error) => write!(f, "failed to construct fixed device Hub: {error}"),
        }
    }
}

impl std::error::Error for FixedMultiDeviceHubError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Hub(error) => Some(error),
            _ => None,
        }
    }
}
