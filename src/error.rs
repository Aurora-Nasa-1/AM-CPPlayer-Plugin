//! Typed business failures carried in the response frame.

use crate::models::Failure;

pub fn failure(category: &str, message: impl Into<String>) -> Failure {
    Failure {
        category: category.to_string(),
        message: message.into(),
    }
}

pub fn unsupported(message: impl Into<String>) -> Failure {
    failure("Unsupported", message)
}

pub fn not_found(message: impl Into<String>) -> Failure {
    failure("NotFound", message)
}

pub fn authentication(message: impl Into<String>) -> Failure {
    failure("Authentication", message)
}

pub fn network(message: impl Into<String>) -> Failure {
    failure("Network", message)
}

pub fn rate_limited(message: impl Into<String>) -> Failure {
    failure("RateLimit", message)
}

pub fn timeout(message: impl Into<String>) -> Failure {
    failure("Timeout", message)
}

pub fn invalid_response(message: impl Into<String>) -> Failure {
    failure("InvalidResponse", message)
}
