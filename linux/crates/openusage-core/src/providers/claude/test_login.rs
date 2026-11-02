struct Prompter {
    prompts: Mutex<Vec<LoginPrompt>>,
}

impl LoginPrompter for Prompter {
    fn show(&self, prompt: &LoginPrompt) {
        if let LoginPrompt::Browser { url, .. } = prompt {
            let url = url.clone();
            thread::spawn(move || complete_callback(&url));
        }
        self.prompts.lock().unwrap().push(prompt.clone());
    }
    fn read_line(&self, _secret: bool) -> Option<String> {
        None
    }
    fn pasted_codes(&self) -> std::sync::mpsc::Receiver<String> {
        let (_sender, receiver) = std::sync::mpsc::channel();
        receiver
    }
}

fn complete_callback(authorize: &str) {
    let url = url::Url::parse(authorize).unwrap();
    let query = |name: &str| {
        url.query_pairs()
            .find(|(key, _)| key == name)
            .unwrap()
            .1
            .into_owned()
    };
    let redirect = url::Url::parse(&query("redirect_uri")).unwrap();
    let port = redirect.port().unwrap();
    let path = redirect.path().to_string();
    let state = query("state");
    for _ in 0..50 {
        if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) {
            let request = format!("GET {path}?code=fake-authorization-code&state={state} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
            stream.write_all(request.as_bytes()).unwrap();
            let mut body = Vec::new();
            let _ = stream.read_to_end(&mut body);
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn browser_sign_in_exchanges_the_code_and_names_the_account_by_email() {
    let http = Fake::new(|request| {
        Ok(match routes(request) {
            "token" => {
                let body =
                    serde_json::from_slice::<serde_json::Value>(request.body.as_deref().unwrap())
                        .unwrap();
                assert_eq!(body["grant_type"], "authorization_code");
                assert_eq!(body["code"], "fake-authorization-code");
                assert_eq!(body["client_id"], CLIENT_ID);
                assert!(body["redirect_uri"]
                    .as_str()
                    .unwrap()
                    .starts_with("http://localhost:"));
                assert_ne!(body["state"], body["code_verifier"]);
                response(
                    200,
                    r#"{"access_token":"claude-access","refresh_token":"claude-refresh","expires_in":3600}"#,
                )
            }
            "profile" => {
                assert_eq!(
                    request.headers.get("Authorization").map(String::as_str),
                    Some("Bearer claude-access")
                );
                assert!(
                    request.headers.get("anthropic-beta").is_none(),
                    "login profile is bearer only"
                );
                response(
                    200,
                    r#"{"account":{"email":"claude@example.com","uuid":"acct"},"organization":{"uuid":"org"}}"#,
                )
            }
            _ => response(404, ""),
        })
    });
    let prompter = Prompter {
        prompts: Mutex::new(Vec::new()),
    };
    let (label, credential) = super::login(&LoginMethod::Default)
        .login(&prompter, &http)
        .unwrap();
    assert_eq!(label, "claude@example.com");
    let LoginPrompt::Browser {
        url,
        accepts_pasted_code,
    } = &prompter.prompts.lock().unwrap()[0]
    else {
        panic!("browser prompt")
    };
    assert!(*accepts_pasted_code);
    let authorize = url::Url::parse(url).unwrap();
    assert_eq!(
        authorize
            .query_pairs()
            .find(|(key, _)| key == "scope")
            .unwrap()
            .1,
        "user:profile"
    );
    assert_eq!(
        authorize
            .query_pairs()
            .find(|(key, _)| key == "code")
            .unwrap()
            .1,
        "true"
    );
    assert!(!url.contains("code_verifier="));
    let Credential::OAuth(tokens) = credential else {
        panic!("oauth")
    };
    assert_eq!(
        tokens
            .extras
            .get(OAuthTokens::IDENTITY_KEY)
            .map(String::as_str),
        Some("acct/org")
    );
    assert!(tokens.expires_at.is_some());
}

#[test]
fn browser_sign_in_fails_with_the_profile_message_when_the_profile_is_unusable() {
    for profile in [
        response(429, r#"{"type":"error"}"#),
        response(200, r#"{"account":{"email":"claude@example.com"}}"#),
    ] {
        let http = Fake::new(move |request| {
            Ok(match routes(request) {
                "token" => response(
                    200,
                    r#"{"access_token":"claude-access","refresh_token":"claude-refresh","expires_in":3600}"#,
                ),
                _ => profile.clone(),
            })
        });
        let error = super::login(&LoginMethod::Default)
            .login(
                &Prompter {
                    prompts: Mutex::new(Vec::new()),
                },
                &http,
            )
            .unwrap_err();
        assert_eq!(
            error,
            LoginError::Failed(
                "Claude didn\u{2019}t return your profile. Click Try Again to sign in again."
                    .into()
            )
        );
    }
}

#[test]
fn client_id_is_the_mac_app_client() {
    assert_eq!(client::CLIENT_ID, "9d1c250a-e61b-44d9-88ed-5944d1962f5e");
}
