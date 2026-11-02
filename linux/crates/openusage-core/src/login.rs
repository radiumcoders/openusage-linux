//! Sign-in building blocks shared by the provider ports: PKCE, the loopback OAuth callback server, the
//! browser-or-pasted-code race, device-code polling, and the prompts the CLI shows.
//! Ports of `PKCE.swift`, `LoopbackCallbackServer.swift`, `OAuthLoginSupport.swift`, `DeviceCodePoller.swift`.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine;
use rand::RngCore;
use sha2::{Digest, Sha256};

use crate::error::LoginError;

/// What the user is asked to do while signing in.
#[derive(Debug, Clone, PartialEq)]
pub enum LoginPrompt {
    /// Open this URL; when `accepts_pasted_code`, a pasted code or callback URL also completes it.
    Browser {
        url: String,
        accepts_pasted_code: bool,
    },
    /// The page shows a token for the user to paste. Nothing comes back on its own.
    BrowserToken { url: String },
    DeviceCode {
        user_code: String,
        verification_url: String,
    },
    /// Ask for an API key or token (read with echo off in a terminal).
    Secret {
        label: String,
        key_page: Option<String>,
    },
}

pub trait LoginPrompter: Send + Sync {
    /// Shows the prompt (and opens a URL in the browser where there is one).
    fn show(&self, prompt: &LoginPrompt);
    /// One line the user typed or pasted; `None` at end of input. `secret` hides the echo.
    fn read_line(&self, secret: bool) -> Option<String>;
    /// Lines pasted while a browser sign-in waits for its callback. The sender drops at end of input,
    /// which is not a cancel: the browser can still finish the sign-in.
    fn pasted_codes(&self) -> mpsc::Receiver<String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

pub fn base64_url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

impl Pkce {
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        let verifier = base64_url(&bytes);
        let challenge = Self::challenge(&verifier);
        Self {
            verifier,
            challenge,
        }
    }

    pub fn challenge(verifier: &str) -> String {
        base64_url(&Sha256::digest(verifier.as_bytes()))
    }
}

pub fn random_state() -> String {
    uuid::Uuid::new_v4().to_string().to_uppercase()
}

/// The OAuth redirect listener on 127.0.0.1.
pub struct LoopbackServer {
    listener: TcpListener,
    state: String,
    path: String,
    cancelled: AtomicBool,
    pub redirect_uri: String,
}

impl LoopbackServer {
    /// `port: None` picks a free port. Fails when a fixed port is taken.
    pub fn start(state: &str, port: Option<u16>, path: &str) -> Result<Self, LoginError> {
        let listener = TcpListener::bind(("127.0.0.1", port.unwrap_or(0)))
            .map_err(|error| LoginError::ListenerFailed(error.to_string()))?;
        let port = listener
            .local_addr()
            .map_err(|error| LoginError::ListenerFailed(error.to_string()))?
            .port();
        Ok(Self {
            listener,
            state: state.to_string(),
            path: path.to_string(),
            cancelled: AtomicBool::new(false),
            redirect_uri: format!("http://localhost:{port}{path}"),
        })
    }

    /// Serves requests until one carries the code (or an error) for this state.
    pub fn wait_for_code(&self) -> Result<String, LoginError> {
        for stream in self.listener.incoming() {
            if self.cancelled.load(Ordering::SeqCst) {
                return Err(LoginError::Cancelled);
            }
            let Ok(stream) = stream else { continue };
            if let Some(result) = self.serve(stream) {
                return result;
            }
        }
        Err(LoginError::Cancelled)
    }

    fn serve(&self, mut stream: TcpStream) -> Option<Result<String, LoginError>> {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 4096];
        while !buffer.windows(4).any(|window| window == b"\r\n\r\n") && buffer.len() < 16_384 {
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(count) => buffer.extend_from_slice(&chunk[..count]),
            }
        }
        let request = String::from_utf8_lossy(&buffer);
        let line = request.lines().next().unwrap_or("");
        let target = line.split(' ').nth(1).unwrap_or("");
        if !line.starts_with("GET ") {
            reply(&mut stream, 405, "Only GET is supported.");
            return None;
        }
        let Ok(url) = url::Url::parse(&format!("http://localhost{target}")) else {
            reply(&mut stream, 400, "Sign-in failed.");
            return Some(Err(LoginError::InvalidCallback));
        };
        if url.path() != self.path {
            reply(&mut stream, 404, "Sign-in callback not found.");
            return None;
        }
        let query = |name: &str| {
            url.query_pairs()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.into_owned())
        };
        if query("state").as_deref() != Some(self.state.as_str()) {
            reply(&mut stream, 400, "Sign-in state did not match.");
            return None;
        }
        if let Some(error) = query("error") {
            reply(&mut stream, 400, "Sign-in failed.");
            return Some(Err(LoginError::AuthorizationFailed(error)));
        }
        match query("code").filter(|code| !code.is_empty()) {
            Some(code) => {
                reply(
                    &mut stream,
                    200,
                    "Sign-in complete. You may close this page.",
                );
                Some(Ok(code))
            }
            None => {
                reply(&mut stream, 400, "Authorization code was missing.");
                Some(Err(LoginError::InvalidCallback))
            }
        }
    }
}

fn reply(stream: &mut TcpStream, status: u16, body: &str) {
    let body = format!("<!doctype html><title>OpenUsage</title><p>{body}</p>");
    let reason = if status == 200 { "OK" } else { "Error" };
    let _ = write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

impl LoopbackServer {
    /// Ends a `wait_for_code` running on another thread.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        if let Ok(address) = self.listener.local_addr() {
            let _ = TcpStream::connect(address);
        }
    }
}

/// The browser's callback or a pasted code, whichever comes first.
#[derive(Debug, Clone, PartialEq)]
pub enum AuthorizationInput {
    Callback(String),
    Pasted(String),
}

/// Builds the authorize URL, shows it, and waits for the loopback callback or a pasted code.
/// An input that ends without a code means none is coming; the browser still decides.
#[allow(clippy::too_many_arguments)]
pub fn authorize_in_browser(
    authorize_url: &str,
    client_id: &str,
    scopes: &str,
    state: &str,
    pkce: &Pkce,
    server: LoopbackServer,
    extra_query: &[(&str, &str)],
    prompter: &dyn LoginPrompter,
) -> Result<String, LoginError> {
    let mut url = url::Url::parse(authorize_url).map_err(|_| LoginError::InvalidResponse)?;
    url.query_pairs_mut()
        .append_pair("client_id", client_id)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", &server.redirect_uri)
        .append_pair("scope", scopes)
        .append_pair("code_challenge", &pkce.challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", state);
    for (key, value) in extra_query {
        url.query_pairs_mut().append_pair(key, value);
    }
    prompter.show(&LoginPrompt::Browser {
        url: url.to_string(),
        accepts_pasted_code: true,
    });
    let pasted = prompter.pasted_codes();
    let input = thread::scope(|scope| {
        let (sender, receiver) = mpsc::channel();
        let server = &server;
        scope.spawn(move || {
            let _ = sender.send(server.wait_for_code());
        });
        let mut paste_open = true;
        let result = loop {
            if let Ok(result) = receiver.recv_timeout(Duration::from_millis(100)) {
                break result.map(AuthorizationInput::Callback);
            }
            if paste_open {
                match pasted.try_recv() {
                    Ok(line) if !line.trim().is_empty() => {
                        break Ok(AuthorizationInput::Pasted(line))
                    }
                    Ok(_) | Err(mpsc::TryRecvError::Empty) => {}
                    Err(mpsc::TryRecvError::Disconnected) => paste_open = false,
                }
            }
        };
        server.cancel();
        result
    })?;
    authorization_code(input, state)
}

/// The code from a callback, or from a pasted code, `code#state` pair, or whole callback URL.
pub fn authorization_code(
    input: AuthorizationInput,
    expected_state: &str,
) -> Result<String, LoginError> {
    match input {
        AuthorizationInput::Callback(code) => Ok(code),
        AuthorizationInput::Pasted(value) => {
            let trimmed = value.trim();
            let (code, state) = if let Ok(url) = url::Url::parse(trimmed)
                .ok()
                .filter(|url| url.host().is_some())
                .ok_or(())
            {
                let get = |name: &str| {
                    url.query_pairs()
                        .find(|(key, _)| key == name)
                        .map(|(_, value)| value.into_owned())
                };
                (get("code"), get("state"))
            } else if let Some((code, state)) = trimmed.split_once('#') {
                (Some(code.to_string()), Some(state.to_string()))
            } else {
                (Some(trimmed.to_string()), None)
            };
            if let Some(state) = state {
                if state != expected_state {
                    return Err(LoginError::InvalidState);
                }
            }
            code.filter(|code| !code.is_empty())
                .ok_or(LoginError::InvalidCode)
        }
    }
}

/// What one device-code poll learned.
pub enum Poll<T> {
    Done(T),
    Pending,
    SlowDown,
}

/// Polls every `interval` seconds until `poll` finishes or `expires_in` seconds pass.
pub fn poll_device_code<T>(
    interval: u64,
    expires_in: u64,
    mut poll: impl FnMut() -> Result<Poll<T>, LoginError>,
) -> Result<T, LoginError> {
    let deadline = Instant::now() + Duration::from_secs(expires_in);
    let mut wait = Duration::from_secs(interval.max(1));
    while Instant::now() < deadline {
        thread::sleep(wait);
        match poll()? {
            Poll::Done(value) => return Ok(value),
            Poll::Pending => {}
            Poll::SlowDown => wait += Duration::from_secs(5),
        }
    }
    Err(LoginError::Expired)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_matches_rfc_7636_appendix_b() {
        assert_eq!(
            Pkce::challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn pasted_codes_are_read_from_urls_and_pairs() {
        let url = AuthorizationInput::Pasted("http://localhost:1/cb?code=abc&state=s".into());
        assert_eq!(authorization_code(url, "s").unwrap(), "abc");
        assert_eq!(
            authorization_code(AuthorizationInput::Pasted("abc#s".into()), "s").unwrap(),
            "abc"
        );
        assert_eq!(
            authorization_code(AuthorizationInput::Pasted("abc#x".into()), "s"),
            Err(LoginError::InvalidState)
        );
        assert_eq!(
            authorization_code(AuthorizationInput::Pasted(" abc ".into()), "s").unwrap(),
            "abc"
        );
    }

    #[test]
    fn loopback_server_returns_the_code_for_its_state() {
        let server = LoopbackServer::start("s1", None, "/callback").unwrap();
        let address = server.listener.local_addr().unwrap();
        let client = thread::spawn(move || {
            let mut stream = TcpStream::connect(address).unwrap();
            stream
                .write_all(b"GET /callback?state=wrong&code=x HTTP/1.1\r\n\r\n")
                .unwrap();
            let mut stream = TcpStream::connect(address).unwrap();
            stream
                .write_all(b"GET /callback?state=s1&code=good HTTP/1.1\r\n\r\n")
                .unwrap();
            let mut reply = String::new();
            stream.read_to_string(&mut reply).unwrap();
            reply
        });
        assert_eq!(server.wait_for_code().unwrap(), "good");
        assert!(client.join().unwrap().starts_with("HTTP/1.1 200"));
    }
}
