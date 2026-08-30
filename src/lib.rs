pub mod backend;
pub mod config;
pub mod gateway;
pub mod mutation_authority;
pub mod policy;
pub mod v2_agent_handoff;
pub mod v2_browser;
pub mod v2_browser_execute;
pub mod v2_browser_normalize;
pub mod v2_browser_refs;
pub mod v2_browser_runtime;
pub mod v2_browser_staging;
pub mod v2_m0;
pub mod v2_m0_backend;
pub mod v2_m0_execution;
pub mod v2_m0_transport;
pub mod v2_m0_trust;
pub mod v2_m1;
pub mod v2_m1_agent;
pub mod v2_m1_backend;
pub mod v2_m1_keys;
pub mod v2_m1_persistence;
pub mod v2_m1_process;
pub mod v2_m1_shell;
pub mod v2_m1_tls;

// Deployment-layer quota and billing controls intentionally have no core module here.
pub mod v2_m1_filesystem;
pub mod v2_m1_grpc;
pub mod v2_m1_hub;
pub mod v2_m1_northbound;

pub mod v2_enrollment;
pub mod v2_execution_safety;
pub mod v2_grant_signer;
pub mod v2_handoff_control;
pub mod v2_handoff_coordinator;
pub mod v2_interaction_context;
pub mod v2_limits;
pub mod v2_maintenance;
pub mod v2_multi_device;
pub mod v2_observability;
pub mod v2_online_recovery;
pub mod v2_operator_handoff;
pub mod v2_reference_backend;
pub mod v2_state_lock;
pub(crate) mod v2_terminal_pty;
#[cfg(all(test, unix))]
mod v2_terminal_pty_acceptance;
pub(crate) mod v2_terminal_pty_handoff;
pub mod v2_tls_lifecycle;

#[cfg(windows)]
mod v2_windows_acl;

pub mod v2_doctor;
