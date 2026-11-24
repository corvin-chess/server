//! Outbound mail sinks for email OTP (CV-133).
//! Real SMTP stays out of scope until hosting/DNS (CV-032); tests use CaptureMailer.

use std::collections::HashMap;
use std::sync::Mutex;

/// Delivers a one-time login/register code to an email address.
pub trait Mailer: Send + Sync {
    fn send_otp(&self, email: &str, code: &str) -> Result<(), String>;
}

/// Logs that an OTP was sent without printing the code (safe default for local runs).
#[derive(Debug, Default)]
pub struct LogMailer;

impl Mailer for LogMailer {
    fn send_otp(&self, email: &str, _code: &str) -> Result<(), String> {
        eprintln!("corvin-server: OTP queued for {email}");
        Ok(())
    }
}

/// Test sink that retains the latest code per email.
#[derive(Debug, Default)]
pub struct CaptureMailer {
    last: Mutex<HashMap<String, String>>,
}

impl CaptureMailer {
    pub fn last_code_for(&self, email: &str) -> Option<String> {
        let key = email.trim().to_ascii_lowercase();
        self.last
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&key)
            .cloned()
    }
}

impl Mailer for CaptureMailer {
    fn send_otp(&self, email: &str, code: &str) -> Result<(), String> {
        let key = email.trim().to_ascii_lowercase();
        self.last
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(key, code.to_owned());
        Ok(())
    }
}
