//! Resolved browser command/result types for the signed Hub-Agent transport.
//!
//! Northbound request types contain CUMG `ref_...` capabilities. These types
//! are constructed only after Hub-side context/ref validation has resolved
//! those capabilities to backend-private handles. They never form the public
//! MCP schema and their Debug output redacts backend handles and user text.

use crate::v2_browser::{
    BrowserAction, BrowserDialogAction, BrowserDialogDelivery, BrowserInputRoute,
    BrowserPointerAction, BrowserPrepareProfileMode, BrowserTypeMode,
};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserRuntimeCapability {
    Inspect,
    Prepare,
    Navigate,
    Click,
    Type,
    Dialog,
    Pointer,
    UploadFile,
    Download,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BrowserBackendCommand {
    Prepare {
        context_id: String,
        process_id: u32,
        window_id: Option<u64>,
        allow_launch: bool,
        profile_mode: BrowserPrepareProfileMode,
        profile_name: Option<String>,
    },
    Bind {
        context_id: String,
        process_id: u32,
        window_id: u64,
    },
    Inspect {
        context_id: String,
        backend_target_id: String,
        backend_tab_id: String,
        backend_scope_ref: Option<String>,
        query: Option<String>,
        backend_continuation: Option<String>,
        include_screenshot: bool,
    },
    Navigate {
        context_id: String,
        backend_target_id: String,
        backend_tab_id: String,
        url: String,
    },
    Click {
        context_id: String,
        backend_target_id: String,
        backend_tab_id: String,
        target: BrowserBackendClickTarget,
        input_route: BrowserInputRoute,
    },
    Type {
        context_id: String,
        backend_target_id: String,
        backend_tab_id: String,
        backend_element_ref: String,
        text: String,
        mode: BrowserTypeMode,
        replace: bool,
    },
    Dialog {
        context_id: String,
        backend_target_id: String,
        backend_tab_id: String,
        backend_dialog_id: String,
        action: BrowserDialogAction,
        prompt_text: Option<String>,
        delivery: BrowserDialogDelivery,
    },
    Pointer {
        context_id: String,
        backend_target_id: String,
        backend_tab_id: String,
        backend_element_ref: String,
        action: BrowserPointerAction,
        backend_destination_ref: Option<String>,
        delta_x: i32,
        delta_y: i32,
        input_route: BrowserInputRoute,
    },
    Upload {
        context_id: String,
        backend_target_id: String,
        backend_tab_id: String,
        backend_element_ref: String,
        staged_files: Vec<BrowserStagedUploadFile>,
    },
    Download {
        context_id: String,
        backend_target_id: String,
        backend_tab_id: String,
        backend_element_ref: String,
        staging_root: String,
        max_bytes: u64,
        overwrite: bool,
    },
}

impl BrowserBackendCommand {
    pub fn capability(&self) -> BrowserRuntimeCapability {
        match self {
            Self::Bind { .. } | Self::Inspect { .. } => BrowserRuntimeCapability::Inspect,
            Self::Prepare { .. } => BrowserRuntimeCapability::Prepare,
            Self::Navigate { .. } => BrowserRuntimeCapability::Navigate,
            Self::Click { .. } => BrowserRuntimeCapability::Click,
            Self::Type { .. } => BrowserRuntimeCapability::Type,
            Self::Dialog { .. } => BrowserRuntimeCapability::Dialog,
            Self::Pointer { .. } => BrowserRuntimeCapability::Pointer,
            Self::Upload { .. } => BrowserRuntimeCapability::UploadFile,
            Self::Download { .. } => BrowserRuntimeCapability::Download,
        }
    }

    pub fn context_id(&self) -> &str {
        match self {
            Self::Prepare { context_id, .. }
            | Self::Bind { context_id, .. }
            | Self::Inspect { context_id, .. }
            | Self::Navigate { context_id, .. }
            | Self::Click { context_id, .. }
            | Self::Type { context_id, .. }
            | Self::Dialog { context_id, .. }
            | Self::Pointer { context_id, .. }
            | Self::Upload { context_id, .. }
            | Self::Download { context_id, .. } => context_id,
        }
    }
}

impl fmt::Debug for BrowserBackendCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BrowserBackendCommand")
            .field("capability", &self.capability())
            .field("context_id", &"[redacted]")
            .field("backend_material", &"[redacted]")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BrowserBackendClickTarget {
    Element { backend_element_ref: String },
    ViewportCss { x: i32, y: i32 },
}

impl fmt::Debug for BrowserBackendClickTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Element { .. } => f
                .debug_struct("Element")
                .field("backend_element_ref", &"[redacted]")
                .finish(),
            Self::ViewportCss { x, y } => f
                .debug_struct("ViewportCss")
                .field("x", x)
                .field("y", y)
                .finish(),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserStagedUploadFile {
    pub path: String,
    pub expected_bytes: u64,
}

impl fmt::Debug for BrowserStagedUploadFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BrowserStagedUploadFile")
            .field("path", &"[redacted]")
            .field("expected_bytes", &self.expected_bytes)
            .finish()
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BrowserBackendResult {
    Prepared {
        prepared: bool,
        prepared_process_id: Option<u32>,
        side_effect_count: u32,
    },
    Bound {
        backend_target_id: String,
        process_id: u32,
        window_id: u64,
        tabs: Vec<BrowserBackendTab>,
    },
    Snapshot {
        backend_snapshot_id: String,
        outline: String,
        action_refs: Vec<BrowserBackendSemanticRef>,
        content_refs: Vec<BrowserBackendSemanticRef>,
        complete: bool,
        omitted: u32,
        backend_continuation: Option<String>,
        screenshot: Option<BrowserBackendScreenshot>,
    },
    NavigationCompleted,
    ClickCompleted {
        effect: BrowserMutationEffect,
    },
    TypeCompleted,
    DialogCompleted,
    PointerCompleted,
    UploadAssigned {
        file_count: u32,
    },
    DownloadStaged {
        backend_download_id: String,
        bytes_written: u64,
    },
}

impl BrowserBackendResult {
    pub fn matches_command(&self, command: &BrowserBackendCommand) -> bool {
        matches!(
            (self, command),
            (
                Self::Prepared { .. },
                BrowserBackendCommand::Prepare { .. }
            ) | (Self::Bound { .. }, BrowserBackendCommand::Bind { .. })
                | (Self::Snapshot { .. }, BrowserBackendCommand::Inspect { .. })
                | (
                    Self::NavigationCompleted,
                    BrowserBackendCommand::Navigate { .. }
                )
                | (
                    Self::ClickCompleted { .. },
                    BrowserBackendCommand::Click { .. }
                )
                | (Self::TypeCompleted, BrowserBackendCommand::Type { .. })
                | (
                    Self::DialogCompleted,
                    BrowserBackendCommand::Dialog { .. }
                )
                | (
                    Self::PointerCompleted,
                    BrowserBackendCommand::Pointer { .. }
                )
                | (
                    Self::UploadAssigned { .. },
                    BrowserBackendCommand::Upload { .. }
                )
                | (
                    Self::DownloadStaged { .. },
                    BrowserBackendCommand::Download { .. }
                )
        )
    }
}

impl fmt::Debug for BrowserBackendResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self {
            Self::Prepared { .. } => "prepared",
            Self::Bound { .. } => "bound",
            Self::Snapshot { .. } => "snapshot",
            Self::NavigationCompleted => "navigation_completed",
            Self::ClickCompleted { .. } => "click_completed",
            Self::TypeCompleted => "type_completed",
            Self::DialogCompleted => "dialog_completed",
            Self::PointerCompleted => "pointer_completed",
            Self::UploadAssigned { .. } => "upload_assigned",
            Self::DownloadStaged { .. } => "download_staged",
        };
        f.debug_struct("BrowserBackendResult")
            .field("kind", &kind)
            .field("backend_material", &"[redacted]")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserBackendTab {
    pub backend_tab_id: String,
    pub title: Option<String>,
    pub url: Option<String>,
    pub active: Option<bool>,
}

impl fmt::Debug for BrowserBackendTab {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BrowserBackendTab")
            .field("backend_tab_id", &"[redacted]")
            .field("title", &self.title)
            .field("url", &self.url.as_ref().map(|_| "[redacted page url]"))
            .field("active", &self.active)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserBackendSemanticRef {
    pub backend_ref: String,
    pub role: String,
    pub name: Option<String>,
    pub value: Option<String>,
    pub states: Vec<String>,
    pub actions: Vec<BrowserAction>,
    pub frame: String,
    pub visibility: String,
}

impl fmt::Debug for BrowserBackendSemanticRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BrowserBackendSemanticRef")
            .field("backend_ref", &"[redacted]")
            .field("role", &self.role)
            .field("name", &self.name)
            .field("value", &self.value.as_ref().map(|_| "[redacted value]"))
            .field("states", &self.states)
            .field("actions", &self.actions)
            .field("frame", &self.frame)
            .field("visibility", &self.visibility)
            .finish()
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct BrowserBackendScreenshot {
    pub data_base64: String,
    pub mime_type: String,
    pub width_pixels: u32,
    pub height_pixels: u32,
    pub viewport_css_width: u32,
    pub viewport_css_height: u32,
    pub pixel_to_css_scale_x: f64,
    pub pixel_to_css_scale_y: f64,
}

impl fmt::Debug for BrowserBackendScreenshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BrowserBackendScreenshot")
            .field("data_base64", &"[redacted image]")
            .field("mime_type", &self.mime_type)
            .field("width_pixels", &self.width_pixels)
            .field("height_pixels", &self.height_pixels)
            .field("viewport_css_width", &self.viewport_css_width)
            .field("viewport_css_height", &self.viewport_css_height)
            .field("pixel_to_css_scale_x", &self.pixel_to_css_scale_x)
            .field("pixel_to_css_scale_y", &self.pixel_to_css_scale_y)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserMutationEffect {
    Dispatched,
    Unverifiable,
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONTEXT: &str = "ctx_0123456789abcdef0123456789abcdef";

    #[test]
    fn command_capability_is_exact_and_context_is_not_authority() {
        let command = BrowserBackendCommand::Navigate {
            context_id: CONTEXT.into(),
            backend_target_id: "target-secret".into(),
            backend_tab_id: "tab-secret".into(),
            url: "https://example.com/private".into(),
        };
        assert_eq!(command.capability(), BrowserRuntimeCapability::Navigate);
        assert_eq!(command.context_id(), CONTEXT);
        let debug = format!("{command:?}");
        assert!(!debug.contains(CONTEXT));
        assert!(!debug.contains("target-secret"));
        assert!(!debug.contains("tab-secret"));
        assert!(!debug.contains("example.com/private"));
    }

    #[test]
    fn result_type_must_match_the_exact_browser_command() {
        let command = BrowserBackendCommand::Click {
            context_id: CONTEXT.into(),
            backend_target_id: "target".into(),
            backend_tab_id: "tab".into(),
            target: BrowserBackendClickTarget::Element {
                backend_element_ref: "p9:7".into(),
            },
            input_route: BrowserInputRoute::DomEvent,
        };
        assert!(BrowserBackendResult::ClickCompleted {
            effect: BrowserMutationEffect::Unverifiable,
        }
        .matches_command(&command));
        assert!(!BrowserBackendResult::TypeCompleted.matches_command(&command));
    }

    #[test]
    fn transport_debug_redacts_backend_refs_paths_text_urls_and_images() {
        let click_target = BrowserBackendClickTarget::Element {
            backend_element_ref: "p9:7".into(),
        };
        assert!(!format!("{click_target:?}").contains("p9:7"));

        let upload = BrowserBackendCommand::Upload {
            context_id: CONTEXT.into(),
            backend_target_id: "target-secret".into(),
            backend_tab_id: "tab-secret".into(),
            backend_element_ref: "p9:7".into(),
            staged_files: vec![BrowserStagedUploadFile {
                path: "/tmp/cumg/secret.txt".into(),
                expected_bytes: 42,
            }],
        };
        let debug = format!("{upload:?}");
        assert!(!debug.contains("secret.txt"));
        assert!(!debug.contains("p9:7"));

        let screenshot = BrowserBackendScreenshot {
            data_base64: "super-secret-image".into(),
            mime_type: "image/png".into(),
            width_pixels: 100,
            height_pixels: 80,
            viewport_css_width: 100,
            viewport_css_height: 80,
            pixel_to_css_scale_x: 1.0,
            pixel_to_css_scale_y: 1.0,
        };
        assert!(!format!("{screenshot:?}").contains("super-secret-image"));
    }
}
