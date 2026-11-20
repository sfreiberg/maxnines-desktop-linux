//! Sending requests: through ureq in the app, and to something that answers by
//! itself in the tests. Only failing to get an answer is an error here: what the
//! server answers, whatever the status, is for the caller to read.

use std::time::Duration;

use ureq::tls::{RootCerts, TlsConfig};

use crate::Error;

/// A request, as the client builds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: &'static str,
    pub url: String,
    /// The access token, sent as `Authorization: Bearer …`.
    pub bearer: Option<String>,
    pub body: Body,
    /// The longest to wait for the whole answer.
    pub timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body {
    None,
    Json(String),
    /// Already form-encoded.
    Form(String),
}

/// What came back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub status: u16,
    pub text: String,
}

impl Answer {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// Sends a request and gives back the answer.
pub trait Http: Send + Sync {
    fn send(&self, request: &Request) -> Result<Answer, Error>;
}

/// Sends through ureq, trusting the certificates the system does: the ones in its
/// trust store, such as `/etc/ssl/certs`, which a company's own certificate authority
/// is added to, as the Mac and Windows apps do.
pub struct Ureq {
    agent: ureq::Agent,
}

impl Default for Ureq {
    fn default() -> Self {
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .tls_config(
                TlsConfig::builder()
                    .root_certs(RootCerts::PlatformVerifier)
                    .build(),
            )
            .user_agent(concat!(
                "maxnines-desktop-linux/",
                env!("CARGO_PKG_VERSION")
            ))
            .build();
        Self {
            agent: ureq::Agent::new_with_config(config),
        }
    }
}

impl Http for Ureq {
    fn send(&self, request: &Request) -> Result<Answer, Error> {
        let unreachable = |error: &dyn std::fmt::Display| Error::Unreachable(error.to_string());
        let mut builder = ureq::http::Request::builder()
            .method(request.method)
            .uri(&request.url);
        if let Some(token) = &request.bearer {
            builder = builder.header("Authorization", format!("Bearer {token}"));
        }
        let (kind, text) = match &request.body {
            Body::None if matches!(request.method, "GET" | "HEAD" | "DELETE") => (None, None),
            // A POST says it's empty, rather than sending an empty chunked body.
            Body::None => (None, Some(String::new())),
            Body::Json(text) => (Some("application/json"), Some(text.clone())),
            Body::Form(text) => (
                Some("application/x-www-form-urlencoded"),
                Some(text.clone()),
            ),
        };
        if let Some(kind) = kind {
            builder = builder.header("Content-Type", kind);
        }
        let response = match text {
            Some(text) => {
                let built = builder.body(text).map_err(|error| unreachable(&error))?;
                let configured = self
                    .agent
                    .configure_request(built)
                    .timeout_global(Some(request.timeout))
                    .build();
                self.agent.run(configured)
            }
            None => {
                let built = builder.body(()).map_err(|error| unreachable(&error))?;
                let configured = self
                    .agent
                    .configure_request(built)
                    .timeout_global(Some(request.timeout))
                    .build();
                self.agent.run(configured)
            }
        };
        let mut response = response.map_err(|error| unreachable(&error))?;
        let status = response.status().as_u16();
        let text = response
            .body_mut()
            .read_to_string()
            .map_err(|error| unreachable(&error))?;
        Ok(Answer { status, text })
    }
}

#[cfg(test)]
pub mod fake {
    //! A server that answers by itself, for the tests.

    use std::sync::Mutex;

    use super::{Answer, Http, Request};
    use crate::Error;

    type Answering = Box<dyn Fn(&Request) -> Result<Answer, Error> + Send + Sync>;

    /// Answers each request with `answer`, and remembers what it was sent.
    pub struct Fake {
        answer: Answering,
        sent: Mutex<Vec<Request>>,
    }

    impl Fake {
        pub fn new(
            answer: impl Fn(&Request) -> Result<Answer, Error> + Send + Sync + 'static,
        ) -> Self {
            Self {
                answer: Box::new(answer),
                sent: Mutex::new(Vec::new()),
            }
        }

        pub fn sent(&self) -> Vec<Request> {
            self.sent.lock().unwrap().clone()
        }
    }

    impl Http for Fake {
        fn send(&self, request: &Request) -> Result<Answer, Error> {
            self.sent.lock().unwrap().push(request.clone());
            (self.answer)(request)
        }
    }

    pub fn answer(status: u16, text: &str) -> Result<Answer, Error> {
        Ok(Answer {
            status,
            text: text.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use ureq::tls::RootCerts;

    use super::Ureq;

    #[test]
    fn certificates_are_checked_against_the_systems_trust_store() {
        let ureq = Ureq::default();
        assert!(matches!(
            ureq.agent.config().tls_config().root_certs(),
            RootCerts::PlatformVerifier
        ));
    }
}
