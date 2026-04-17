// Library entry point — exposes internal modules for integration tests.
// The binary entry point remains main.rs.
pub mod abuse;
pub mod app_state;
pub mod auth;
pub mod channel;
pub mod chat;
pub mod config_validation;
pub mod connections;
pub mod diagnostics;
pub mod ec2_control;
pub mod error;
pub mod ip;
pub mod redaction;
pub mod state;
pub mod voice;
pub mod ws;

// Backward-compatibility shims for crates that still import the pre-refactor
// module layout (`domain::*` / `handlers::*`).
pub mod domain {
    pub mod auth {
        pub use crate::auth::auth::*;
        pub use crate::auth::jwt::{ACCESS_TOKEN_TTL_SECS, sign_access_token};
    }

    pub mod auth_rate_limiter {
        pub use crate::auth::auth_rate_limiter::*;
    }

    pub mod bug_report {
        pub use crate::diagnostics::bug_report::*;
    }

    pub mod invite {
        pub use crate::channel::invite::*;
    }

    pub mod join_rate_limiter {
        pub use crate::abuse::join_rate_limiter::*;
    }

    pub mod jwt {
        pub use crate::auth::jwt::*;
    }

    pub mod llm_client {
        pub use crate::diagnostics::llm_client::*;
    }

    #[cfg(any(test, feature = "test-support"))]
    pub mod mock_sfu_bridge {
        pub use crate::voice::mock_sfu_bridge::*;
    }

    pub mod phrase {
        pub use crate::auth::phrase::*;
    }

    pub mod recovery_rate_limiter {
        pub use crate::auth::recovery_rate_limiter::*;
    }

    pub mod sfu_bridge {
        pub use crate::voice::sfu_bridge::*;
    }

    pub mod turn_cred {
        pub use crate::voice::turn_cred::*;
    }
}

pub mod handlers {
    pub mod ip {
        pub use crate::ip::*;
    }

    #[cfg(feature = "test-metrics")]
    pub mod test_metrics {
        pub use crate::diagnostics::test_metrics::*;
    }

    pub mod ws {
        pub use crate::ws::ws::*;
    }
}
