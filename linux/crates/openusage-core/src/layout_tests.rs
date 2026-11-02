use super::*;
use crate::descriptor::descriptors;
use crate::kind::ProviderKind;
use plist::Value;
use uuid::Uuid;

fn temp_settings() -> (std::path::PathBuf, Settings) {
    let dir = std::env::temp_dir().join(format!("openusage-layout-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let settings = Settings::at(dir.join("settings.plist"));
    (dir, settings)
}

fn account(kind: ProviderKind, prefix: &str, label: &str) -> Account {
    let mut account = Account::new(kind, label, None);
    account.id = Uuid::parse_str(&format!("{prefix}-0000-4000-8000-000000000000")).unwrap();
    account
}

fn metric(account: &Account, key: &str) -> String {
    format!("{}.{key}", account.card_id())
}

fn keys(account: &Account, layout: &LayoutState) -> Vec<&'static str> {
    layout
        .ordered_descriptors(account)
        .iter()
        .map(|descriptor| descriptor.key)
        .collect()
}

fn visible_keys(account: &Account, layout: &LayoutState) -> Vec<&'static str> {
    layout
        .visible_descriptors(account)
        .iter()
        .map(|descriptor| descriptor.key)
        .collect()
}

struct Ordering {
    dir: std::path::PathBuf,
    settings: Settings,
    accounts: Vec<Account>,
    layout: LayoutState,
}

impl Ordering {
    fn new() -> Self {
        let (dir, settings) = temp_settings();
        Self {
            dir,
            settings,
            accounts: Vec::new(),
            layout: LayoutState::default(),
        }
    }

    fn add(&mut self, kind: ProviderKind) -> String {
        let account = Account::new(kind, kind.display_name(), None);
        let id = account.card_id();
        self.accounts.push(account);
        self.layout.materialize(&self.accounts);
        id
    }

    fn remove(&mut self, id: &str) {
        self.accounts.retain(|account| account.card_id() != id);
    }

    fn ids(&self) -> Vec<String> {
        self.layout
            .ordered_accounts(&self.accounts)
            .iter()
            .map(Account::card_id)
            .collect()
    }

    fn families(&self) -> Vec<String> {
        let mut families = Vec::new();
        for account in self.layout.ordered_accounts(&self.accounts) {
            let kind = account.kind.raw().to_string();
            if !families.contains(&kind) {
                families.push(kind);
            }
        }
        families
    }

    fn groups(&self) -> Vec<(String, Vec<String>)> {
        let mut groups = Vec::new();
        for account in self.layout.ordered_accounts(&self.accounts) {
            let kind = account.kind.raw().to_string();
            if let Some(group) = groups
                .iter_mut()
                .find(|group: &&mut (String, Vec<String>)| group.0 == kind)
            {
                group.1.push(account.card_id());
            } else {
                groups.push((kind, vec![account.card_id()]));
            }
        }
        groups
    }

    fn move_family(&mut self, dragged: &str, target: &str) -> bool {
        let changed = self.layout.move_provider(&self.accounts, dragged, target);
        if changed {
            save(&self.settings, &self.layout).unwrap();
        }
        changed
    }

    fn move_one(&mut self, dragged: &str, target: &str) -> bool {
        let changed = self.layout.move_account(&self.accounts, dragged, target);
        if changed {
            save(&self.settings, &self.layout).unwrap();
        }
        changed
    }

    fn relaunch(&mut self) {
        self.layout = load(&self.settings);
    }
}

impl Drop for Ordering {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn undo_walks_back_multiple_steps() {
    let claude = account(ProviderKind::Claude, "8ad3b217", "Claude");
    let codex = account(ProviderKind::Codex, "11111111", "ChatGPT");
    let cursor = account(ProviderKind::Cursor, "22222222", "Cursor");
    let accounts = vec![claude.clone(), codex.clone(), cursor.clone()];
    let mut layout = LayoutState::default();
    let metrics_before = keys(&claude, &layout);
    let providers_before = layout
        .ordered_accounts(&accounts)
        .iter()
        .map(Account::card_id)
        .collect::<Vec<_>>();

    assert!(layout.move_metric(
        &claude,
        &metric(&claude, "extra"),
        &metric(&claude, "session")
    ));
    let metrics_after = keys(&claude, &layout);
    assert_ne!(metrics_after, metrics_before);
    assert!(layout.move_account(&accounts, &cursor.card_id(), &claude.card_id()));
    assert_ne!(
        layout
            .ordered_accounts(&accounts)
            .iter()
            .map(Account::card_id)
            .collect::<Vec<_>>(),
        providers_before
    );

    assert!(layout.undo());
    assert_eq!(
        layout
            .ordered_accounts(&accounts)
            .iter()
            .map(Account::card_id)
            .collect::<Vec<_>>(),
        providers_before
    );
    assert_eq!(keys(&claude, &layout), metrics_after);

    assert!(layout.undo());
    assert_eq!(keys(&claude, &layout), metrics_before);
    assert!(!layout.can_undo());
    assert!(!layout.undo());
}

#[test]
fn an_account_change_that_keeps_every_card_keeps_undo() {
    let (dir, settings) = temp_settings();
    let claude = account(ProviderKind::Claude, "8ad3b217", "Claude");
    let accounts = vec![claude.clone()];
    let mut layout = LayoutState::default();
    let order_before = keys(&claude, &layout);
    assert!(layout.move_metric(
        &claude,
        &metric(&claude, "weekly"),
        &metric(&claude, "session")
    ));
    save(&settings, &layout).unwrap();

    layout.reload(&settings, &accounts, &accounts);

    assert!(
        layout.undo(),
        "renaming an account keeps the layout's undo history"
    );
    assert_eq!(keys(&claude, &layout), order_before);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_account_change_that_drops_a_card_clears_undo() {
    let (dir, settings) = temp_settings();
    let claude = account(ProviderKind::Claude, "8ad3b217", "Claude");
    let codex = account(ProviderKind::Codex, "11111111", "ChatGPT");
    let mut layout = LayoutState::default();
    assert!(layout.move_metric(
        &claude,
        &metric(&claude, "weekly"),
        &metric(&claude, "session")
    ));
    save(&settings, &layout).unwrap();
    layout.reload(
        &settings,
        &[claude.clone(), codex],
        std::slice::from_ref(&claude),
    );
    assert!(!layout.can_undo());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn undo_stack_is_capped_at_max_depth() {
    let claude = account(ProviderKind::Claude, "8ad3b217", "Claude");
    let mut layout = LayoutState::default();
    let weekly = metric(&claude, "weekly");
    let session = metric(&claude, "session");
    for _ in 0..(MAX_UNDO_DEPTH + 10) {
        assert!(layout.move_metric(&claude, &weekly, &session));
    }
    let mut steps = 0;
    while layout.undo() {
        steps += 1;
    }
    assert_eq!(steps, MAX_UNDO_DEPTH);
}

#[test]
fn a_no_op_does_not_record_undo() {
    let claude = account(ProviderKind::Claude, "8ad3b217", "Claude");
    let mut layout = LayoutState::default();
    let weekly = metric(&claude, "weekly");
    assert!(!layout.move_metric(&claude, &weekly, &weekly));
    assert!(!layout.can_undo());
    assert!(!layout.undo());
    assert!(!layout.set_hidden(&weekly, false));
    assert!(!layout.can_undo());
}

#[test]
fn fresh_order_follows_provider_declarations() {
    let mut layout = LayoutState::default();
    let accounts: Vec<Account> = ProviderKind::ALL
        .into_iter()
        .enumerate()
        .map(|(index, kind)| account(kind, &format!("{:08x}", index + 1), kind.display_name()))
        .collect();
    for account in &accounts {
        assert_eq!(
            keys(account, &layout),
            descriptors(account)
                .iter()
                .map(|descriptor| descriptor.key)
                .collect::<Vec<_>>()
        );
        assert!(layout
            .visible_descriptors(account)
            .iter()
            .all(|descriptor| !layout.is_hidden(&descriptor.id)));
    }
    let _ = &mut layout;
}

#[test]
fn a_fresh_layout_shows_every_metric() {
    let layout = LayoutState::default();
    let claude = account(ProviderKind::Claude, "8ad3b217", "Work");
    let codex = account(ProviderKind::Codex, "11111111", "Work");
    for account in [&claude, &codex] {
        let shown = visible_keys(account, &layout);
        assert!(shown.contains(&"session"), "{shown:?}");
        assert!(shown.contains(&"rateLimitResets"), "{shown:?}");
    }
}

#[test]
fn metric_order_persists() {
    let (dir, settings) = temp_settings();
    let claude = account(ProviderKind::Claude, "8ad3b217", "Claude");
    let mut layout = LayoutState::default();
    let first = keys(&claude, &layout)[0];
    assert!(layout.move_metric(&claude, &metric(&claude, "extra"), &metric(&claude, first)));
    save(&settings, &layout).unwrap();
    assert_eq!(keys(&claude, &load(&settings))[0], "extra");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn visible_metrics_follow_metric_order_and_skip_hidden_ones() {
    let claude = account(ProviderKind::Claude, "8ad3b217", "Claude");
    let mut layout = LayoutState::default();
    assert!(layout.move_metric(
        &claude,
        &metric(&claude, "weekly"),
        &metric(&claude, "session")
    ));
    assert_eq!(
        keys(&claude, &layout),
        vec![
            "weekly",
            "session",
            "sonnet",
            "fable",
            "extra",
            "rateLimitResets"
        ]
    );
    assert!(layout.set_hidden(&metric(&claude, "weekly"), true));
    assert_eq!(
        visible_keys(&claude, &layout),
        vec!["session", "sonnet", "fable", "extra", "rateLimitResets"]
    );
    assert_eq!(
        keys(&claude, &layout)[0],
        "weekly",
        "the account page keeps a hidden metric in its place"
    );
}

#[test]
fn the_account_list_follows_a_reorder() {
    let claude = account(ProviderKind::Claude, "8ad3b217", "Claude");
    let codex = account(ProviderKind::Codex, "11111111", "ChatGPT");
    let cursor = account(ProviderKind::Cursor, "22222222", "Cursor");
    let accounts = vec![claude.clone(), codex.clone(), cursor.clone()];
    let mut layout = LayoutState::default();
    assert!(layout.move_account(&accounts, &cursor.card_id(), &claude.card_id()));
    let ids = layout
        .ordered_accounts(&accounts)
        .iter()
        .map(Account::card_id)
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        vec![cursor.card_id(), claude.card_id(), codex.card_id()]
    );
}

#[test]
fn hiding_and_reordering_an_accounts_metrics_survives_a_restart() {
    let (dir, settings) = temp_settings();
    let claude = account(ProviderKind::Claude, "8ad3b217", "Claude");
    let codex = account(ProviderKind::Codex, "11111111", "ChatGPT");
    let mut layout = LayoutState::default();
    assert!(layout.set_hidden(&metric(&claude, "weekly"), true));
    assert!(layout.move_metric(
        &claude,
        &metric(&claude, "fable"),
        &metric(&claude, "session")
    ));
    save(&settings, &layout).unwrap();

    let relaunched = load(&settings);
    assert!(relaunched.is_hidden(&metric(&claude, "weekly")));
    assert_eq!(
        keys(&claude, &relaunched),
        vec![
            "fable",
            "session",
            "weekly",
            "sonnet",
            "extra",
            "rateLimitResets"
        ]
    );
    assert_eq!(
        visible_keys(&claude, &relaunched),
        vec!["fable", "session", "sonnet", "extra", "rateLimitResets"]
    );
    assert!(
        !relaunched.is_hidden(&metric(&codex, "weekly")),
        "hiding is per account"
    );

    let mut shown = relaunched;
    assert!(shown.set_hidden(&metric(&claude, "weekly"), false));
    save(&settings, &shown).unwrap();
    assert!(!load(&settings).is_hidden(&metric(&claude, "weekly")));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn new_providers_go_to_the_bottom_in_the_order_they_were_added() {
    let mut app = Ordering::new();
    app.add(ProviderKind::Claude);
    app.add(ProviderKind::Codex);
    app.add(ProviderKind::Grok);
    app.add(ProviderKind::Cursor);
    app.add(ProviderKind::Antigravity);
    assert_eq!(
        app.families(),
        vec!["claude", "codex", "grok", "cursor", "antigravity"]
    );
}

#[test]
fn a_new_account_goes_to_the_bottom_of_its_provider() {
    let mut app = Ordering::new();
    let first = app.add(ProviderKind::Claude);
    let codex = app.add(ProviderKind::Codex);
    let grok = app.add(ProviderKind::Grok);
    assert!(app.move_family(&grok, &codex));
    let second = app.add(ProviderKind::Claude);
    assert_eq!(
        app.ids(),
        vec![first.clone(), second.clone(), grok.clone(), codex.clone()]
    );
    assert_eq!(
        app.groups(),
        vec![
            ("claude".into(), vec![first, second]),
            ("grok".into(), vec![grok]),
            ("codex".into(), vec![codex]),
        ]
    );
}

#[test]
fn a_second_account_joins_its_provider_without_a_drag_and_survives_relaunch() {
    let mut app = Ordering::new();
    let first = app.add(ProviderKind::Claude);
    let codex = app.add(ProviderKind::Codex);
    let second = app.add(ProviderKind::Claude);
    let ids = vec![first.clone(), second.clone(), codex.clone()];
    let groups = vec![
        ("claude".into(), vec![first, second]),
        ("codex".into(), vec![codex]),
    ];
    assert_eq!(app.ids(), ids);
    assert_eq!(app.groups(), groups);
    save(&app.settings, &app.layout).unwrap();
    app.relaunch();
    assert_eq!(app.ids(), ids);
    assert_eq!(app.groups(), groups);
}

#[test]
fn an_account_keeps_its_place_when_its_providers_first_account_is_removed() {
    let mut app = Ordering::new();
    let first = app.add(ProviderKind::Claude);
    let codex = app.add(ProviderKind::Codex);
    let second = app.add(ProviderKind::Claude);
    app.remove(&first);
    assert_eq!(app.ids(), vec![second.clone(), codex.clone()]);
    assert_eq!(
        app.groups(),
        vec![
            ("claude".into(), vec![second]),
            ("codex".into(), vec![codex])
        ]
    );
}

#[test]
fn accounts_stay_grouped_by_provider_when_dragged() {
    let mut app = Ordering::new();
    let first = app.add(ProviderKind::Claude);
    let codex = app.add(ProviderKind::Codex);
    let second = app.add(ProviderKind::Claude);
    assert!(!app.move_one(&codex, &second));
    assert_eq!(
        app.ids(),
        vec![first.clone(), second.clone(), codex.clone()]
    );
    assert!(app.move_one(&second, &first));
    assert_eq!(
        app.ids(),
        vec![second.clone(), first.clone(), codex.clone()]
    );
    assert_eq!(
        app.groups(),
        vec![
            ("claude".into(), vec![second, first]),
            ("codex".into(), vec![codex])
        ]
    );
}

#[test]
fn a_single_account_provider_can_move_past_another_single_account_provider() {
    let mut app = Ordering::new();
    let claude = app.add(ProviderKind::Claude);
    let codex = app.add(ProviderKind::Codex);
    assert!(app.move_one(&claude, &codex));
    assert_eq!(app.ids(), vec![codex, claude]);
}

#[test]
fn a_provider_removed_and_added_back_goes_to_the_bottom() {
    let mut app = Ordering::new();
    let claude = app.add(ProviderKind::Claude);
    app.add(ProviderKind::Codex);
    app.add(ProviderKind::Grok);
    save(
        &app.settings,
        &LayoutState {
            provider_order: app.ids(),
            ..LayoutState::default()
        },
    )
    .unwrap();
    app.layout = load(&app.settings);
    app.remove(&claude);
    app.add(ProviderKind::Claude);
    assert_eq!(app.families(), vec!["codex", "grok", "claude"]);
}

#[test]
fn saved_order_and_new_providers_survive_relaunch() {
    let mut app = Ordering::new();
    let claude = app.add(ProviderKind::Claude);
    let codex = app.add(ProviderKind::Codex);
    assert!(app.move_family(&codex, &claude));
    app.add(ProviderKind::Grok);
    app.add(ProviderKind::Cursor);
    app.relaunch();
    assert_eq!(app.families(), vec!["codex", "claude", "grok", "cursor"]);
}

#[test]
fn a_new_metric_follows_declared_order_after_the_saved_ones() {
    let claude = account(ProviderKind::Claude, "8ad3b217", "Claude");
    let mut layout = LayoutState::default();
    layout.metric_order_by_provider.insert(
        claude.card_id(),
        vec![
            metric(&claude, "session"),
            metric(&claude, "weekly"),
            metric(&claude, "gone"),
        ],
    );
    assert_eq!(
        keys(&claude, &layout),
        vec![
            "session",
            "weekly",
            "sonnet",
            "fable",
            "extra",
            "rateLimitResets"
        ]
    );
}

#[test]
fn saves_the_whole_layout_under_one_key() {
    let (dir, settings) = temp_settings();
    let state = LayoutState {
        hidden_metric_ids: BTreeSet::from(["claude@8ad3b217.extra".into()]),
        provider_order: vec!["cursor@22222222".into(), "claude@8ad3b217".into()],
        metric_order_by_provider: BTreeMap::from([(
            "claude@8ad3b217".into(),
            vec![
                "claude@8ad3b217.weekly".into(),
                "claude@8ad3b217.session".into(),
            ],
        )]),
        seeded_provider_ids: BTreeSet::from(["claude@8ad3b217".into(), "cursor@22222222".into()]),
        undo: Vec::new(),
    };
    save(&settings, &state).unwrap();
    assert_eq!(load(&settings), state);
    let keys: Vec<_> = settings
        .read_all()
        .keys()
        .filter(|key| key.starts_with("openusage.layout"))
        .cloned()
        .collect();
    assert_eq!(keys, vec!["openusage.layout.v2".to_string()]);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_mac_layout_with_stars_and_hidden_metrics_keeps_its_hidden_metrics() {
    let (dir, settings) = temp_settings();
    let claude = account(ProviderKind::Claude, "8ad3b217", "Claude");
    let codex = account(ProviderKind::Codex, "11111111", "ChatGPT");
    let cursor = account(ProviderKind::Cursor, "22222222", "Cursor");
    let saved = format!(
        r#"{{
            "hiddenMetricIDs": ["{claude_weekly}", "{claude_extra}"],
            "pinnedMetricIDs": ["{claude_session}", "{codex_session}"],
            "providerOrder": ["{codex}", "{claude}", "{cursor}"],
            "metricOrderByProvider": {{"{claude}": ["{claude_fable}", "{claude_session}"]}},
            "seededProviderIDs": ["{claude}", "{codex}", "{cursor}"]
        }}"#,
        claude = claude.card_id(),
        codex = codex.card_id(),
        cursor = cursor.card_id(),
        claude_weekly = metric(&claude, "weekly"),
        claude_extra = metric(&claude, "extra"),
        claude_session = metric(&claude, "session"),
        claude_fable = metric(&claude, "fable"),
        codex_session = metric(&codex, "session"),
    );
    settings
        .update(|values| {
            values.insert(KEY.into(), Value::Data(saved.into_bytes()));
        })
        .unwrap();
    let layout = load(&settings);
    let accounts = vec![claude.clone(), codex.clone(), cursor.clone()];
    let ids = layout
        .ordered_accounts(&accounts)
        .iter()
        .map(Account::card_id)
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        vec![codex.card_id(), claude.card_id(), cursor.card_id()],
        "the provider order survives"
    );
    let order = keys(&claude, &layout);
    assert_eq!(
        &order[..2],
        ["fable", "session"],
        "the metric order survives"
    );
    let hidden: Vec<_> = descriptors(&claude)
        .into_iter()
        .filter(|descriptor| layout.is_hidden(&descriptor.id))
        .map(|descriptor| descriptor.key)
        .collect();
    assert_eq!(
        hidden,
        vec!["weekly", "extra"],
        "a metric saved as hidden stays hidden"
    );
    assert!(!layout.is_hidden(&metric(&codex, "weekly")));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_layout_saved_without_hidden_metrics_still_loads() {
    let (dir, settings) = temp_settings();
    let claude = account(ProviderKind::Claude, "8ad3b217", "Claude");
    let codex = account(ProviderKind::Codex, "11111111", "ChatGPT");
    let cursor = account(ProviderKind::Cursor, "22222222", "Cursor");
    let saved = format!(
        r#"{{
            "providerOrder": ["{codex}", "{claude}", "{cursor}"],
            "metricOrderByProvider": {{"{claude}": ["{fable}", "{session}"]}},
            "seededProviderIDs": ["{claude}", "{codex}", "{cursor}"]
        }}"#,
        claude = claude.card_id(),
        codex = codex.card_id(),
        cursor = cursor.card_id(),
        fable = metric(&claude, "fable"),
        session = metric(&claude, "session"),
    );
    settings
        .update(|values| {
            values.insert(KEY.into(), Value::Data(saved.into_bytes()));
        })
        .unwrap();
    let layout = load(&settings);
    let accounts = [codex.clone(), claude.clone(), cursor.clone()];
    assert_eq!(
        layout
            .ordered_accounts(&accounts)
            .iter()
            .map(Account::card_id)
            .collect::<Vec<_>>(),
        vec![codex.card_id(), claude.card_id(), cursor.card_id()]
    );
    let order = keys(&claude, &layout);
    assert_eq!(&order[..2], ["fable", "session"]);
    assert_eq!(visible_keys(&claude, &layout), keys(&claude, &layout));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_layout_that_cannot_be_read_starts_over() {
    let (dir, settings) = temp_settings();
    settings
        .update(|values| {
            values.insert(KEY.into(), Value::Data(b"not-json".to_vec()));
        })
        .unwrap();
    assert_eq!(load(&settings), LayoutState::default());
    settings
        .update(|values| {
            values.insert(
                KEY.into(),
                Value::Data(br#"{"hiddenMetricIDs":[]}"#.to_vec()),
            );
        })
        .unwrap();
    assert_eq!(load(&settings), LayoutState::default());
    settings
        .update(|values| {
            values.insert(KEY.into(), Value::String("nope".into()));
        })
        .unwrap();
    assert_eq!(load(&settings), LayoutState::default());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn prune_drops_metrics_and_accounts_that_no_longer_exist_and_keeps_provider_order() {
    let claude = account(ProviderKind::Claude, "8ad3b217", "Claude");
    let mut layout = LayoutState::default();
    layout.provider_order = vec!["gone".into(), claude.card_id()];
    layout.hidden_metric_ids.insert(metric(&claude, "weekly"));
    layout.hidden_metric_ids.insert(metric(&claude, "nope"));
    layout
        .metric_order_by_provider
        .insert("gone".into(), vec!["gone.weekly".into()]);
    layout
        .metric_order_by_provider
        .insert(claude.card_id(), vec![metric(&claude, "fable")]);
    layout.seeded_provider_ids.insert("gone".into());
    layout.seeded_provider_ids.insert(claude.card_id());
    layout.prune(std::slice::from_ref(&claude));
    assert!(layout
        .hidden_metric_ids
        .contains(&metric(&claude, "weekly")));
    assert!(!layout.hidden_metric_ids.contains(&metric(&claude, "nope")));
    assert!(!layout.metric_order_by_provider.contains_key("gone"));
    assert!(layout
        .metric_order_by_provider
        .contains_key(&claude.card_id()));
    assert_eq!(
        layout.seeded_provider_ids,
        BTreeSet::from([claude.card_id()])
    );
    assert_eq!(
        layout.provider_order,
        vec!["gone".to_string(), claude.card_id()]
    );
    assert_eq!(
        layout.ordered_accounts(std::slice::from_ref(&claude))[0].card_id(),
        claude.card_id()
    );
}

#[test]
fn zero_seven_keys_are_left_unread_and_unwritten() {
    let (dir, settings) = temp_settings();
    let v1 = br#"["codex","claude"]"#.to_vec();
    settings
        .update(|values| {
            values.insert(
                "openusage.layout.v1.providerOrder".into(),
                Value::Data(v1.clone()),
            );
            values.insert(
                "openusage.enabledProviders.v1".into(),
                Value::String("claude".into()),
            );
        })
        .unwrap();
    let claude = account(ProviderKind::Claude, "8ad3b217", "Claude");
    let codex = account(ProviderKind::Codex, "11111111", "ChatGPT");
    let layout = load(&settings);
    let ordered = layout.ordered_accounts(&[claude.clone(), codex]);
    assert_eq!(
        ordered[0].card_id(),
        claude.card_id(),
        "0.7.x order is not applied"
    );
    assert!(keys(&claude, &layout).starts_with(&["session", "weekly", "sonnet"]));
    assert!(visible_keys(&claude, &layout)
        .iter()
        .any(|key| *key == "extra"));
    save(&settings, &layout).unwrap();
    assert_eq!(
        settings
            .get("openusage.layout.v1.providerOrder")
            .unwrap()
            .into_data()
            .unwrap(),
        v1
    );
    assert_eq!(
        settings
            .get("openusage.enabledProviders.v1")
            .unwrap()
            .as_string(),
        Some("claude")
    );
    let _ = std::fs::remove_dir_all(dir);
}
