//! V2-M0 control-plane prototype.
//!
//! This module is deliberately isolated from the V1 MCP gateway. It proves the
//! control semantics before any Hub/Agent transport is selected.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fmt;

pub const CONTROL_SCHEMA_VERSION: u16 = 3;
pub const CAPABILITY_SCHEMA_VERSION: u16 = 3;
pub const MAX_GRANT_LIFETIME_MS: u64 = 5 * 60 * 1000;
pub const MAX_TYPE_TEXT_BYTES: usize = 32 * 1024;
pub const MAX_SCREENSHOT_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_WINDOW_RESULTS: usize = 512;
pub const MAX_UI_ELEMENTS: usize = 512;
pub const MAX_UI_QUERY_BYTES: usize = 1_024;
pub const MAX_UI_TEXT_BYTES: usize = 2 * 1024;
pub const MAX_UI_REF_BYTES: usize = 512;
pub const MAX_UI_PREDICATES: usize = 8;
pub const MAX_KEYBOARD_MODIFIERS: usize = 5;
pub const MAX_MENU_PATH_SEGMENTS: usize = 16;
pub const MAX_MENU_SEGMENT_BYTES: usize = 200;
pub const MAX_CLIPBOARD_TEXT_BYTES: usize = 1024 * 1024;
pub const MAX_CLIPBOARD_TYPES: usize = 64;
pub const MAX_CLIPBOARD_TYPE_BYTES: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityClass {
    Observe,
    Interact,
    System,
    Dangerous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceCapability {
    ListApplications,
    ScreenGeometry,
    Screenshot,
    PointerClick,
    PointerDrag,
    TypeText,
    ExecuteProcess,
    Shell,
    ReadFile,
    ListDirectory,
    ListWindows,
    LaunchApplication,
    InspectWindow,
    VerifyUiState,
    TerminateApplication,
    ActivateWindow,
    SetWindowFrame,
    InvokeMenu,
    KeyboardInput,
    Scroll,
    ClipboardRead,
    ClipboardWrite,
    PointerPosition,
    MovePointer,
    SetUiValue,
    CaptureRegion,
    DesktopScope,
}

impl DeviceCapability {
    pub fn class(self) -> CapabilityClass {
        match self {
            Self::ListApplications
            | Self::ScreenGeometry
            | Self::Screenshot
            | Self::ReadFile
            | Self::ListDirectory
            | Self::ListWindows
            | Self::InspectWindow
            | Self::VerifyUiState
            | Self::ClipboardRead
            | Self::PointerPosition
            | Self::CaptureRegion => CapabilityClass::Observe,
            Self::PointerClick
            | Self::PointerDrag
            | Self::TypeText
            | Self::InvokeMenu
            | Self::KeyboardInput
            | Self::Scroll
            | Self::ClipboardWrite
            | Self::MovePointer
            | Self::SetUiValue => CapabilityClass::Interact,
            Self::LaunchApplication
            | Self::ActivateWindow
            | Self::SetWindowFrame
            | Self::DesktopScope => CapabilityClass::System,
            // Direct process/free-form shell and forced process termination can
            // mutate or destroy arbitrary local state.
            Self::ExecuteProcess | Self::Shell | Self::TerminateApplication => {
                CapabilityClass::Dangerous
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PointerButton {
    Left,
    Right,
    Middle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputDeliveryMode {
    Background,
    Foreground,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyboardModifier {
    Meta,
    Shift,
    Alt,
    Control,
    Function,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "space", rename_all = "snake_case")]
pub enum PointerTarget {
    DesktopPhysical {
        x: i32,
        y: i32,
    },
    WindowPhysical {
        process_id: u32,
        window_id: u64,
        x: i32,
        y: i32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InputTarget {
    Desktop,
    Window {
        process_id: u32,
        window_id: Option<u64>,
    },
    WindowPoint {
        process_id: u32,
        window_id: u64,
        x: i32,
        y: i32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScrollDirection {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScrollGranularity {
    Line,
    Page,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScrollTarget {
    Window {
        process_id: u32,
        window_id: Option<u64>,
    },
    WindowPoint {
        process_id: u32,
        window_id: u64,
        x: i32,
        y: i32,
    },
    DesktopPoint {
        x: i32,
        y: i32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessEnvVar {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessRequest {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub env: Vec<ProcessEnvVar>,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellRequest {
    pub command: String,
    pub cwd: String,
    pub env: Vec<ProcessEnvVar>,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessOutput {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub timed_out: bool,
    pub cancelled: bool,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DirectoryEntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryEntry {
    pub name: String,
    pub kind: DirectoryEntryKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowInfo {
    pub window_id: u64,
    pub process_id: u32,
    pub application: String,
    pub title: String,
    pub bounds: UiRect,
    pub is_on_screen: bool,
    pub on_current_workspace: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiRole {
    Window,
    Button,
    Text,
    TextField,
    Checkbox,
    RadioButton,
    Link,
    Menu,
    MenuItem,
    Toolbar,
    Tab,
    List,
    ListItem,
    Table,
    Row,
    Cell,
    Group,
    Image,
    Slider,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiElement {
    pub element_ref: String,
    pub role: UiRole,
    pub label: Option<String>,
    pub value: Option<String>,
    pub bounds: Option<UiRect>,
    pub enabled: Option<bool>,
    pub selected: Option<bool>,
    pub parent_ref: Option<String>,
    pub depth: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiElementSelector {
    pub role: Option<UiRole>,
    pub label_contains: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UiPredicate {
    WindowExists {
        exists: bool,
    },
    WindowBounds {
        bounds: UiRect,
        tolerance_px: u32,
    },
    ElementExists {
        selector: UiElementSelector,
    },
    ElementState {
        selector: UiElementSelector,
        enabled: Option<bool>,
        selected: Option<bool>,
        value_equals: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
    Satisfied,
    Unsatisfied,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiPredicateResult {
    pub status: VerificationStatus,
    pub unknown_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiImage {
    pub data_base64: String,
    pub mime_type: String,
    pub width_pixels: u32,
    pub height_pixels: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DeviceCommand {
    ListApplications,
    ScreenGeometry,
    Screenshot,
    ScreenshotContextual {
        context_id: String,
    },
    PointerClick {
        x: i32,
        y: i32,
        button: PointerButton,
    },
    PointerClickAdvanced {
        context_id: Option<String>,
        target: PointerTarget,
        button: PointerButton,
        click_count: u8,
        modifiers: Vec<KeyboardModifier>,
        delivery: InputDeliveryMode,
    },
    PointerDrag {
        from_x: i32,
        from_y: i32,
        to_x: i32,
        to_y: i32,
        duration_ms: u64,
    },
    PointerDragAdvanced {
        context_id: Option<String>,
        from: PointerTarget,
        to: PointerTarget,
        button: PointerButton,
        modifiers: Vec<KeyboardModifier>,
        delivery: InputDeliveryMode,
        duration_ms: u64,
        steps: u16,
    },
    TypeText {
        text: String,
    },
    TypeTextAdvanced {
        context_id: Option<String>,
        text: String,
        target: InputTarget,
        delivery: InputDeliveryMode,
        delay_ms: u16,
    },
    ExecuteProcess {
        request: ProcessRequest,
    },
    Shell {
        request: ShellRequest,
    },
    ReadFile {
        path: String,
    },
    ListDirectory {
        path: String,
    },
    ListWindows {
        process_id: Option<u32>,
        on_screen_only: bool,
    },
    LaunchApplication {
        identifier: Option<String>,
        name: Option<String>,
        targets: Vec<String>,
        new_instance: bool,
    },
    InspectWindow {
        process_id: u32,
        window_id: u64,
        query: Option<String>,
        max_elements: u32,
        max_depth: u32,
        include_screenshot: bool,
    },
    InspectWindowContextual {
        context_id: String,
        process_id: u32,
        window_id: u64,
        query: Option<String>,
        max_elements: u32,
        max_depth: u32,
        include_screenshot: bool,
    },
    VerifyUiState {
        process_id: u32,
        window_id: u64,
        predicates: Vec<UiPredicate>,
        timeout_ms: u64,
        stable_samples: u8,
        include_screenshot: bool,
    },
    VerifyUiStateContextual {
        context_id: String,
        process_id: u32,
        window_id: u64,
        predicates: Vec<UiPredicate>,
        timeout_ms: u64,
        stable_samples: u8,
        include_screenshot: bool,
    },
    TerminateApplication {
        process_id: u32,
    },
    ActivateWindow {
        process_id: u32,
        window_id: Option<u64>,
    },
    SetWindowFrame {
        context_id: Option<String>,
        process_id: u32,
        window_id: u64,
        bounds: UiRect,
    },
    InvokeMenu {
        context_id: Option<String>,
        process_id: u32,
        window_id: u64,
        path: Vec<String>,
    },
    KeyboardInput {
        context_id: Option<String>,
        key: String,
        modifiers: Vec<KeyboardModifier>,
        target: InputTarget,
        delivery: InputDeliveryMode,
    },
    Scroll {
        context_id: Option<String>,
        direction: ScrollDirection,
        granularity: ScrollGranularity,
        amount: u8,
        target: ScrollTarget,
        delivery: InputDeliveryMode,
    },
    ClipboardRead {
        context_id: Option<String>,
        include_text: bool,
    },
    ClipboardWrite {
        context_id: Option<String>,
        text: String,
    },
    PointerPosition {
        context_id: Option<String>,
    },
    MovePointer {
        context_id: String,
        x: i32,
        y: i32,
    },
    SetUiValue {
        context_id: String,
        process_id: u32,
        window_id: u64,
        element_ref: String,
        value: String,
    },
    CaptureRegion {
        context_id: Option<String>,
        process_id: u32,
        window_id: u64,
        bounds: UiRect,
    },
    ExpandInteractionScope {
        context_id: String,
        reason: String,
    },
}

impl DeviceCommand {
    pub fn capability(&self) -> DeviceCapability {
        match self {
            Self::ListApplications => DeviceCapability::ListApplications,
            Self::ScreenGeometry => DeviceCapability::ScreenGeometry,
            Self::Screenshot | Self::ScreenshotContextual { .. } => DeviceCapability::Screenshot,
            Self::PointerClick { .. } | Self::PointerClickAdvanced { .. } => {
                DeviceCapability::PointerClick
            }
            Self::PointerDrag { .. } | Self::PointerDragAdvanced { .. } => {
                DeviceCapability::PointerDrag
            }
            Self::TypeText { .. } | Self::TypeTextAdvanced { .. } => DeviceCapability::TypeText,
            Self::ExecuteProcess { .. } => DeviceCapability::ExecuteProcess,
            Self::Shell { .. } => DeviceCapability::Shell,
            Self::ReadFile { .. } => DeviceCapability::ReadFile,
            Self::ListDirectory { .. } => DeviceCapability::ListDirectory,
            Self::ListWindows { .. } => DeviceCapability::ListWindows,
            Self::LaunchApplication { .. } => DeviceCapability::LaunchApplication,
            Self::InspectWindow { .. } | Self::InspectWindowContextual { .. } => {
                DeviceCapability::InspectWindow
            }
            Self::VerifyUiState { .. } | Self::VerifyUiStateContextual { .. } => {
                DeviceCapability::VerifyUiState
            }
            Self::TerminateApplication { .. } => DeviceCapability::TerminateApplication,
            Self::ActivateWindow { .. } => DeviceCapability::ActivateWindow,
            Self::SetWindowFrame { .. } => DeviceCapability::SetWindowFrame,
            Self::InvokeMenu { .. } => DeviceCapability::InvokeMenu,
            Self::KeyboardInput { .. } => DeviceCapability::KeyboardInput,
            Self::Scroll { .. } => DeviceCapability::Scroll,
            Self::ClipboardRead { .. } => DeviceCapability::ClipboardRead,
            Self::ClipboardWrite { .. } => DeviceCapability::ClipboardWrite,
            Self::PointerPosition { .. } => DeviceCapability::PointerPosition,
            Self::MovePointer { .. } => DeviceCapability::MovePointer,
            Self::SetUiValue { .. } => DeviceCapability::SetUiValue,
            Self::CaptureRegion { .. } => DeviceCapability::CaptureRegion,
            Self::ExpandInteractionScope { .. } => DeviceCapability::DesktopScope,
        }
    }

    pub fn class(&self) -> CapabilityClass {
        self.capability().class()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityAdvertisement {
    pub backend: String,
    pub backend_version: String,
    pub platform: String,
    pub capability_schema_version: u16,
    pub revision: u64,
    pub supported: Vec<DeviceCapability>,
}

impl CapabilityAdvertisement {
    pub fn supports(&self, capability: DeviceCapability) -> bool {
        self.supported.contains(&capability)
    }
}

#[derive(Clone)]
pub struct DeviceIdentity {
    signing_key: SigningKey,
}

impl DeviceIdentity {
    pub fn generate() -> Self {
        Self {
            signing_key: SigningKey::generate(&mut OsRng),
        }
    }

    pub fn from_secret_key_bytes(secret_key: [u8; 32]) -> Self {
        Self {
            signing_key: SigningKey::from_bytes(&secret_key),
        }
    }

    pub(crate) fn secret_key_bytes(&self) -> [u8; 32] {
        self.signing_key.to_bytes()
    }

    pub fn public_key(&self) -> Vec<u8> {
        self.signing_key.verifying_key().to_bytes().to_vec()
    }

    pub fn enrollment_proof(&self, challenge: &[u8]) -> Vec<u8> {
        self.signing_key.sign(challenge).to_bytes().to_vec()
    }

    pub fn sign_message(&self, message: &[u8]) -> Vec<u8> {
        self.signing_key.sign(message).to_bytes().to_vec()
    }

    pub fn verifying_key(&self) -> VerifyingKey {
        self.signing_key.verifying_key()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceSession {
    pub device_id: String,
    pub generation: u64,
    pub capabilities: CapabilityAdvertisement,
}

#[derive(Debug, Clone)]
struct EnrolledDevice {
    verifying_key: VerifyingKey,
    generation: u64,
    capabilities: Option<CapabilityAdvertisement>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrolledDeviceSnapshot {
    pub device_id: String,
    pub verifying_key: [u8; 32],
    pub generation: u64,
    pub capabilities: Option<CapabilityAdvertisement>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceRegistrySnapshot {
    pub schema_version: u16,
    pub devices: Vec<EnrolledDeviceSnapshot>,
    pub revoked_device_ids: Vec<String>,
}

#[derive(Debug, Default)]
pub struct DeviceRegistry {
    devices: HashMap<String, EnrolledDevice>,
    revoked: HashSet<String>,
}

impl DeviceRegistry {
    pub fn snapshot(&self) -> DeviceRegistrySnapshot {
        let mut devices: Vec<_> = self
            .devices
            .iter()
            .map(|(device_id, device)| EnrolledDeviceSnapshot {
                device_id: device_id.clone(),
                verifying_key: device.verifying_key.to_bytes(),
                generation: device.generation,
                capabilities: device.capabilities.clone(),
            })
            .collect();
        devices.sort_by(|left, right| left.device_id.cmp(&right.device_id));
        let mut revoked_device_ids: Vec<_> = self.revoked.iter().cloned().collect();
        revoked_device_ids.sort();
        DeviceRegistrySnapshot {
            schema_version: CONTROL_SCHEMA_VERSION,
            devices,
            revoked_device_ids,
        }
    }

    pub fn from_snapshot(snapshot: DeviceRegistrySnapshot) -> Result<Self, ControlError> {
        if snapshot.schema_version != CONTROL_SCHEMA_VERSION {
            return Err(ControlError::UnsupportedControlSchema {
                got: snapshot.schema_version,
            });
        }
        let mut devices = HashMap::new();
        for device in snapshot.devices {
            if devices.contains_key(&device.device_id) {
                return Err(ControlError::InvalidRegistrySnapshot);
            }
            let verifying_key = VerifyingKey::from_bytes(&device.verifying_key)
                .map_err(|_| ControlError::InvalidRegistrySnapshot)?;
            if let Some(capabilities) = &device.capabilities {
                if capabilities.capability_schema_version != CAPABILITY_SCHEMA_VERSION {
                    return Err(ControlError::UnsupportedCapabilitySchema {
                        got: capabilities.capability_schema_version,
                    });
                }
            }
            devices.insert(
                device.device_id,
                EnrolledDevice {
                    verifying_key,
                    generation: device.generation,
                    capabilities: device.capabilities,
                },
            );
        }
        let revoked = snapshot.revoked_device_ids.into_iter().collect();
        Ok(Self { devices, revoked })
    }

    /// Offline administrative provisioning for a device public key that was
    /// transferred through an operator-controlled trust channel. Runtime
    /// enrollment should still use challenge/proof when keys are introduced in-band.
    pub fn provision_trusted_device(&mut self, verifying_key: VerifyingKey) -> String {
        let key_bytes = verifying_key.to_bytes();
        let device_id = format!("dev_{}", hex(&key_bytes));
        self.devices
            .entry(device_id.clone())
            .or_insert(EnrolledDevice {
                verifying_key,
                generation: 0,
                capabilities: None,
            });
        device_id
    }

    pub fn device_verifier(&self, device_id: &str) -> Result<VerifyingKey, ControlError> {
        self.devices
            .get(device_id)
            .map(|device| device.verifying_key)
            .ok_or(ControlError::UnknownDevice)
    }

    pub fn disconnect(&mut self, device_id: &str) -> Result<(), ControlError> {
        let device = self
            .devices
            .get_mut(device_id)
            .ok_or(ControlError::UnknownDevice)?;
        device.capabilities = None;
        Ok(())
    }

    pub fn mark_all_offline(&mut self) {
        for device in self.devices.values_mut() {
            device.capabilities = None;
        }
    }

    pub fn enrollment_challenge() -> [u8; 32] {
        let mut challenge = [0_u8; 32];
        OsRng.fill_bytes(&mut challenge);
        challenge
    }

    pub fn enroll(
        &mut self,
        public_key: &[u8],
        challenge: &[u8],
        proof: &[u8],
    ) -> Result<String, ControlError> {
        let key_bytes: [u8; 32] = public_key
            .try_into()
            .map_err(|_| ControlError::InvalidDeviceIdentity)?;
        let verifying_key = VerifyingKey::from_bytes(&key_bytes)
            .map_err(|_| ControlError::InvalidDeviceIdentity)?;
        let sig_bytes: [u8; 64] = proof
            .try_into()
            .map_err(|_| ControlError::InvalidDeviceIdentity)?;
        let signature = Signature::from_bytes(&sig_bytes);
        verifying_key
            .verify(challenge, &signature)
            .map_err(|_| ControlError::InvalidEnrollmentProof)?;

        let device_id = format!("dev_{}", hex(&key_bytes));
        self.devices
            .entry(device_id.clone())
            .or_insert(EnrolledDevice {
                verifying_key,
                generation: 0,
                capabilities: None,
            });
        Ok(device_id)
    }

    pub fn revoke_device(&mut self, device_id: &str) {
        self.revoked.insert(device_id.to_owned());
    }

    pub fn verify_device_signature(
        &self,
        device_id: &str,
        message: &[u8],
        proof: &[u8],
    ) -> Result<(), ControlError> {
        if self.revoked.contains(device_id) {
            return Err(ControlError::DeviceRevoked);
        }
        let device = self
            .devices
            .get(device_id)
            .ok_or(ControlError::UnknownDevice)?;
        let sig_bytes: [u8; 64] = proof
            .try_into()
            .map_err(|_| ControlError::InvalidDeviceSignature)?;
        let signature = Signature::from_bytes(&sig_bytes);
        device
            .verifying_key
            .verify(message, &signature)
            .map_err(|_| ControlError::InvalidDeviceSignature)
    }

    pub fn rotate_device_key(
        &mut self,
        device_id: &str,
        new_public_key: &[u8],
        rotation_message: &[u8],
        old_proof: &[u8],
        new_proof: &[u8],
    ) -> Result<(), ControlError> {
        if self.revoked.contains(device_id) {
            return Err(ControlError::DeviceRevoked);
        }
        let new_key_bytes: [u8; 32] = new_public_key
            .try_into()
            .map_err(|_| ControlError::InvalidDeviceIdentity)?;
        let new_verifying_key = VerifyingKey::from_bytes(&new_key_bytes)
            .map_err(|_| ControlError::InvalidDeviceIdentity)?;
        let old_sig_bytes: [u8; 64] = old_proof
            .try_into()
            .map_err(|_| ControlError::InvalidDeviceSignature)?;
        let new_sig_bytes: [u8; 64] = new_proof
            .try_into()
            .map_err(|_| ControlError::InvalidDeviceSignature)?;
        let old_signature = Signature::from_bytes(&old_sig_bytes);
        let new_signature = Signature::from_bytes(&new_sig_bytes);
        let device = self
            .devices
            .get_mut(device_id)
            .ok_or(ControlError::UnknownDevice)?;
        device
            .verifying_key
            .verify(rotation_message, &old_signature)
            .map_err(|_| ControlError::InvalidDeviceRotationProof)?;
        new_verifying_key
            .verify(rotation_message, &new_signature)
            .map_err(|_| ControlError::InvalidDeviceRotationProof)?;
        device.verifying_key = new_verifying_key;
        device.generation = device.generation.saturating_add(1).max(1);
        device.capabilities = None;
        Ok(())
    }

    pub fn connect(
        &mut self,
        device_id: &str,
        capabilities: CapabilityAdvertisement,
    ) -> Result<DeviceSession, ControlError> {
        if self.revoked.contains(device_id) {
            return Err(ControlError::DeviceRevoked);
        }
        if capabilities.capability_schema_version != CAPABILITY_SCHEMA_VERSION {
            return Err(ControlError::UnsupportedCapabilitySchema {
                got: capabilities.capability_schema_version,
            });
        }
        let device = self
            .devices
            .get_mut(device_id)
            .ok_or(ControlError::UnknownDevice)?;
        // Touch the key so the registry cannot accidentally become a plain name registry.
        let _cryptographic_identity = device.verifying_key.to_bytes();
        device.generation = device.generation.saturating_add(1).max(1);
        device.capabilities = Some(capabilities.clone());
        Ok(DeviceSession {
            device_id: device_id.to_owned(),
            generation: device.generation,
            capabilities,
        })
    }

    pub fn current_session(&self, device_id: &str) -> Result<DeviceSession, ControlError> {
        let device = self
            .devices
            .get(device_id)
            .ok_or(ControlError::UnknownDevice)?;
        let capabilities = device
            .capabilities
            .clone()
            .ok_or(ControlError::DeviceOffline)?;
        Ok(DeviceSession {
            device_id: device_id.to_owned(),
            generation: device.generation,
            capabilities,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantPayload {
    pub schema_version: u16,
    pub issuer_key_id: String,
    pub grant_id: String,
    pub device_id: String,
    pub capability: CapabilityClass,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_capability: Option<DeviceCapability>,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantToken {
    pub payload: GrantPayload,
    pub signature: Vec<u8>,
}

#[derive(Clone)]
pub struct GrantAuthority {
    signing_key: SigningKey,
}

impl GrantAuthority {
    pub fn generate() -> Self {
        Self {
            signing_key: SigningKey::generate(&mut OsRng),
        }
    }

    pub fn from_secret_key_bytes(secret_key: [u8; 32]) -> Self {
        Self {
            signing_key: SigningKey::from_bytes(&secret_key),
        }
    }

    pub(crate) fn secret_key_bytes(&self) -> [u8; 32] {
        self.signing_key.to_bytes()
    }

    pub fn verifier(&self) -> VerifyingKey {
        self.signing_key.verifying_key()
    }

    pub fn key_id(&self) -> String {
        verifying_key_id(&self.verifier())
    }

    pub fn issue(
        &self,
        device_id: &str,
        capability: CapabilityClass,
        now_ms: u64,
        ttl_ms: u64,
    ) -> Result<GrantToken, ControlError> {
        if ttl_ms == 0 || ttl_ms > MAX_GRANT_LIFETIME_MS {
            return Err(ControlError::InvalidGrantLifetime);
        }
        let expires_at_ms = now_ms
            .checked_add(ttl_ms)
            .ok_or(ControlError::InvalidGrantLifetime)?;
        let mut random = [0_u8; 16];
        OsRng.fill_bytes(&mut random);
        let payload = GrantPayload {
            schema_version: CONTROL_SCHEMA_VERSION,
            issuer_key_id: self.key_id(),
            grant_id: format!("grant_{}", hex(&random)),
            device_id: device_id.to_owned(),
            capability,
            device_capability: None,
            issued_at_ms: now_ms,
            expires_at_ms,
        };
        let bytes = canonical_grant_bytes(&payload)?;
        let signature = self.signing_key.sign(&bytes).to_bytes().to_vec();
        Ok(GrantToken { payload, signature })
    }

    pub fn issue_for_device_capability(
        &self,
        device_id: &str,
        capability: DeviceCapability,
        now_ms: u64,
        ttl_ms: u64,
    ) -> Result<GrantToken, ControlError> {
        if ttl_ms == 0 || ttl_ms > MAX_GRANT_LIFETIME_MS {
            return Err(ControlError::InvalidGrantLifetime);
        }
        let expires_at_ms = now_ms
            .checked_add(ttl_ms)
            .ok_or(ControlError::InvalidGrantLifetime)?;
        let mut random = [0_u8; 16];
        OsRng.fill_bytes(&mut random);
        let payload = GrantPayload {
            schema_version: CONTROL_SCHEMA_VERSION,
            issuer_key_id: self.key_id(),
            grant_id: format!("grant_{}", hex(&random)),
            device_id: device_id.to_owned(),
            capability: capability.class(),
            device_capability: Some(capability),
            issued_at_ms: now_ms,
            expires_at_ms,
        };
        let bytes = canonical_grant_bytes(&payload)?;
        let signature = self.signing_key.sign(&bytes).to_bytes().to_vec();
        Ok(GrantToken { payload, signature })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsumedGrantSnapshot {
    pub grant_id: String,
    pub expires_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantLedgerSnapshot {
    pub schema_version: u16,
    pub verifier_keys: Vec<[u8; 32]>,
    pub consumed_grants: Vec<ConsumedGrantSnapshot>,
    pub revoked_grant_ids: Vec<String>,
}

#[derive(Debug)]
pub struct GrantLedger {
    verifiers: HashMap<String, VerifyingKey>,
    consumed: HashMap<String, u64>,
    revoked: HashSet<String>,
}

impl GrantLedger {
    pub fn new(verifier: VerifyingKey) -> Self {
        let mut verifiers = HashMap::new();
        verifiers.insert(verifying_key_id(&verifier), verifier);
        Self {
            verifiers,
            consumed: HashMap::new(),
            revoked: HashSet::new(),
        }
    }

    pub fn snapshot(&self) -> GrantLedgerSnapshot {
        let mut verifier_keys: Vec<_> = self
            .verifiers
            .values()
            .map(VerifyingKey::to_bytes)
            .collect();
        verifier_keys.sort();
        let mut consumed_grants: Vec<_> = self
            .consumed
            .iter()
            .map(|(grant_id, expires_at_ms)| ConsumedGrantSnapshot {
                grant_id: grant_id.clone(),
                expires_at_ms: *expires_at_ms,
            })
            .collect();
        consumed_grants.sort_by(|left, right| left.grant_id.cmp(&right.grant_id));
        let mut revoked_grant_ids: Vec<_> = self.revoked.iter().cloned().collect();
        revoked_grant_ids.sort();
        GrantLedgerSnapshot {
            schema_version: CONTROL_SCHEMA_VERSION,
            verifier_keys,
            consumed_grants,
            revoked_grant_ids,
        }
    }

    pub fn from_snapshot(snapshot: GrantLedgerSnapshot) -> Result<Self, ControlError> {
        if snapshot.schema_version != CONTROL_SCHEMA_VERSION {
            return Err(ControlError::UnsupportedControlSchema {
                got: snapshot.schema_version,
            });
        }
        if snapshot.verifier_keys.is_empty() {
            return Err(ControlError::InvalidGrantLedgerSnapshot);
        }
        let mut verifiers = HashMap::new();
        for key_bytes in snapshot.verifier_keys {
            let verifier = VerifyingKey::from_bytes(&key_bytes)
                .map_err(|_| ControlError::InvalidGrantLedgerSnapshot)?;
            verifiers.insert(verifying_key_id(&verifier), verifier);
        }
        let mut consumed = HashMap::new();
        for entry in snapshot.consumed_grants {
            if entry.grant_id.trim().is_empty()
                || entry.expires_at_ms == 0
                || consumed
                    .insert(entry.grant_id, entry.expires_at_ms)
                    .is_some()
            {
                return Err(ControlError::InvalidGrantLedgerSnapshot);
            }
        }
        Ok(Self {
            verifiers,
            consumed,
            revoked: snapshot.revoked_grant_ids.into_iter().collect(),
        })
    }

    pub fn trust_verifier(&mut self, verifier: VerifyingKey) -> String {
        let key_id = verifying_key_id(&verifier);
        self.verifiers.insert(key_id.clone(), verifier);
        key_id
    }

    pub fn retire_verifier(&mut self, key_id: &str) -> bool {
        self.verifiers.remove(key_id).is_some()
    }

    pub fn revoke(&mut self, grant_id: &str) {
        self.revoked.insert(grant_id.to_owned());
    }

    pub fn authorize_once(
        &mut self,
        token: &GrantToken,
        device_id: &str,
        required: CapabilityClass,
        now_ms: u64,
    ) -> Result<(), ControlError> {
        self.validate_unconsumed_grant(token, device_id, now_ms)?;
        let payload = &token.payload;
        if payload.capability != required {
            return Err(ControlError::CapabilityDenied {
                granted: payload.capability,
                required,
            });
        }
        self.consume(payload);
        Ok(())
    }

    pub fn authorize_device_capability_once(
        &mut self,
        token: &GrantToken,
        device_id: &str,
        required: DeviceCapability,
        now_ms: u64,
    ) -> Result<(), ControlError> {
        self.validate_unconsumed_grant(token, device_id, now_ms)?;
        let payload = &token.payload;
        if payload.capability != required.class() {
            return Err(ControlError::CapabilityDenied {
                granted: payload.capability,
                required: required.class(),
            });
        }
        if payload.device_capability != Some(required) {
            return Err(ControlError::DeviceCapabilityGrantMismatch {
                granted: payload.device_capability,
                required,
            });
        }
        self.consume(payload);
        Ok(())
    }

    fn validate_unconsumed_grant(
        &mut self,
        token: &GrantToken,
        device_id: &str,
        now_ms: u64,
    ) -> Result<(), ControlError> {
        self.verify_signature(token)?;
        let payload = &token.payload;
        if payload.schema_version != CONTROL_SCHEMA_VERSION {
            return Err(ControlError::UnsupportedControlSchema {
                got: payload.schema_version,
            });
        }
        if payload.device_id != device_id {
            return Err(ControlError::GrantDeviceMismatch);
        }
        let lifetime = payload
            .expires_at_ms
            .checked_sub(payload.issued_at_ms)
            .ok_or(ControlError::InvalidGrantLifetime)?;
        if lifetime == 0 || lifetime > MAX_GRANT_LIFETIME_MS {
            return Err(ControlError::InvalidGrantLifetime);
        }
        self.consumed
            .retain(|_, expires_at_ms| *expires_at_ms > now_ms);
        if now_ms < payload.issued_at_ms {
            return Err(ControlError::GrantNotYetValid);
        }
        if now_ms >= payload.expires_at_ms {
            return Err(ControlError::GrantExpired);
        }
        if self.revoked.contains(&payload.grant_id) {
            return Err(ControlError::GrantRevoked);
        }
        if self.consumed.contains_key(&payload.grant_id) {
            return Err(ControlError::GrantReplay);
        }
        Ok(())
    }

    fn consume(&mut self, payload: &GrantPayload) {
        self.consumed
            .insert(payload.grant_id.clone(), payload.expires_at_ms);
    }

    fn verify_signature(&self, token: &GrantToken) -> Result<(), ControlError> {
        let sig_bytes: [u8; 64] = token
            .signature
            .as_slice()
            .try_into()
            .map_err(|_| ControlError::InvalidGrantSignature)?;
        let signature = Signature::from_bytes(&sig_bytes);
        let bytes = canonical_grant_bytes(&token.payload)?;
        let verifier = self
            .verifiers
            .get(&token.payload.issuer_key_id)
            .ok_or(ControlError::UnknownGrantSigningKey)?;
        verifier
            .verify(&bytes, &signature)
            .map_err(|_| ControlError::InvalidGrantSignature)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandEnvelope {
    pub schema_version: u16,
    pub device_id: String,
    pub device_generation: u64,
    pub capability_revision: u64,
    pub operation_id: String,
    pub command: DeviceCommand,
}

impl CommandEnvelope {
    pub fn required_class(&self) -> CapabilityClass {
        self.command.class()
    }
}

pub fn validate_command_session(
    command: &CommandEnvelope,
    session: &DeviceSession,
) -> Result<(), ControlError> {
    if command.schema_version != CONTROL_SCHEMA_VERSION {
        return Err(ControlError::UnsupportedControlSchema {
            got: command.schema_version,
        });
    }
    if command.device_id != session.device_id {
        return Err(ControlError::CommandDeviceMismatch);
    }
    if command.device_generation != session.generation {
        return Err(ControlError::StaleDeviceGeneration {
            expected: session.generation,
            got: command.device_generation,
        });
    }
    if command.capability_revision != session.capabilities.revision {
        return Err(ControlError::StaleCapabilityRevision {
            expected: session.capabilities.revision,
            got: command.capability_revision,
        });
    }
    let required_capability = command.command.capability();
    if !session.capabilities.supports(required_capability) {
        return Err(ControlError::UnsupportedDeviceCapability(
            required_capability,
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceErrorCode {
    InvalidRequest,
    PermissionDenied,
    NotFound,
    IoFailure,
    InternalFailure,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DeviceResult {
    Applications {
        count: u64,
    },
    ScreenGeometry {
        width_points: u32,
        height_points: u32,
        scale_factor_milli: u32,
    },
    Screenshot {
        data_base64: String,
        mime_type: String,
        width_pixels: u32,
        height_pixels: u32,
    },
    PointerClickCompleted,
    PointerDragCompleted,
    TypeTextCompleted,
    ApplicationTerminated {
        process_id: u32,
    },
    WindowActivated {
        process_id: u32,
        window_id: Option<u64>,
        process_activated: bool,
        exact_window_verified: Option<bool>,
    },
    WindowFrameSet {
        process_id: u32,
        window_id: u64,
        bounds: UiRect,
    },
    MenuInvoked,
    KeyboardInputCompleted,
    ScrollCompleted,
    ClipboardState {
        types: Vec<String>,
        text: Option<String>,
    },
    ClipboardWritten {
        types: Vec<String>,
    },
    PointerPosition {
        x_points: i32,
        y_points: i32,
    },
    PointerMoveCompleted,
    UiValueSet,
    RegionCaptured {
        image: UiImage,
    },
    InteractionScopeExpanded,
    Process {
        output: ProcessOutput,
    },
    Shell {
        output: ProcessOutput,
    },
    FileContents {
        bytes: Vec<u8>,
        truncated: bool,
    },
    DirectoryEntries {
        entries: Vec<DirectoryEntry>,
        truncated: bool,
    },
    Windows {
        windows: Vec<WindowInfo>,
        truncated: bool,
    },
    ApplicationLaunched {
        process_id: u32,
        identifier: Option<String>,
        name: String,
        process_running: bool,
        window_ready: bool,
        windows: Vec<WindowInfo>,
        windows_truncated: bool,
    },
    WindowSnapshot {
        snapshot_ref: String,
        process_id: u32,
        window_id: u64,
        elements: Vec<UiElement>,
        elements_complete: bool,
        screenshot: Option<UiImage>,
    },
    UiStateVerification {
        status: VerificationStatus,
        stable: bool,
        samples: u32,
        predicates: Vec<UiPredicateResult>,
        screenshot: Option<UiImage>,
    },
    Error {
        code: DeviceErrorCode,
    },
}

impl DeviceResult {
    pub(crate) fn matches_command(&self, command: &DeviceCommand) -> bool {
        matches!(
            (self, command),
            (Self::Applications { .. }, DeviceCommand::ListApplications)
                | (Self::ScreenGeometry { .. }, DeviceCommand::ScreenGeometry)
                | (Self::Screenshot { .. }, DeviceCommand::Screenshot)
                | (
                    Self::Screenshot { .. },
                    DeviceCommand::ScreenshotContextual { .. }
                )
                | (
                    Self::PointerClickCompleted,
                    DeviceCommand::PointerClick { .. } | DeviceCommand::PointerClickAdvanced { .. }
                )
                | (
                    Self::PointerDragCompleted,
                    DeviceCommand::PointerDrag { .. } | DeviceCommand::PointerDragAdvanced { .. }
                )
                | (
                    Self::TypeTextCompleted,
                    DeviceCommand::TypeText { .. } | DeviceCommand::TypeTextAdvanced { .. }
                )
                | (Self::Process { .. }, DeviceCommand::ExecuteProcess { .. })
                | (Self::Shell { .. }, DeviceCommand::Shell { .. })
                | (Self::FileContents { .. }, DeviceCommand::ReadFile { .. })
                | (
                    Self::DirectoryEntries { .. },
                    DeviceCommand::ListDirectory { .. }
                )
                | (Self::Windows { .. }, DeviceCommand::ListWindows { .. })
                | (
                    Self::ApplicationLaunched { .. },
                    DeviceCommand::LaunchApplication { .. }
                )
                | (
                    Self::WindowSnapshot { .. },
                    DeviceCommand::InspectWindow { .. }
                )
                | (
                    Self::WindowSnapshot { .. },
                    DeviceCommand::InspectWindowContextual { .. }
                )
                | (
                    Self::UiStateVerification { .. },
                    DeviceCommand::VerifyUiState { .. }
                )
                | (
                    Self::UiStateVerification { .. },
                    DeviceCommand::VerifyUiStateContextual { .. }
                )
                | (
                    Self::ApplicationTerminated { .. },
                    DeviceCommand::TerminateApplication { .. }
                )
                | (
                    Self::WindowActivated { .. },
                    DeviceCommand::ActivateWindow { .. }
                )
                | (
                    Self::WindowFrameSet { .. },
                    DeviceCommand::SetWindowFrame { .. }
                )
                | (Self::MenuInvoked, DeviceCommand::InvokeMenu { .. })
                | (
                    Self::KeyboardInputCompleted,
                    DeviceCommand::KeyboardInput { .. }
                )
                | (Self::ScrollCompleted, DeviceCommand::Scroll { .. })
                | (
                    Self::ClipboardState { .. },
                    DeviceCommand::ClipboardRead { .. }
                )
                | (
                    Self::ClipboardWritten { .. },
                    DeviceCommand::ClipboardWrite { .. }
                )
                | (
                    Self::PointerPosition { .. },
                    DeviceCommand::PointerPosition { .. }
                )
                | (
                    Self::PointerMoveCompleted,
                    DeviceCommand::MovePointer { .. }
                )
                | (Self::UiValueSet, DeviceCommand::SetUiValue { .. })
                | (
                    Self::RegionCaptured { .. },
                    DeviceCommand::CaptureRegion { .. }
                )
                | (
                    Self::InteractionScopeExpanded,
                    DeviceCommand::ExpandInteractionScope { .. }
                )
                | (Self::Error { .. }, _)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandResultEnvelope {
    pub schema_version: u16,
    pub device_id: String,
    pub device_generation: u64,
    pub capability_revision: u64,
    pub operation_id: String,
    pub result: DeviceResult,
}

pub fn validate_command_result(
    command: &CommandEnvelope,
    result: &CommandResultEnvelope,
) -> Result<(), ControlError> {
    if result.schema_version != CONTROL_SCHEMA_VERSION {
        return Err(ControlError::UnsupportedControlSchema {
            got: result.schema_version,
        });
    }
    if result.device_id != command.device_id {
        return Err(ControlError::ResultDeviceMismatch);
    }
    if result.device_generation != command.device_generation {
        return Err(ControlError::ResultGenerationMismatch);
    }
    if result.capability_revision != command.capability_revision {
        return Err(ControlError::ResultCapabilityRevisionMismatch);
    }
    if result.operation_id != command.operation_id {
        return Err(ControlError::ResultOperationMismatch);
    }
    if !result.result.matches_command(&command.command) {
        return Err(ControlError::ResultTypeMismatch);
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationLease {
    pub device_id: String,
    pub operation_id: String,
    pub device_generation: u64,
    pub expires_at_ms: u64,
}

#[derive(Debug, Default)]
pub struct LeaseManager {
    leases: HashMap<String, OperationLease>,
}

impl LeaseManager {
    pub fn acquire(
        &mut self,
        device_id: &str,
        operation_id: &str,
        device_generation: u64,
        now_ms: u64,
        ttl_ms: u64,
    ) -> Result<OperationLease, ControlError> {
        if ttl_ms == 0 {
            return Err(ControlError::InvalidLeaseLifetime);
        }
        if let Some(existing) = self.leases.get(device_id) {
            if existing.expires_at_ms > now_ms {
                if existing.operation_id == operation_id
                    && existing.device_generation == device_generation
                {
                    return Ok(existing.clone());
                }
                return Err(ControlError::LeaseConflict {
                    owner_operation_id: existing.operation_id.clone(),
                    owner_generation: existing.device_generation,
                });
            }
        }
        let lease = OperationLease {
            device_id: device_id.to_owned(),
            operation_id: operation_id.to_owned(),
            device_generation,
            expires_at_ms: now_ms.saturating_add(ttl_ms),
        };
        self.leases.insert(device_id.to_owned(), lease.clone());
        Ok(lease)
    }

    pub fn release(
        &mut self,
        device_id: &str,
        operation_id: &str,
        device_generation: u64,
    ) -> Result<(), ControlError> {
        let existing = self
            .leases
            .get(device_id)
            .ok_or(ControlError::LeaseNotFound)?;
        if existing.operation_id != operation_id || existing.device_generation != device_generation
        {
            return Err(ControlError::LeaseOwnershipMismatch);
        }
        self.leases.remove(device_id);
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyOutcome {
    Allowed,
    Denied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditReason {
    GrantValidBackendCompleted,
    ObserveGrantCannotAuthorizeInteract,
    GrantReplayRejected,
    GrantRevokedRejected,
    GrantExpiredRejected,
    LeaseConflictAfterReconnect,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEvidence {
    pub event_id: String,
    pub occurred_at_ms: u64,
    pub device_id: String,
    pub device_generation: u64,
    pub grant_id: Option<String>,
    pub operation_id: Option<String>,
    pub capability: Option<CapabilityClass>,
    pub outcome: PolicyOutcome,
    pub reason: AuditReason,
}

#[derive(Debug, Default)]
pub struct AuditLog {
    events: Vec<AuditEvidence>,
}

impl AuditLog {
    pub fn record(&mut self, mut evidence: AuditEvidence) {
        if evidence.event_id.is_empty() {
            let mut random = [0_u8; 12];
            OsRng.fill_bytes(&mut random);
            evidence.event_id = format!("audit_{}", hex(&random));
        }
        self.events.push(evidence);
    }

    pub fn events(&self) -> &[AuditEvidence] {
        &self.events
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlError {
    InvalidDeviceIdentity,
    InvalidEnrollmentProof,
    InvalidDeviceSignature,
    InvalidDeviceRotationProof,
    InvalidRegistrySnapshot,
    UnknownDevice,
    DeviceRevoked,
    DeviceOffline,
    UnsupportedCapabilitySchema {
        got: u16,
    },
    UnsupportedControlSchema {
        got: u16,
    },
    InvalidGrantLifetime,
    InvalidGrantSignature,
    UnknownGrantSigningKey,
    InvalidGrantLedgerSnapshot,
    GrantDeviceMismatch,
    GrantNotYetValid,
    GrantExpired,
    GrantRevoked,
    GrantReplay,
    CapabilityDenied {
        granted: CapabilityClass,
        required: CapabilityClass,
    },
    DeviceCapabilityGrantMismatch {
        granted: Option<DeviceCapability>,
        required: DeviceCapability,
    },
    CommandDeviceMismatch,
    StaleDeviceGeneration {
        expected: u64,
        got: u64,
    },
    StaleCapabilityRevision {
        expected: u64,
        got: u64,
    },
    UnsupportedDeviceCapability(DeviceCapability),
    ResultDeviceMismatch,
    ResultGenerationMismatch,
    ResultCapabilityRevisionMismatch,
    ResultOperationMismatch,
    ResultTypeMismatch,
    InvalidLeaseLifetime,
    LeaseConflict {
        owner_operation_id: String,
        owner_generation: u64,
    },
    LeaseNotFound,
    LeaseOwnershipMismatch,
    Serialization,
}

impl fmt::Display for ControlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for ControlError {}

fn canonical_grant_bytes(payload: &GrantPayload) -> Result<Vec<u8>, ControlError> {
    serde_json::to_vec(payload).map_err(|_| ControlError::Serialization)
}

pub fn verifying_key_id(verifier: &VerifyingKey) -> String {
    format!("key_{}", hex(&verifier.to_bytes()))
}

fn hex(bytes: &[u8]) -> String {
    const TABLE: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(TABLE[(byte >> 4) as usize] as char);
        output.push(TABLE[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capabilities(revision: u64) -> CapabilityAdvertisement {
        CapabilityAdvertisement {
            backend: "cua".into(),
            backend_version: "0.19.3".into(),
            platform: "darwin-arm64".into(),
            capability_schema_version: CAPABILITY_SCHEMA_VERSION,
            revision,
            supported: vec![
                DeviceCapability::ListApplications,
                DeviceCapability::ScreenGeometry,
                DeviceCapability::PointerClick,
            ],
        }
    }

    fn enrolled() -> (DeviceRegistry, DeviceIdentity, String) {
        let identity = DeviceIdentity::generate();
        let challenge = DeviceRegistry::enrollment_challenge();
        let proof = identity.enrollment_proof(&challenge);
        let mut registry = DeviceRegistry::default();
        let device_id = registry
            .enroll(&identity.public_key(), &challenge, &proof)
            .unwrap();
        (registry, identity, device_id)
    }

    #[test]
    fn enrollment_requires_proof_of_private_key() {
        let identity = DeviceIdentity::generate();
        let attacker = DeviceIdentity::generate();
        let challenge = DeviceRegistry::enrollment_challenge();
        let bad_proof = attacker.enrollment_proof(&challenge);
        let mut registry = DeviceRegistry::default();
        assert_eq!(
            registry.enroll(&identity.public_key(), &challenge, &bad_proof),
            Err(ControlError::InvalidEnrollmentProof)
        );
    }

    #[test]
    fn observe_grant_cannot_authorize_interact_and_is_one_shot() {
        let (mut registry, _identity, device_id) = enrolled();
        let session = registry.connect(&device_id, capabilities(7)).unwrap();
        let authority = GrantAuthority::generate();
        let mut ledger = GrantLedger::new(authority.verifier());
        let token = authority
            .issue(&device_id, CapabilityClass::Observe, 1_000, 30_000)
            .unwrap();

        let interact = CommandEnvelope {
            schema_version: CONTROL_SCHEMA_VERSION,
            device_id: device_id.clone(),
            device_generation: session.generation,
            capability_revision: 7,
            operation_id: "op-interact".into(),
            command: DeviceCommand::PointerClick {
                x: 10,
                y: 20,
                button: PointerButton::Left,
            },
        };
        validate_command_session(&interact, &session).unwrap();
        assert!(matches!(
            ledger.authorize_once(&token, &device_id, interact.required_class(), 1_001),
            Err(ControlError::CapabilityDenied { .. })
        ));

        let observe = CommandEnvelope {
            command: DeviceCommand::ListApplications,
            operation_id: "op-observe".into(),
            ..interact
        };
        ledger
            .authorize_once(&token, &device_id, observe.required_class(), 1_002)
            .unwrap();
        assert_eq!(
            ledger.authorize_once(&token, &device_id, observe.required_class(), 1_003),
            Err(ControlError::GrantReplay)
        );
    }

    #[test]
    fn unknown_grant_signing_key_and_signature_tampering_are_rejected() {
        let (_registry, _identity, device_id) = enrolled();
        let authority = GrantAuthority::generate();
        let attacker = GrantAuthority::generate();
        let mut ledger = GrantLedger::new(authority.verifier());
        let forged = attacker
            .issue(&device_id, CapabilityClass::Observe, 1_000, 30_000)
            .unwrap();
        assert_eq!(
            ledger.authorize_once(&forged, &device_id, CapabilityClass::Observe, 1_001),
            Err(ControlError::UnknownGrantSigningKey)
        );

        let mut tampered = authority
            .issue(&device_id, CapabilityClass::Observe, 1_000, 30_000)
            .unwrap();
        tampered.signature[0] ^= 0x01;
        assert_eq!(
            ledger.authorize_once(&tampered, &device_id, CapabilityClass::Observe, 1_001),
            Err(ControlError::InvalidGrantSignature)
        );
    }

    #[test]
    fn grant_lifetime_is_bounded_and_expired_consumption_tombstones_are_pruned() {
        let authority = GrantAuthority::generate();
        assert_eq!(
            authority.issue(
                "dev",
                CapabilityClass::Observe,
                1_000,
                MAX_GRANT_LIFETIME_MS + 1,
            ),
            Err(ControlError::InvalidGrantLifetime)
        );

        let mut ledger = GrantLedger::new(authority.verifier());
        let first = authority
            .issue("dev", CapabilityClass::Observe, 1_000, 10)
            .unwrap();
        ledger
            .authorize_once(&first, "dev", CapabilityClass::Observe, 1_001)
            .unwrap();
        assert_eq!(ledger.snapshot().consumed_grants.len(), 1);

        let second = authority
            .issue("dev", CapabilityClass::Observe, 1_020, 10)
            .unwrap();
        ledger
            .authorize_once(&second, "dev", CapabilityClass::Observe, 1_020)
            .unwrap();
        let snapshot = ledger.snapshot();
        assert_eq!(snapshot.consumed_grants.len(), 1);
        assert_eq!(
            snapshot.consumed_grants[0].grant_id,
            second.payload.grant_id
        );
    }

    #[test]
    fn screenshot_is_observe_and_type_text_is_interact() {
        assert_eq!(
            DeviceCapability::Screenshot.class(),
            CapabilityClass::Observe
        );
        assert_eq!(
            DeviceCapability::TypeText.class(),
            CapabilityClass::Interact
        );
        assert_eq!(
            DeviceCommand::Screenshot.capability(),
            DeviceCapability::Screenshot
        );
        assert_eq!(
            DeviceCommand::TypeText { text: "x".into() }.capability(),
            DeviceCapability::TypeText
        );
    }

    #[test]
    fn exact_screenshot_grant_does_not_authorize_type_text() {
        let authority = GrantAuthority::generate();
        let mut ledger = GrantLedger::new(authority.verifier());
        let screenshot = authority
            .issue_for_device_capability("dev", DeviceCapability::Screenshot, 1_000, 30_000)
            .unwrap();
        assert!(matches!(
            ledger.authorize_device_capability_once(
                &screenshot,
                "dev",
                DeviceCapability::ScreenGeometry,
                1_001,
            ),
            Err(ControlError::DeviceCapabilityGrantMismatch { .. })
        ));
        assert!(matches!(
            ledger.authorize_device_capability_once(
                &screenshot,
                "dev",
                DeviceCapability::TypeText,
                1_001,
            ),
            Err(ControlError::CapabilityDenied { .. })
        ));
    }

    #[test]
    fn exact_device_capability_grants_do_not_accept_class_only_tokens() {
        let authority = GrantAuthority::generate();
        let mut ledger = GrantLedger::new(authority.verifier());
        let class_only = authority
            .issue("dev", CapabilityClass::Dangerous, 1_000, 30_000)
            .unwrap();
        assert!(matches!(
            ledger.authorize_device_capability_once(
                &class_only,
                "dev",
                DeviceCapability::ExecuteProcess,
                1_001
            ),
            Err(ControlError::DeviceCapabilityGrantMismatch { .. })
        ));

        let exact = authority
            .issue_for_device_capability("dev", DeviceCapability::ExecuteProcess, 1_000, 30_000)
            .unwrap();
        ledger
            .authorize_device_capability_once(
                &exact,
                "dev",
                DeviceCapability::ExecuteProcess,
                1_001,
            )
            .unwrap();

        let process_for_shell = authority
            .issue_for_device_capability("dev", DeviceCapability::ExecuteProcess, 1_000, 30_000)
            .unwrap();
        assert!(matches!(
            ledger.authorize_device_capability_once(
                &process_for_shell,
                "dev",
                DeviceCapability::Shell,
                1_001,
            ),
            Err(ControlError::DeviceCapabilityGrantMismatch { .. })
        ));
    }

    #[test]
    fn expired_and_revoked_grants_fail_closed() {
        let (_registry, _identity, device_id) = enrolled();
        let authority = GrantAuthority::generate();
        let mut ledger = GrantLedger::new(authority.verifier());
        let expired = authority
            .issue(&device_id, CapabilityClass::Observe, 10, 5)
            .unwrap();
        assert_eq!(
            ledger.authorize_once(&expired, &device_id, CapabilityClass::Observe, 15),
            Err(ControlError::GrantExpired)
        );

        let revoked = authority
            .issue(&device_id, CapabilityClass::Observe, 20, 10_000)
            .unwrap();
        ledger.revoke(&revoked.payload.grant_id);
        assert_eq!(
            ledger.authorize_once(&revoked, &device_id, CapabilityClass::Observe, 21),
            Err(ControlError::GrantRevoked)
        );
    }

    #[test]
    fn reconnect_does_not_transfer_an_in_flight_lease() {
        let (mut registry, _identity, device_id) = enrolled();
        let first = registry.connect(&device_id, capabilities(1)).unwrap();
        let mut leases = LeaseManager::default();
        leases
            .acquire(&device_id, "op-1", first.generation, 1_000, 10_000)
            .unwrap();

        let second = registry.connect(&device_id, capabilities(1)).unwrap();
        assert!(second.generation > first.generation);
        assert!(matches!(
            leases.acquire(&device_id, "op-2", second.generation, 1_001, 10_000),
            Err(ControlError::LeaseConflict { owner_generation, .. }) if owner_generation == first.generation
        ));
        assert_eq!(
            leases.release(&device_id, "op-1", second.generation),
            Err(ControlError::LeaseOwnershipMismatch)
        );

        leases
            .acquire(&device_id, "op-2", second.generation, 11_001, 10_000)
            .unwrap();
    }

    #[test]
    fn pre_v3_control_and_capability_schemas_fail_closed_during_rolling_upgrade() {
        let (mut registry, _identity, device_id) = enrolled();
        let mut old_capabilities = capabilities(8);
        old_capabilities.capability_schema_version = 2;
        assert_eq!(
            registry.connect(&device_id, old_capabilities),
            Err(ControlError::UnsupportedCapabilitySchema { got: 2 })
        );

        let session = registry.connect(&device_id, capabilities(9)).unwrap();
        let old_command = CommandEnvelope {
            schema_version: 2,
            device_id,
            device_generation: session.generation,
            capability_revision: session.capabilities.revision,
            operation_id: "op-old-schema".into(),
            command: DeviceCommand::ListApplications,
        };
        assert_eq!(
            validate_command_session(&old_command, &session),
            Err(ControlError::UnsupportedControlSchema { got: 2 })
        );
    }

    #[test]
    fn stale_generation_and_capability_revision_are_rejected() {
        let (mut registry, _identity, device_id) = enrolled();
        let first = registry.connect(&device_id, capabilities(9)).unwrap();
        let second = registry.connect(&device_id, capabilities(10)).unwrap();

        let stale_generation = CommandEnvelope {
            schema_version: CONTROL_SCHEMA_VERSION,
            device_id: device_id.clone(),
            device_generation: first.generation,
            capability_revision: second.capabilities.revision,
            operation_id: "op-stale-generation".into(),
            command: DeviceCommand::ListApplications,
        };
        assert!(matches!(
            validate_command_session(&stale_generation, &second),
            Err(ControlError::StaleDeviceGeneration { .. })
        ));

        let stale_revision = CommandEnvelope {
            device_generation: second.generation,
            capability_revision: 9,
            operation_id: "op-stale-revision".into(),
            ..stale_generation
        };
        assert!(matches!(
            validate_command_session(&stale_revision, &second),
            Err(ControlError::StaleCapabilityRevision { .. })
        ));
    }

    #[test]
    fn contextual_observation_results_match_their_contextual_commands() {
        let context_id = "ctx_0123456789abcdef0123456789abcdef".to_owned();
        assert!(
            DeviceResult::Screenshot {
                data_base64: "AA==".into(),
                mime_type: "image/png".into(),
                width_pixels: 1,
                height_pixels: 1,
            }
            .matches_command(&DeviceCommand::ScreenshotContextual {
                context_id: context_id.clone(),
            })
        );
        assert!(
            DeviceResult::WindowSnapshot {
                snapshot_ref: "s".into(),
                process_id: 1,
                window_id: 2,
                elements: vec![],
                elements_complete: true,
                screenshot: None,
            }
            .matches_command(&DeviceCommand::InspectWindowContextual {
                context_id: context_id.clone(),
                process_id: 1,
                window_id: 2,
                query: None,
                max_elements: 1,
                max_depth: 1,
                include_screenshot: false,
            })
        );
        assert!(
            DeviceResult::UiStateVerification {
                status: VerificationStatus::Satisfied,
                stable: true,
                samples: 1,
                predicates: vec![],
                screenshot: None,
            }
            .matches_command(&DeviceCommand::VerifyUiStateContextual {
                context_id,
                process_id: 1,
                window_id: 2,
                predicates: vec![],
                timeout_ms: 0,
                stable_samples: 1,
                include_screenshot: false,
            })
        );
    }

    #[test]
    fn desktop_parity_results_match_only_their_exact_new_commands() {
        let context_id = "ctx_0123456789abcdef0123456789abcdef".to_owned();
        assert!(
            DeviceResult::UiValueSet.matches_command(&DeviceCommand::SetUiValue {
                context_id: context_id.clone(),
                process_id: 1,
                window_id: 2,
                element_ref: "ref_0123456789abcdef0123456789abcdef".into(),
                value: "x".into(),
            })
        );
        assert!(
            DeviceResult::RegionCaptured {
                image: UiImage {
                    data_base64: "/9j/2Q==".into(),
                    mime_type: "image/jpeg".into(),
                    width_pixels: 1,
                    height_pixels: 1,
                },
            }
            .matches_command(&DeviceCommand::CaptureRegion {
                context_id: Some(context_id.clone()),
                process_id: 1,
                window_id: 2,
                bounds: UiRect {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                },
            })
        );
        assert!(DeviceResult::InteractionScopeExpanded.matches_command(
            &DeviceCommand::ExpandInteractionScope {
                context_id,
                reason: "test".into(),
            }
        ));
        assert!(!DeviceResult::InteractionScopeExpanded.matches_command(
            &DeviceCommand::SetUiValue {
                context_id: "ctx_0123456789abcdef0123456789abcdef".into(),
                process_id: 1,
                window_id: 2,
                element_ref: "ref_0123456789abcdef0123456789abcdef".into(),
                value: "x".into(),
            }
        ));
    }

    #[test]
    fn typed_results_must_match_the_command_envelope() {
        let command = CommandEnvelope {
            schema_version: CONTROL_SCHEMA_VERSION,
            device_id: "dev-test".into(),
            device_generation: 3,
            capability_revision: 9,
            operation_id: "op-test".into(),
            command: DeviceCommand::ListApplications,
        };
        let result = CommandResultEnvelope {
            schema_version: CONTROL_SCHEMA_VERSION,
            device_id: command.device_id.clone(),
            device_generation: command.device_generation,
            capability_revision: command.capability_revision,
            operation_id: command.operation_id.clone(),
            result: DeviceResult::Applications { count: 7 },
        };
        validate_command_result(&command, &result).unwrap();

        let mismatched = CommandResultEnvelope {
            result: DeviceResult::PointerClickCompleted,
            ..result
        };
        assert_eq!(
            validate_command_result(&command, &mismatched),
            Err(ControlError::ResultTypeMismatch)
        );
    }

    #[test]
    fn registry_snapshot_round_trip_preserves_generation_capabilities_and_revocation() {
        let (mut registry, _identity, device_id) = enrolled();
        registry.connect(&device_id, capabilities(5)).unwrap();
        let other = DeviceIdentity::generate();
        let challenge = DeviceRegistry::enrollment_challenge();
        let proof = other.enrollment_proof(&challenge);
        let other_id = registry
            .enroll(&other.public_key(), &challenge, &proof)
            .unwrap();
        registry.revoke_device(&other_id);
        let restored = DeviceRegistry::from_snapshot(registry.snapshot()).unwrap();
        assert_eq!(restored.current_session(&device_id).unwrap().generation, 1);
        assert_eq!(
            restored
                .current_session(&device_id)
                .unwrap()
                .capabilities
                .revision,
            5
        );
        assert_eq!(
            restored.verify_device_signature(&other_id, b"x", &other.sign_message(b"x")),
            Err(ControlError::DeviceRevoked)
        );
    }

    #[test]
    fn grant_ledger_snapshot_preserves_consumed_and_revoked_replay_state() {
        let authority = GrantAuthority::generate();
        let mut ledger = GrantLedger::new(authority.verifier());
        let consumed = authority
            .issue("dev", CapabilityClass::Observe, 10, 100)
            .unwrap();
        ledger
            .authorize_once(&consumed, "dev", CapabilityClass::Observe, 11)
            .unwrap();
        let revoked = authority
            .issue("dev", CapabilityClass::Observe, 10, 100)
            .unwrap();
        ledger.revoke(&revoked.payload.grant_id);
        let mut restored = GrantLedger::from_snapshot(ledger.snapshot()).unwrap();
        assert_eq!(
            restored.authorize_once(&consumed, "dev", CapabilityClass::Observe, 12),
            Err(ControlError::GrantReplay)
        );
        assert_eq!(
            restored.authorize_once(&revoked, "dev", CapabilityClass::Observe, 12),
            Err(ControlError::GrantRevoked)
        );
    }

    #[test]
    fn audit_evidence_has_no_raw_command_or_result_fields() {
        let mut log = AuditLog::default();
        log.record(AuditEvidence {
            event_id: String::new(),
            occurred_at_ms: 1,
            device_id: "dev-test".into(),
            device_generation: 2,
            grant_id: Some("grant-test".into()),
            operation_id: Some("op-test".into()),
            capability: Some(CapabilityClass::Observe),
            outcome: PolicyOutcome::Allowed,
            reason: AuditReason::GrantValidBackendCompleted,
        });
        let value = serde_json::to_value(&log.events()[0]).unwrap();
        assert!(value.get("arguments").is_none());
        assert!(value.get("result").is_none());
        assert!(value.get("screenshot").is_none());
    }
}
