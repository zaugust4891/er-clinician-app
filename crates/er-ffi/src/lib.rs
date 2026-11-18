//! Durable local phone boundary. Network, hardware capture and generation are later steps.
//!
//! Every handwritten export enters `ffi_guard`. Step 2 must additionally fault
//! stateful handles on panic and require close/reopen/replay; catching unwind is
//! not permission to reuse partially mutated state or recover poisoned locks.

use std::panic::{UnwindSafe, catch_unwind};
use std::sync::Once;

// Also catches panic=abort supplied through Cargo config or environment flags.
#[cfg(not(panic = "unwind"))]
compile_error!("er-ffi requires panic=unwind, including optimized Apple builds");

uniffi::setup_scaffolding!();

/// Stable categories only: never carry arbitrary implementation error strings.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, uniffi::Error)]
pub enum FfiError {
    #[error("ER_INVALID_INPUT")]
    InvalidInput,
    #[error("ER_INVALID_STATE")]
    InvalidState,
    #[error("ER_NOT_FOUND")]
    NotFound,
    #[error("ER_CONFLICT")]
    Conflict,
    #[error("ER_SCOPE")]
    Scope,
    #[error("ER_RECOVERY")]
    Recovery,
    #[error("ER_STORAGE")]
    Storage,
    #[error("ER_CLOSED")]
    Closed,
    #[error("ER_BUSY")]
    Busy,
    #[error("ER_INTERNAL")]
    Internal,
}

/// Local configuration only. Passing this record does not open/create a store.
/// The phone supplies its protected Application Support path. Scope is opaque;
/// authentication and durable scope binding arrive with the device core.
#[derive(Clone, PartialEq, Eq, uniffi::Record)]
pub struct DeviceConfig {
    pub database_path: String,
    pub account_scope: String,
}

fn ffi_guard<T>(
    operation: impl FnOnce() -> Result<T, FfiError> + UnwindSafe,
) -> Result<T, FfiError> {
    static INSTALL_HOOK: Once = Once::new();
    INSTALL_HOOK.call_once(|| {
        // This crate is the phone composition boundary, not a shared backend
        // library. Do not chain the previous hook: it can print PHI/backtraces.
        // No output also avoids a second panic if a diagnostic sink fails.
        std::panic::set_hook(Box::new(|_| {}));
    });
    match catch_unwind(operation) {
        Ok(result) => result,
        Err(payload) => {
            // An arbitrary panic payload can itself panic on Drop. Do not let
            // that turn a sanitized error into UniFFI's raw panic error path.
            std::mem::forget(payload);
            Err(FfiError::Internal)
        }
    }
}

#[uniffi::export]
pub fn ping() -> Result<String, FfiError> {
    ffi_guard(|| Ok("pong".to_owned()))
}

/// Validate basic transport input and return it unchanged; performs no I/O.
#[uniffi::export]
pub fn validate_config(config: DeviceConfig) -> Result<DeviceConfig, FfiError> {
    ffi_guard(|| {
        if !config.database_path.starts_with('/')
            || config.database_path.len() > 4096
            || config.database_path.contains('\0')
            || config.account_scope.trim().is_empty()
            || config.account_scope.len() > 256
            || config.account_scope.chars().any(char::is_control)
        {
            return Err(FfiError::InvalidInput);
        }
        Ok(config)
    })
}

/// Isolated test artifacts only. The sentinel must never reach errors or logs.
#[cfg(feature = "ffi-test")]
#[uniffi::export]
pub fn panic_for_test() -> Result<(), FfiError> {
    ffi_guard(|| panic!("ER_FFI_PRIVATE_SENTINEL patient transcript token /private/path"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stateful_handle_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ErDevice>();
    }

    #[test]
    fn normal_and_typed_error_paths() {
        assert_eq!(ping().unwrap(), "pong");
        assert_eq!(ffi_guard::<()>(|| Err(FfiError::Busy)), Err(FfiError::Busy));
    }

    #[test]
    fn config_round_trip_and_sanitized_validation() {
        let config = DeviceConfig {
            database_path: "/synthetic/ClinicalData/device.sqlite".into(),
            account_scope: "synthetic-account".into(),
        };
        assert!(validate_config(config.clone()).unwrap() == config);
        for invalid in [
            DeviceConfig {
                database_path: "private-relative-path".into(),
                ..config.clone()
            },
            DeviceConfig {
                database_path: "/private\0path".into(),
                ..config.clone()
            },
            DeviceConfig {
                account_scope: " ".into(),
                ..config.clone()
            },
            DeviceConfig {
                account_scope: "private\ntoken".into(),
                ..config.clone()
            },
            DeviceConfig {
                account_scope: "x".repeat(257),
                ..config.clone()
            },
            DeviceConfig {
                database_path: format!("/{}", "x".repeat(4096)),
                ..config.clone()
            },
        ] {
            let Err(error) = validate_config(invalid) else {
                panic!("invalid config accepted")
            };
            assert_eq!(error, FfiError::InvalidInput);
            assert_eq!(error.to_string(), "ER_INVALID_INPUT");
        }
    }

    #[test]
    fn unwind_is_typed_and_stateless_bridge_remains_usable() {
        let error = ffi_guard::<()>(|| panic!("ER_FFI_PRIVATE_SENTINEL")).unwrap_err();
        assert_eq!(error, FfiError::Internal);
        assert_eq!(error.to_string(), "ER_INTERNAL");
        assert_eq!(ping().unwrap(), "pong");
    }

    #[test]
    fn panic_payload_destructor_does_not_escape_guard() {
        struct PanickingDrop;
        impl Drop for PanickingDrop {
            fn drop(&mut self) {
                panic!("ER_FFI_PRIVATE_SENTINEL destructor");
            }
        }
        assert_eq!(
            ffi_guard::<()>(|| std::panic::panic_any(PanickingDrop)),
            Err(FfiError::Internal)
        );
    }

    #[test]
    fn hook_suppresses_payload_in_subprocess() {
        const CHILD: &str = "ER_FFI_HOOK_TEST_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let _ = ffi_guard::<()>(|| panic!("ER_FFI_PRIVATE_SENTINEL"));
            return;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::hook_suppresses_payload_in_subprocess",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("RUST_BACKTRACE", "1")
            .output()
            .unwrap();
        assert!(output.status.success());
        for bytes in [&output.stdout, &output.stderr] {
            assert!(!String::from_utf8_lossy(bytes).contains("ER_FFI_PRIVATE_SENTINEL"));
        }
    }
}

mod device;
pub use device::*;
