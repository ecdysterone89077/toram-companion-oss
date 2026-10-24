use super::*;

pub(super) unsafe fn create_controls(hwnd: HWND) -> UiHandles {
    let font = unsafe { GetStockObject(DEFAULT_GUI_FONT) };
    let label = |text: &str, x, y, w, h| unsafe {
        create_ctrl(
            hwnd,
            "STATIC",
            text,
            WS_CHILD | WS_VISIBLE,
            x,
            y,
            w,
            h,
            0,
            font,
        )
    };
    let button = |text: &str, x, y, w, id| unsafe {
        create_ctrl(
            hwnd,
            "BUTTON",
            text,
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_PUSHBUTTON,
            x,
            y,
            w,
            28,
            id,
            font,
        )
    };
    let hwnd_telemetry = label("Waiting for clients...", 16, 10, 990, 20);
    let hwnd_combat = label("Feedback OFF | No received combat samples", 16, 32, 990, 30);
    let hwnd_status = label(
        "Select clients, then apply a profile or run a command batch.",
        16,
        66,
        990,
        20,
    );
    label("Target", 16, 98, 46, 22);
    let hwnd_targets = unsafe {
        create_ctrl(
            hwnd,
            "COMBOBOX",
            "",
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_VSCROLL | 3,
            64,
            94,
            270,
            260,
            ID_COMBO_TARGET,
            font,
        )
    };
    button("Refresh clients", 344, 94, 112, ID_BTN_CLIENTS);
    button("Cancel batch", 466, 94, 110, ID_BTN_CANCEL);
    let btn_hotkeys = button("Hotkeys F1-F8: OFF", 586, 94, 174, ID_BTN_HOTKEYS_TOGGLE);
    button("Reset Account Login", 770, 94, 220, ID_BTN_RESET_ACCOUNT);
    for (i, (text, id)) in [
        ("Native reset", ID_PROFILE_NATIVE),
        ("Balanced +100", ID_PROFILE_BALANCED),
        ("Burst +300", ID_PROFILE_BURST),
        ("Recovery +10 (experimental)", ID_PROFILE_RECOVERY),
        ("Legacy preset", ID_PROFILE_SAFE),
    ]
    .into_iter()
    .enumerate()
    {
        button(text, 16 + i as i32 * 198, 134, 184, id);
    }
    let btn_invincibility = button(
        "Invincible [?] (F1 / 32)",
        16,
        174,
        488,
        ID_BTN_INVINCIBILITY,
    );
    let btn_crit = button(
        "Stability + Crit [?] (F2 / 73)",
        16,
        210,
        488,
        ID_BTN_ALWAYS_CRIT,
    );
    let btn_inst = button(
        "Instant Attack [?] (F3 / 72)",
        16,
        246,
        488,
        ID_BTN_INSTANT_ATK,
    );
    let btn_mp = button(
        "MP Recovery [?] (F4 / 74)",
        16,
        282,
        488,
        ID_BTN_UNLIMITED_MP,
    );
    let btn_speed = button("Speed [?] (F5 / 68)", 516, 174, 474, ID_BTN_SPEED);
    let btn_no_collision = button(
        "No Collision [?] (F6 / 69)",
        516,
        210,
        474,
        ID_BTN_NO_COLLISION,
    );
    let btn_range = button("Engage [?] (F7 / 71)", 516, 246, 474, ID_BTN_RANGE);
    let btn_rigidity = button(
        "Anti-Rigidity [?] (F8 / 70)",
        516,
        282,
        474,
        ID_BTN_CLEAR_DELAY,
    );
    for (i, (text, id)) in [
        ("175% request [75]", ID_BTN_CRIT_175),
        ("200% request [76]", ID_BTN_CRIT_200),
        ("250% capped [77]", ID_BTN_CRIT_250),
        ("300% capped [78]", ID_BTN_CRIT_300),
        ("Native damage [79]", ID_BTN_CRIT_RESET),
    ]
    .into_iter()
    .enumerate()
    {
        button(text, 16 + i as i32 * 198, 326, 184, id);
    }
    // The additive ladder is bounded by the measured ceiling, so its steps
    // now land on the same value. The menu shows the capped maximum and the
    // retired unsafe step instead of three identical buttons. The ladder
    // commands themselves still work from the command line and from saved
    // plans.
    button(
        "Damage max (capped) [85]",
        16,
        358,
        488,
        ID_BTN_CRIT_ADD_300,
    );
    button("+1000 UNSAFE [81]", 516, 358, 474, ID_BTN_CRIT_ADD_1000);
    for (i, (text, id)) in [
        ("MP +10 [86]", ID_BTN_MP_10),
        ("MP +25 [87]", ID_BTN_MP_25),
        ("MP +50 [88]", ID_BTN_MP_50),
        ("Native MP [91]", ID_BTN_MP_NATIVE),
    ]
    .into_iter()
    .enumerate()
    {
        button(text, 16 + i as i32 * 250, 390, 238, id);
    }
    for (i, (text, id)) in [
        ("Probe resources", ID_BTN_PROBE),
        ("Local refill [2]", ID_BTN_HEAL),
        ("Feedback ON", ID_BTN_FEEDBACK_ON),
        ("Feedback OFF", ID_BTN_FEEDBACK_OFF),
        ("Reset counters", ID_BTN_RESET_METRICS),
        ("Restart worker", ID_BTN_HOT_RELOAD),
    ]
    .into_iter()
    .enumerate()
    {
        button(text, 16 + i as i32 * 166, 426, 156, id);
    }
    label("Capture", 16, 722, 60, 22);
    for (i, (text, id)) in [
        ("Action capture [80]", ID_BTN_CAPTURE_ACTION),
        ("Response capture [82]", ID_BTN_CAPTURE_RESPONSES),
        ("Mark phase [116]", ID_BTN_CAPTURE_PHASE),
        ("Stop capture + log [93]", ID_BTN_CAPTURE_STOP),
    ]
    .into_iter()
    .enumerate()
    {
        button(text, 84 + i as i32 * 230, 722, 220, id);
    }
    label("Batch", 16, 466, 56, 22);
    let hwnd_edit_custom = unsafe {
        create_ctrl(
            hwnd,
            "EDIT",
            "83+89",
            WS_CHILD | WS_VISIBLE | WS_BORDER | WS_TABSTOP,
            76,
            462,
            310,
            26,
            ID_EDIT_CUSTOM,
            font,
        )
    };
    button("Run batch", 396, 462, 92, ID_BTN_RUN_CUSTOM);
    button("Save plan", 498, 462, 104, ID_BTN_SAVE_PLAN);
    button("Load plan", 612, 462, 104, ID_BTN_LOAD_PLAN);
    button("Stop worker", 726, 462, 126, ID_BTN_UNLOAD);
    button("Refresh log", 862, 462, 128, ID_BTN_REFRESH_LOG);
    label("Companion-only final (Faunas disabled)", 16, 762, 340, 22);
    let _ = FAUNAS_UI_FEATURES.len();
    let btn_faunas: Vec<usize> = Vec::new();
    let initial_log = RUNNER
        .get()
        .map(|r| read_tail_lines(&r.wcore_log_path, 40))
        .unwrap_or_default()
        .replace("\r\n", "\n")
        .replace('\n', "\r\n");
    let hwnd_log = unsafe {
        create_ctrl(
            hwnd,
            "EDIT",
            &initial_log,
            WS_CHILD
                | WS_VISIBLE
                | WS_BORDER
                | WS_VSCROLL
                | ES_MULTILINE
                | ES_AUTOVSCROLL
                | ES_READONLY,
            16,
            500,
            974,
            214,
            0,
            font,
        )
    };
    UiHandles {
        hwnd_main: hwnd as usize,
        hwnd_telemetry: hwnd_telemetry as usize,
        hwnd_combat: hwnd_combat as usize,
        hwnd_targets: hwnd_targets as usize,
        hwnd_status: hwnd_status as usize,
        hwnd_edit_custom: hwnd_edit_custom as usize,
        hwnd_log: hwnd_log as usize,
        btn_invincibility: btn_invincibility as usize,
        btn_crit: btn_crit as usize,
        btn_inst: btn_inst as usize,
        btn_mp: btn_mp as usize,
        btn_speed: btn_speed as usize,
        btn_no_collision: btn_no_collision as usize,
        btn_range: btn_range as usize,
        btn_rigidity: btn_rigidity as usize,
        btn_hotkeys: btn_hotkeys as usize,
        btn_faunas,
        target_options: vec![TargetSelection::All],
    }
}
