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

fn port_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap()
}

#[test]
fn browser_sign_in_uses_port_1455_and_the_pi_originator() {
    let _lock = port_lock();
    let access = jwt(r#"{"https://api.openai.com/auth":{"chatgpt_account_id":"chatgpt-account"}}"#);
    let id_token = jwt(r#"{"email":"codex@example.com"}"#);
    let http = Fake::new(move |request| {
        assert_eq!(path_of(&request.url), "/oauth/token");
        let body = std::str::from_utf8(request.body.as_deref().unwrap())
            .unwrap()
            .to_string();
        assert!(body.contains("grant_type=authorization_code"));
        assert!(body.contains(&format!("client_id={CLIENT_ID}")));
        assert!(body.contains("code=fake-authorization-code"));
        assert!(body.contains("redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback"));
        assert!(body.contains("code_verifier="));
        Ok(response(
            200,
            &format!(
                r#"{{"access_token":"{access}","refresh_token":"codex-refresh","id_token":"{id_token}","expires_in":3600}}"#
            ),
        ))
    });
    let prompter = Prompter {
        prompts: Mutex::new(Vec::new()),
    };
    let (label, credential) = super::login(&LoginMethod::Default)
        .login(&prompter, &http)
        .unwrap();
    assert_eq!(label, "codex@example.com");
    let LoginPrompt::Browser { url, .. } = &prompter.prompts.lock().unwrap()[0] else {
        panic!("browser")
    };
    let authorize = url::Url::parse(url).unwrap();
    let query = |name: &str| {
        authorize
            .query_pairs()
            .find(|(key, _)| key == name)
            .unwrap()
            .1
            .into_owned()
    };
    assert_eq!(query("originator"), "pi");
    assert_eq!(query("scope"), "openid profile email offline_access");
    assert_eq!(query("redirect_uri"), "http://localhost:1455/auth/callback");
    assert_eq!(query("id_token_add_organizations"), "true");
    let Credential::OAuth(tokens) = credential else {
        panic!("oauth")
    };
    assert_eq!(
        tokens.extras.get("chatgpt_account_id").map(String::as_str),
        Some("chatgpt-account")
    );
    assert_eq!(
        tokens
            .extras
            .get(OAuthTokens::IDENTITY_KEY)
            .map(String::as_str),
        Some("chatgpt-account/codex@example.com")
    );
}

#[test]
fn a_taken_callback_port_falls_back_to_the_device_code() {
    let _lock = port_lock();
    let _listener = TcpListener::bind(("127.0.0.1", client::REDIRECT_PORT)).unwrap();
    let http = Fake::new(|request| {
        Ok(match path_of(&request.url).as_str() {
            "/api/accounts/deviceauth/usercode" => response(
                200,
                r#"{"device_auth_id":"dev","user_code":"ABCD-EFGH","interval":"1"}"#,
            ),
            "/api/accounts/deviceauth/token" => response(
                200,
                r#"{"authorization_code":"device-code","code_verifier":"device-verifier"}"#,
            ),
            "/oauth/token" => {
                let body = std::str::from_utf8(request.body.as_deref().unwrap()).unwrap();
                assert!(body.contains("code=device-code"));
                assert!(body.contains("code_verifier=device-verifier"));
                assert!(body.contains(
                    "redirect_uri=https%3A%2F%2Fauth.openai.com%2Fdeviceauth%2Fcallback"
                ));
                response(
                    200,
                    r#"{"access_token":"device-access","refresh_token":"device-refresh","expires_in":60}"#,
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
    assert_eq!(label, "ChatGPT Account");
    let LoginPrompt::DeviceCode {
        user_code,
        verification_url,
    } = &prompter.prompts.lock().unwrap()[0]
    else {
        panic!("device")
    };
    assert_eq!(user_code, "ABCD-EFGH");
    assert_eq!(verification_url, "https://auth.openai.com/codex/device");
    let Credential::OAuth(tokens) = credential else {
        panic!("oauth")
    };
    assert_eq!(tokens.access_token, "device-access");
}

#[test]
fn device_sign_in_reports_an_expired_code() {
    let _lock = port_lock();
    let _listener = TcpListener::bind(("127.0.0.1", client::REDIRECT_PORT)).unwrap();
    let http = Fake::new(|request| {
        Ok(match path_of(&request.url).as_str() {
            "/api/accounts/deviceauth/usercode" => response(
                200,
                r#"{"device_auth_id":"dev","user_code":"ABCD-EFGH","interval":1}"#,
            ),
            _ => response(400, r#"{"error":"expired_token"}"#),
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
    assert_eq!(error, LoginError::Failed("Device sign-in expired.".into()));
}
