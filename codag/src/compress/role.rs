//! Deterministic post-classification role overrides.
//!
//! Faithful port of `override_role` + the cascade regexes from
//! `v2/src/v2/classifier/overrides.py`.
//!
//! `Role` is the public enum; `override_role` returns the corrected role given
//! a template/message and the current (possibly `None`) role.

use regex::Regex;
use std::sync::OnceLock;

use crate::compress::Role;

fn stack_frame_re() -> &'static Regex {
    // _STACK_FRAME_RE = r'^\s*File\s+"[^"]+",\s+line\s+(?:\d+|<\*>),\s+in\s+\S'
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r#"^\s*File\s+"[^"]+",\s+line\s+(?:\d+|<\*>),\s+in\s+\S"#).unwrap()
    })
}

fn traceback_header_re() -> &'static Regex {
    // _TRACEBACK_HEADER_RE = r"^\s*Traceback \(most recent call last\):"
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^\s*Traceback \(most recent call last\):").unwrap())
}

fn framework_glue_patterns() -> &'static [Regex] {
    static R: OnceLock<Vec<Regex>> = OnceLock::new();
    R.get_or_init(|| {
        vec![
            Regex::new(r"^\s*raise exc\b").unwrap(),
            Regex::new(r"^\s*await wrap_app_handling_exceptions\b").unwrap(),
            Regex::new(r"^\s*await self\.app\(scope, receive").unwrap(),
            Regex::new(r"^\s*await self\.middleware_stack\(").unwrap(),
            Regex::new(r"^\s*await app\(scope, receive").unwrap(),
            Regex::new(r"^\s*return await self\.app\(scope, receive").unwrap(),
        ]
    })
}

fn healthy_http_re() -> &'static Regex {
    // _HEALTHY_HTTP_RE = r'\bHTTP/\d\.\d"\s+(?:[23]\d\d|<\*>)\s+(?:OK|Created|No Content|Not Modified|<\*>)'
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r#"\bHTTP/\d\.\d"\s+(?:[23]\d\d|<\*>)\s+(?:OK|Created|No Content|Not Modified|<\*>)"#)
            .unwrap()
    })
}

fn http_5xx_re() -> &'static Regex {
    // _HTTP_5XX_RE = r'\bHTTP/\d\.\d"\s+5\d\d\b'
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"\bHTTP/\d\.\d"\s+5\d\d\b"#).unwrap())
}

fn consequence_patterns() -> &'static [Regex] {
    static R: OnceLock<Vec<Regex>> = OnceLock::new();
    R.get_or_init(|| {
        vec![
            // _HTTP_ERROR_ARROW_RE
            Regex::new(
                r"(?:->|=>)\s+(?:5\d\d|429|408)\b|\b(?:5\d\d|429)\s+returned\b|\b(?:5\d\d|429)\s+rate\b",
            )
            .unwrap(),
            // _K8S_FAILURE_RE
            Regex::new(
                r"\b(?:OOMKilled|CrashLoopBackOff|ImagePullBackOff|ErrImagePull|FailedScheduling|Evicted|FailedMount|FailedCreate|NodeNotReady|NetworkNotReady|Preempted)\b",
            )
            .unwrap(),
            // _IO_FAILURE_RE (IGNORECASE)
            Regex::new(
                r"(?i)\b(?:ENOSPC|EROFS|EACCES|EMFILE|ENFILE)\b|\bNo space left on device\b|\b(?:write|read)\s+failed\b",
            )
            .unwrap(),
            // _TLS_FAILURE_RE (IGNORECASE)
            Regex::new(
                r"(?i)\bTLS handshake failed\b|\bcertificate (?:has )?expired\b|\bx509:\s+certificate\b",
            )
            .unwrap(),
            // _CIRCUIT_OPEN_RE (IGNORECASE)
            Regex::new(r"(?i)\bcircuit\s+(?:breaker\s+)?\S*?\s*(?:state\s*=\s*)?OPEN\b").unwrap(),
            // _OP_TIMEOUT_RE (IGNORECASE)
            Regex::new(
                r"(?i)\btimeout\s+acquiring\b|\btimeout\s+after\s+\d+(?:ms|s|m)\b|\b(?:i/o|io|connect|read|write|query|heartbeat|request|rpc)\s+timeout\b",
            )
            .unwrap(),
            // _FAILURE_RATE_RE (IGNORECASE)
            Regex::new(
                r"(?i)\b\d+%\s+(?:of\s+\S+\s+(?:requests|calls|responses))|(?:rate|miss\s+rate|failure\s+rate)\s+(?:hit\s+|jumped\s+\d+%\s*->\s*)?\d+%|\b\d+%\s+rate(?:\s+over|d)?\b|\b\d+%\s+rate-limited\b",
            )
            .unwrap(),
            // _POOL_DEPLETION_RE (IGNORECASE)
            Regex::new(
                r"(?i)\b(?:pool|connection\s+pool|db_pool|threadpool|thread_pool)\s*[:\(]?\s*(?:exhausted|in_use=|waiting=|saturated|full)\b",
            )
            .unwrap(),
        ]
    })
}

/// The role classifier. Stateless; provides `override_role`.
#[derive(Debug, Default, Clone)]
pub struct RoleClassifier;

impl RoleClassifier {
    pub fn new() -> Self {
        RoleClassifier
    }

    /// Port of `override_role(raw_template, current_role)`.
    /// `current_role == None` is `Role::None`.
    pub fn override_role(raw_template: &str, current_role: Option<Role>) -> Role {
        let current = current_role.unwrap_or(Role::None);
        if raw_template.is_empty() {
            return current;
        }

        // Stack frames + traceback header -> context.
        if stack_frame_re().is_match(raw_template) || traceback_header_re().is_match(raw_template)
        {
            return Role::Context;
        }

        // Framework re-raise glue -> routine.
        for pat in framework_glue_patterns() {
            if pat.is_match(raw_template) {
                return Role::Routine;
            }
        }

        // HTTP 5xx -> consequence, ALWAYS (before healthy-HTTP).
        if http_5xx_re().is_match(raw_template) {
            return Role::Consequence;
        }

        // Healthy 2xx/3xx -> routine.
        if healthy_http_re().is_match(raw_template) {
            return Role::Routine;
        }

        // Cascade rescue: only escalate from weak roles (None/Context/Routine).
        if matches!(current, Role::None | Role::Context | Role::Routine) {
            for pat in consequence_patterns() {
                if pat.is_match(raw_template) {
                    return Role::Consequence;
                }
            }
        }

        current
    }

    /// Port of `is_stack_frame`.
    pub fn is_stack_frame(text: &str) -> bool {
        !text.is_empty() && stack_frame_re().is_match(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn or(s: &str) -> Role {
        RoleClassifier::override_role(s, None)
    }

    #[test]
    fn http_5xx_consequence() {
        assert_eq!(
            or(r#"127.0.0.1 - "GET /x HTTP/1.1" 503 12"#),
            Role::Consequence
        );
    }

    #[test]
    fn healthy_http_routine() {
        assert_eq!(or(r#""GET /x HTTP/1.1" 200 OK"#), Role::Routine);
    }

    #[test]
    fn oomkilled_consequence() {
        assert_eq!(or("pod web-7 OOMKilled restart"), Role::Consequence);
    }

    #[test]
    fn enospc_consequence() {
        assert_eq!(or("disk write error: ENOSPC"), Role::Consequence);
        assert_eq!(or("No space left on device"), Role::Consequence);
    }

    #[test]
    fn tls_consequence() {
        assert_eq!(or("TLS handshake failed with upstream"), Role::Consequence);
        assert_eq!(or("certificate has expired"), Role::Consequence);
    }

    #[test]
    fn circuit_open_consequence() {
        assert_eq!(or("circuit breaker payments OPEN"), Role::Consequence);
        assert_eq!(or("circuit state=OPEN"), Role::Consequence);
    }

    #[test]
    fn op_timeout_consequence() {
        assert_eq!(or("timeout acquiring connection"), Role::Consequence);
        assert_eq!(or("timeout after 30s"), Role::Consequence);
        assert_eq!(or("query timeout"), Role::Consequence);
    }

    #[test]
    fn failure_rate_consequence() {
        assert_eq!(or("504 rate 89% of /x requests over 60s"), Role::Consequence);
        assert_eq!(or("failure rate hit 47%"), Role::Consequence);
    }

    #[test]
    fn pool_depletion_consequence() {
        assert_eq!(or("db_pool exhausted waiting=12"), Role::Consequence);
        assert_eq!(or("connection pool: saturated"), Role::Consequence);
    }

    #[test]
    fn stack_frame_context() {
        assert_eq!(
            or(r#"  File "app.py", line 42, in handler"#),
            Role::Context
        );
        assert!(RoleClassifier::is_stack_frame(
            r#"  File "app.py", line 42, in handler"#
        ));
    }

    #[test]
    fn does_not_downgrade_strong_role() {
        // current root_cause-equivalent: we model strong roles only as the ones
        // we keep; here a Consequence pattern with current Consequence stays.
        assert_eq!(
            RoleClassifier::override_role("db_pool exhausted", Some(Role::Consequence)),
            Role::Consequence
        );
    }
}
