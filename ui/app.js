(function () {
  let currentConfig = null;
  let activeTargetId = null;
  let editingChannelId = null;
  let editingFriendId = null;
  let lastScanData = null;
  let pendingDetectedUrl = null;
  const history = [];

  function send(msg) {
    if (window.ipc && typeof window.ipc.postMessage === "function") {
      window.ipc.postMessage(JSON.stringify(msg));
    }
  }

  function $(id) {
    return document.getElementById(id);
  }

  function showToast(message, level = "info") {
    const container = $("toastContainer");
    const el = document.createElement("div");
    el.className = "toast " + (level === "error" ? "error" : level === "ok" ? "ok" : "");
    el.textContent = message;
    container.appendChild(el);
    setTimeout(() => el.remove(), 3400);
  }

  function splitCsv(val) {
    return (val || "")
      .split(/[,、]/)
      .map((s) => s.trim())
      .filter(Boolean);
  }

  function joinCsv(arr) {
    return (arr || []).join(", ");
  }

  function genId(prefix) {
    return prefix + "_" + Date.now().toString(36) + "_" + Math.random().toString(36).slice(2, 6);
  }

  function switchTab(tabName) {
    document.querySelectorAll(".nav-btn").forEach((b) => {
      b.classList.toggle("active", b.dataset.tab === tabName);
    });
    document.querySelectorAll(".tab-panel").forEach((p) => {
      p.classList.toggle("active", p.id === "tab-" + tabName);
    });
  }

  document.querySelectorAll(".nav-btn").forEach((btn) => {
    btn.addEventListener("click", () => switchTab(btn.dataset.tab));
  });

  document.querySelectorAll("[data-jump-tab]").forEach((btn) => {
    btn.addEventListener("click", () => switchTab(btn.dataset.jumpTab));
  });

  function targetDisplayName(t) {
    if (t.name && t.name.trim()) return t.name.trim();
    if (t.passphrases && t.passphrases.length && t.passphrases[0].trim()) {
      return t.passphrases[0].trim();
    }
    if (t.channel_id && t.channel_id.trim()) return "ID:" + t.channel_id.trim();
    if (t.user_id && t.user_id.trim()) return "User:" + t.user_id.trim();
    return "名称未設定";
  }

  function isValidTarget(t) {
    return !!(
      (t.name && t.name.trim()) ||
      (t.channel_id && t.channel_id.trim()) ||
      (t.user_id && t.user_id.trim()) ||
      (t.webhook_url && t.webhook_url.trim())
    );
  }

  function getTargets() {
    if (!currentConfig || !currentConfig.discord_targets) return [];
    return (currentConfig.discord_targets.targets || []).filter(isValidTarget);
  }

  function renderQuickTargets() {
    const targets = getTargets();
    const friends = targets.filter((t) => t.kind === "dm" || t.kind === "mention");
    const channels = targets.filter((t) => t.kind === "channel");

    // 1. Grouped <select>
    const sel = $("quickTargetSelect");
    sel.innerHTML = "";
    const defOpt = document.createElement("option");
    defOpt.value = "";
    defOpt.textContent = "現在の画面（宛先未指定）";
    sel.appendChild(defOpt);

    if (friends.length) {
      const grp = document.createElement("optgroup");
      grp.label = "フレンド / DM";
      friends.forEach((f) => {
        const opt = document.createElement("option");
        opt.value = f.id;
        const prefix = f.kind === "mention" ? "@" : "DM: ";
        const pass = f.passphrases && f.passphrases.length ? ` [合言葉: ${f.passphrases[0]}]` : "";
        opt.textContent = `${prefix}${targetDisplayName(f)}${pass}`;
        grp.appendChild(opt);
      });
      sel.appendChild(grp);
    }

    if (channels.length) {
      const grp = document.createElement("optgroup");
      grp.label = "サーバー / チャンネル";
      channels.forEach((c) => {
        const opt = document.createElement("option");
        opt.value = c.id;
        const srv = c.server_name && c.server_name.trim() ? `[${c.server_name.trim()}] ` : "";
        const pass = c.passphrases && c.passphrases.length ? ` [合言葉: ${c.passphrases[0]}]` : "";
        opt.textContent = `${srv}#${targetDisplayName(c)}${pass}`;
        grp.appendChild(opt);
      });
      sel.appendChild(grp);
    }

    sel.value = activeTargetId || "";

    // 2. One-click chips on Dashboard
    const chips = $("quickTargetChips");
    chips.innerHTML = "";

    const makeChip = (id, label, cls) => {
      const b = document.createElement("button");
      b.type = "button";
      b.className = "quick-chip " + cls + ((activeTargetId || "") === (id || "") ? " active" : "");
      b.textContent = label;
      b.addEventListener("click", () => {
        activeTargetId = id || null;
        send({ type: "SelectDiscordTarget", target_id: activeTargetId, navigate: !!activeTargetId });
      });
      chips.appendChild(b);
    };

    makeChip("", "現在の画面", "");
    friends.forEach((f) => {
      const prefix = f.kind === "mention" ? "@" : "DM:";
      makeChip(f.id, `${prefix}${targetDisplayName(f)}`, "dm");
    });
    channels.forEach((c) => {
      makeChip(c.id, `#${targetDisplayName(c)}`, "ch");
    });
  }

  function renderServerChannels() {
    const wrap = $("channelGroups");
    const channels = getTargets().filter((t) => t.kind === "channel");
    $("chCountBadge").textContent = `${channels.length}件`;

    if (!channels.length) {
      wrap.innerHTML =
        '<div class="empty-state">まだサーバー・チャンネルが登録されていません。上のフォームまたはDiscord URL貼り付けから登録してください。</div>';
      return;
    }

    // Group by server_name (or guild_id)
    const groups = new Map();
    channels.forEach((c) => {
      const key =
        (c.server_name && c.server_name.trim()) ||
        (c.guild_id && c.guild_id.trim() ? `サーバー (${c.guild_id.trim()})` : "サーバー名未設定");
      if (!groups.has(key)) groups.set(key, []);
      groups.get(key).push(c);
    });

    wrap.innerHTML = "";
    groups.forEach((list, serverLabel) => {
      const box = document.createElement("div");
      box.className = "server-group";

      const header = document.createElement("div");
      header.className = "server-group-header";
      const guildIds = [...new Set(list.map((c) => c.guild_id).filter(Boolean))];
      header.innerHTML = `<span></span><span class="server-id-tag"></span>`;
      header.children[0].textContent = `${serverLabel} (${list.length}ch)`;
      header.children[1].textContent = guildIds.length ? `Server ID: ${guildIds[0]}` : "";
      box.appendChild(header);

      const rows = document.createElement("div");
      rows.className = "channel-rows";

      list.forEach((c) => {
        const row = document.createElement("div");
        row.className = "dest-row" + (c.id === activeTargetId ? " selected" : "");

        const left = document.createElement("div");
        left.className = "dest-left";

        const hash = document.createElement("div");
        hash.className = "channel-hash";
        hash.textContent = "#";
        left.appendChild(hash);

        const meta = document.createElement("div");
        meta.className = "dest-meta";

        const nameLine = document.createElement("div");
        nameLine.className = "dest-name-line";
        const nm = document.createElement("span");
        nm.className = "dest-name";
        nm.textContent = targetDisplayName(c);
        nameLine.appendChild(nm);

        if (c.id === activeTargetId) {
          const b = document.createElement("span");
          b.className = "tag-badge green";
          b.textContent = "選択中";
          nameLine.appendChild(b);
        }
        (c.passphrases || []).forEach((p) => {
          const b = document.createElement("span");
          b.className = "tag-badge";
          b.textContent = "合言葉: " + p;
          nameLine.appendChild(b);
        });
        if (c.webhook_url && c.webhook_url.trim()) {
          const b = document.createElement("span");
          b.className = "tag-badge amber";
          b.textContent = "Webhook";
          nameLine.appendChild(b);
        }

        const sub = document.createElement("div");
        sub.className = "dest-sub";
        sub.textContent = `チャンネルID: ${c.channel_id || "未設定"}`;

        meta.appendChild(nameLine);
        meta.appendChild(sub);
        left.appendChild(meta);

        const actions = document.createElement("div");
        actions.className = "row gap-xs";

        const btnSel = document.createElement("button");
        btnSel.className = "btn-success btn-sm";
        btnSel.textContent = c.id === activeTargetId ? "選択中" : "選択";
        btnSel.addEventListener("click", () => {
          send({ type: "SelectDiscordTarget", target_id: c.id, navigate: true });
        });

        const btnOpen = document.createElement("button");
        btnOpen.className = "btn-secondary btn-sm";
        btnOpen.textContent = "開く";
        btnOpen.addEventListener("click", () => {
          send({ type: "NavigateDiscordTarget", target_id: c.id });
        });

        const btnEdit = document.createElement("button");
        btnEdit.className = "btn-secondary btn-sm";
        btnEdit.textContent = "編集";
        btnEdit.addEventListener("click", () => startEditChannel(c));

        const btnDel = document.createElement("button");
        btnDel.className = "btn-danger btn-sm";
        btnDel.textContent = "削除";
        btnDel.addEventListener("click", () => deleteTargetById(c.id));

        actions.appendChild(btnSel);
        actions.appendChild(btnOpen);
        actions.appendChild(btnEdit);
        actions.appendChild(btnDel);

        row.appendChild(left);
        row.appendChild(actions);
        rows.appendChild(row);
      });

      box.appendChild(rows);
      wrap.appendChild(box);
    });
  }

  function renderFriends() {
    const wrap = $("friendsList");
    const friends = getTargets().filter((t) => t.kind === "dm" || t.kind === "mention");
    $("frCountBadge").textContent = `${friends.length}人`;

    if (!friends.length) {
      wrap.innerHTML =
        '<div class="empty-state">まだフレンドが登録されていません。上のフォームから名前・合言葉・DMチャンネルID（またはユーザーID）を登録できます。</div>';
      return;
    }

    wrap.innerHTML = "";
    friends.forEach((f) => {
      const row = document.createElement("div");
      row.className = "dest-row friend-card" + (f.id === activeTargetId ? " selected" : "");

      const left = document.createElement("div");
      left.className = "dest-left";

      const av = document.createElement("div");
      av.className = "avatar-circle " + (f.kind === "mention" ? "mention" : "dm");
      const displayName = targetDisplayName(f);
      av.textContent = displayName.slice(0, 1).toUpperCase();
      left.appendChild(av);

      const meta = document.createElement("div");
      meta.className = "dest-meta";

      const nameLine = document.createElement("div");
      nameLine.className = "dest-name-line";
      const nm = document.createElement("span");
      nm.className = "dest-name";
      nm.textContent = displayName;
      nameLine.appendChild(nm);

      const kindTag = document.createElement("span");
      kindTag.className = "tag-badge " + (f.kind === "dm" ? "green" : "");
      kindTag.textContent = f.kind === "mention" ? "メンション" : "DM";
      nameLine.appendChild(kindTag);

      if (f.id === activeTargetId) {
        const b = document.createElement("span");
        b.className = "tag-badge green";
        b.textContent = "選択中";
        nameLine.appendChild(b);
      }
      (f.passphrases || []).forEach((p) => {
        const b = document.createElement("span");
        b.className = "tag-badge";
        b.textContent = "合言葉: " + p;
        nameLine.appendChild(b);
      });
      if (f.prepend_mention && f.user_id) {
        const b = document.createElement("span");
        b.className = "tag-badge amber";
        b.textContent = "@自動メンション";
        nameLine.appendChild(b);
      }

      const subParts = [];
      if (f.channel_id) subParts.push(`DM ID: ${f.channel_id}`);
      if (f.user_id) subParts.push(`ユーザーID: ${f.user_id}`);
      const sub = document.createElement("div");
      sub.className = "dest-sub";
      sub.textContent = subParts.length ? subParts.join(" / ") : "ID未設定（名前検索で確認）";

      meta.appendChild(nameLine);
      meta.appendChild(sub);
      left.appendChild(meta);

      const actions = document.createElement("div");
      actions.className = "row gap-xs";

      const btnSel = document.createElement("button");
      btnSel.className = "btn-success btn-sm";
      btnSel.textContent = f.id === activeTargetId ? "選択中" : "この人を選ぶ";
      btnSel.addEventListener("click", () => {
        send({ type: "SelectDiscordTarget", target_id: f.id, navigate: true });
      });

      const btnOpen = document.createElement("button");
      btnOpen.className = "btn-secondary btn-sm";
      btnOpen.textContent = "DMを開く";
      btnOpen.addEventListener("click", () => {
        send({ type: "NavigateDiscordTarget", target_id: f.id });
      });

      const btnEdit = document.createElement("button");
      btnEdit.className = "btn-secondary btn-sm";
      btnEdit.textContent = "編集";
      btnEdit.addEventListener("click", () => startEditFriend(f));

      const btnDel = document.createElement("button");
      btnDel.className = "btn-danger btn-sm";
      btnDel.textContent = "削除";
      btnDel.addEventListener("click", () => deleteTargetById(f.id));

      actions.appendChild(btnSel);
      actions.appendChild(btnOpen);
      actions.appendChild(btnEdit);
      actions.appendChild(btnDel);

      row.appendChild(left);
      row.appendChild(actions);
      wrap.appendChild(row);
    });
  }

  function renderAllTargets() {
    renderQuickTargets();
    renderServerChannels();
    renderFriends();
  }

  function resetChannelForm() {
    editingChannelId = null;
    $("chFormTitle").textContent = "サーバー・チャンネルを追加";
    $("btnChSave").textContent = "+ チャンネルを登録";
    $("btnChCancelEdit").style.display = "none";
    $("chUrlPaste").value = "";
    $("chServerName").value = "";
    $("chName").value = "";
    $("chPass").value = "";
    $("chGuildId").value = "";
    $("chChannelId").value = "";
    $("chWebhook").value = "";
  }

  function startEditChannel(c) {
    editingChannelId = c.id;
    $("chFormTitle").textContent = `チャンネル「${targetDisplayName(c)}」を編集`;
    $("btnChSave").textContent = "変更を保存";
    $("btnChCancelEdit").style.display = "inline-block";
    $("chServerName").value = c.server_name || "";
    $("chName").value = c.name || "";
    $("chPass").value = joinCsv(c.passphrases);
    $("chGuildId").value = c.guild_id || "";
    $("chChannelId").value = c.channel_id || "";
    $("chWebhook").value = c.webhook_url || "";
    $("chName").focus();
  }

  function resetFriendForm() {
    editingFriendId = null;
    $("frFormTitle").textContent = "フレンド（DM / メンション）を追加";
    $("btnFrSave").textContent = "+ フレンドを登録";
    $("btnFrCancelEdit").style.display = "none";
    $("frUrlPaste").value = "";
    $("frName").value = "";
    $("frPass").value = "";
    $("frKind").value = "dm";
    $("frChannelId").value = "";
    $("frUserId").value = "";
    $("frPrependMention").checked = false;
  }

  function startEditFriend(f) {
    editingFriendId = f.id;
    $("frFormTitle").textContent = `フレンド「${targetDisplayName(f)}」を編集`;
    $("btnFrSave").textContent = "変更を保存";
    $("btnFrCancelEdit").style.display = "inline-block";
    $("frName").value = f.name || "";
    $("frPass").value = joinCsv(f.passphrases);
    $("frKind").value = f.kind === "mention" ? "mention" : "dm";
    $("frChannelId").value = f.channel_id || "";
    $("frUserId").value = f.user_id || "";
    $("frPrependMention").checked = !!f.prepend_mention;
    $("frName").focus();
  }

  function saveTargetsList(newTargets) {
    if (!currentConfig) return;
    const cleaned = newTargets.filter(isValidTarget);
    currentConfig.discord_targets.targets = cleaned;
    if (activeTargetId && !cleaned.some((t) => t.id === activeTargetId)) {
      activeTargetId = null;
      currentConfig.discord_targets.active_target_id = null;
    }
    renderAllTargets();
    saveNow();
  }

  function deleteTargetById(id) {
    const next = getTargets().filter((t) => t.id !== id);
    if (editingChannelId === id) resetChannelForm();
    if (editingFriendId === id) resetFriendForm();
    saveTargetsList(next);
  }

  function renderDictionary(entries) {
    const wrap = $("dictRows");
    wrap.innerHTML = "";
    (entries || []).forEach((item) => {
      const row = document.createElement("div");
      row.className = "dict-row";
      row.innerHTML = `
        <input type="text" class="input dict-from" placeholder="置換前（例: くさ）" />
        <span class="dict-arrow">→</span>
        <input type="text" class="input dict-to" placeholder="置換後（例: 草）" />
        <button type="button" class="btn-danger btn-sm dict-del">削除</button>
      `;
      row.querySelector(".dict-from").value = item.from || "";
      row.querySelector(".dict-to").value = item.to || "";
      row.querySelector(".dict-del").addEventListener("click", () => row.remove());
      wrap.appendChild(row);
    });
  }

  function applyConfigToUi(cfg, configPath) {
    currentConfig = cfg;
    activeTargetId = (cfg.discord_targets && cfg.discord_targets.active_target_id) || null;
    if (configPath) {
      $("configPathHint").textContent = "設定ファイル: " + configPath;
    }

    updateModeButtons(cfg.input_mode);
    $("vadThreshold").value = cfg.vad.threshold;
    $("vadThresholdVal").textContent = Number(cfg.vad.threshold).toFixed(3);
    $("chkVadAutoAdjust").checked = cfg.vad.auto_adjust !== false;
    updateThresholdMarker(cfg.vad.threshold);

    const nc = cfg.noise_cancel || {
      enabled: true,
      strength: 0.7,
      highpass_enabled: true,
      noise_gate_enabled: false,
      vad_prob_threshold: 0.3,
      radio_clarity_mode: true,
    };
    $("chkNcEnabled").checked = !!nc.enabled;
    $("ncStrength").value = nc.strength;
    $("ncStrengthVal").textContent = Math.round(nc.strength * 100) + "%";
    $("chkNcRadioClarity").checked = nc.radio_clarity_mode !== false;
    $("chkNcHighpass").checked = !!nc.highpass_enabled;
    $("chkNcGate").checked = !!nc.noise_gate_enabled;
    $("ncVadProbThreshold").value = nc.vad_prob_threshold;

    const dt = cfg.discord_targets || {};
    $("targetPrefixes").value = joinCsv(dt.switch_prefixes);
    $("targetSuffixes").value = joinCsv(dt.switch_suffixes);
    $("targetResets").value = joinCsv(dt.reset_phrases);
    $("targetNavDelay").value = dt.navigate_delay_ms || 420;
    $("chkTargetAutoNavSwitch").checked = !!dt.auto_navigate_on_switch;
    $("chkTargetAutoNavSend").checked = !!dt.auto_navigate_on_send;
    $("chkTargetQuickSwitcher").checked = dt.use_quick_switcher_for_unknown !== false;
    $("chkTargetStandalone").checked = !!dt.allow_standalone_passphrase;
    renderAllTargets();

    $("chkAutoStart").checked = !!cfg.auto_start;
    $("chkStartMinimized").checked = !!cfg.start_minimized;

    const di = cfg.discord_integration || {
      bot_token: "",
      use_bot_for_send: false,
      clipboard_url_watch: true,
    };
    $("botTokenInput").value = di.bot_token || "";
    $("chkBotSend").checked = !!di.use_bot_for_send;
    $("chkClipboardWatch").checked = di.clipboard_url_watch !== false;

    $("cmdSend").value = joinCsv(cfg.commands.send);
    $("cmdClearAll").value = joinCsv(cfg.commands.clear_all);
    $("cmdUndo").value = joinCsv(cfg.commands.undo);
    $("cmdNewline").value = joinCsv(cfg.commands.newline);
    $("cmdMute").value = joinCsv(cfg.commands.mute);
    $("cmdResume").value = joinCsv(cfg.commands.resume);

    $("chkRemoveFillers").checked = !!cfg.filter.remove_fillers;
    $("chkAutoSpace").checked = !!cfg.filter.auto_space;
    $("chkStripPunct").checked = !!cfg.filter.strip_punctuation;
    $("chkAppendPeriod").checked = !!cfg.filter.append_period;
    $("fillersInput").value = joinCsv(cfg.filter.fillers);
    renderDictionary(cfg.filter.dictionary);

    $("chkGuardEnabled").checked = !!cfg.guard.enabled;
    $("chkGuardAutoFocus").checked = cfg.guard.auto_focus_discord !== false;
    $("chkGuardRestoreWindow").checked = cfg.guard.restore_previous_window !== false;
    $("allowedProcesses").value = joinCsv(cfg.guard.allowed_processes);
    $("clipboardDelay").value = cfg.guard.clipboard_restore_delay_ms;

    $("hkToggle").value = cfg.hotkeys.toggle_listening || "";
    $("hkPtt").value = cfg.hotkeys.push_to_talk || "";
    $("hkClear").value = cfg.hotkeys.clear_draft || "";
    $("hkOverlay").value = cfg.hotkeys.toggle_overlay || "";

    $("chkOverlayEnabled").checked = !!cfg.overlay.enabled;
    $("chkOverlayClickThrough").checked = !!cfg.overlay.click_through;
    $("chkSoundEnabled").checked = !!cfg.sound.enabled;
    $("soundVolume").value = cfg.sound.volume;
    $("soundVolVal").textContent = Math.round(cfg.sound.volume * 100) + "%";

    $("sttEngine").value = cfg.stt.engine || "auto";
    $("sttModelDir").value = cfg.stt.model_dir || "models";
    $("sttCliPath").value = cfg.stt.cli_path || "";
    $("sttLanguage").value = cfg.stt.language || "ja";
    $("vadSilenceMs").value = cfg.vad.silence_ms;
    $("vadMinSpeechMs").value = cfg.vad.min_speech_ms;
  }

  function collectConfigFromUi() {
    if (!currentConfig) return null;
    const dict = [];
    document.querySelectorAll("#dictRows .dict-row").forEach((row) => {
      const from = row.querySelector(".dict-from").value.trim();
      const to = row.querySelector(".dict-to").value;
      if (from) dict.push({ from, to });
    });

    const targets = getTargets();
    const validActive = targets.some((t) => t.id === activeTargetId) ? activeTargetId : null;

    const selectedDevice = $("audioDeviceSelect").value;
    const activeModeBtn = document.querySelector("#modeSegmented .seg-btn.active");
    const mode = activeModeBtn ? activeModeBtn.dataset.mode : currentConfig.input_mode;

    return {
      ...currentConfig,
      input_mode: mode,
      audio_device: selectedDevice ? selectedDevice : null,
      auto_start: $("chkAutoStart").checked,
      start_minimized: $("chkStartMinimized").checked,
      commands: {
        ...currentConfig.commands,
        send: splitCsv($("cmdSend").value),
        clear_all: splitCsv($("cmdClearAll").value),
        undo: splitCsv($("cmdUndo").value),
        newline: splitCsv($("cmdNewline").value),
        mute: splitCsv($("cmdMute").value),
        resume: splitCsv($("cmdResume").value),
      },
      filter: {
        dictionary: dict,
        fillers: splitCsv($("fillersInput").value),
        remove_fillers: $("chkRemoveFillers").checked,
        auto_space: $("chkAutoSpace").checked,
        strip_punctuation: $("chkStripPunct").checked,
        append_period: $("chkAppendPeriod").checked,
      },
      vad: {
        ...currentConfig.vad,
        threshold: parseFloat($("vadThreshold").value) || 0.025,
        silence_ms: parseInt($("vadSilenceMs").value, 10) || 320,
        min_speech_ms: parseInt($("vadMinSpeechMs").value, 10) || 140,
        auto_adjust: $("chkVadAutoAdjust").checked,
      },
      noise_cancel: {
        enabled: $("chkNcEnabled").checked,
        strength: parseFloat($("ncStrength").value),
        highpass_enabled: $("chkNcHighpass").checked,
        noise_gate_enabled: $("chkNcGate").checked,
        vad_prob_threshold: parseFloat($("ncVadProbThreshold").value) || 0.3,
        radio_clarity_mode: $("chkNcRadioClarity").checked,
      },
      discord_targets: {
        active_target_id: validActive,
        switch_prefixes: splitCsv($("targetPrefixes").value),
        switch_suffixes: splitCsv($("targetSuffixes").value),
        reset_phrases: splitCsv($("targetResets").value),
        allow_standalone_passphrase: $("chkTargetStandalone").checked,
        use_quick_switcher_for_unknown: $("chkTargetQuickSwitcher").checked,
        auto_navigate_on_switch: $("chkTargetAutoNavSwitch").checked,
        auto_navigate_on_send: $("chkTargetAutoNavSend").checked,
        navigate_delay_ms: parseInt($("targetNavDelay").value, 10) || 420,
        targets,
      },
      discord_integration: {
        bot_token: $("botTokenInput").value.trim(),
        use_bot_for_send: $("chkBotSend").checked,
        clipboard_url_watch: $("chkClipboardWatch").checked,
      },
      guard: {
        enabled: $("chkGuardEnabled").checked,
        auto_focus_discord: $("chkGuardAutoFocus").checked,
        restore_previous_window: $("chkGuardRestoreWindow").checked,
        allowed_processes: splitCsv($("allowedProcesses").value),
        clipboard_restore_delay_ms: parseInt($("clipboardDelay").value, 10) || 80,
      },
      hotkeys: {
        toggle_listening: $("hkToggle").value.trim(),
        push_to_talk: $("hkPtt").value.trim(),
        clear_draft: $("hkClear").value.trim(),
        toggle_overlay: $("hkOverlay").value.trim(),
      },
      overlay: {
        ...currentConfig.overlay,
        enabled: $("chkOverlayEnabled").checked,
        click_through: $("chkOverlayClickThrough").checked,
      },
      sound: {
        enabled: $("chkSoundEnabled").checked,
        volume: parseFloat($("soundVolume").value) || 0.45,
      },
      stt: {
        ...currentConfig.stt,
        engine: $("sttEngine").value,
        model_dir: $("sttModelDir").value.trim(),
        cli_path: $("sttCliPath").value.trim(),
        language: $("sttLanguage").value.trim() || "ja",
      },
    };
  }

  function saveNow() {
    const cfg = collectConfigFromUi();
    if (cfg) send({ type: "SaveConfig", config: cfg });
  }

  function updateModeButtons(mode) {
    document.querySelectorAll("#modeSegmented .seg-btn").forEach((b) => {
      b.classList.toggle("active", b.dataset.mode === mode);
    });
    $("modeHint").textContent =
      mode === "draft"
        ? "認識テキストを下書きに溜め、「送信して」の発話で一括ペースト＆送信します。"
        : "発話が確定するたびに、カーソル位置へ即座にペーストします。";
  }

  function updateThresholdMarker(th) {
    const pct = Math.min(100, Math.max(0, (th / 0.2) * 100));
    $("meterThreshold").style.left = pct + "%";
  }

  function addHistoryItem(text) {
    if (!text) return;
    const now = new Date();
    const time = now.toTimeString().slice(0, 8);
    history.unshift({ text, time });
    if (history.length > 50) history.pop();
    renderHistory();
  }

  function renderHistory() {
    const list = $("historyList");
    if (!history.length) {
      list.innerHTML = '<div class="empty-state">まだ認識履歴はありません</div>';
      return;
    }
    list.innerHTML = "";
    history.forEach((item) => {
      const div = document.createElement("div");
      div.className = "history-item";
      const spanText = document.createElement("span");
      spanText.textContent = item.text;
      const spanTime = document.createElement("span");
      spanTime.className = "history-time";
      spanTime.textContent = item.time;
      div.appendChild(spanText);
      div.appendChild(spanTime);
      div.addEventListener("click", () => {
        if (navigator.clipboard) {
          navigator.clipboard.writeText(item.text).then(() => {
            showToast("クリップボードにコピーしました", "ok");
          });
        }
      });
      list.appendChild(div);
    });
  }

  // Channel Form bindings
  $("btnChParseUrl").addEventListener("click", () => {
    const raw = $("chUrlPaste").value.trim();
    const m = raw.match(/channels\/(@me|\d+)\/(\d+)/i);
    if (!m) {
      showToast("Discord のチャンネルURL（.../channels/サーバーID/チャンネルID）を貼ってください", "error");
      return;
    }
    $("chGuildId").value = m[1].toLowerCase() === "@me" ? "" : m[1];
    $("chChannelId").value = m[2];
    showToast("URLからサーバーID・チャンネルIDを入力しました", "ok");
  });

  $("btnChCancelEdit").addEventListener("click", resetChannelForm);

  $("btnChSave").addEventListener("click", () => {
    const name = $("chName").value.trim();
    const channelId = $("chChannelId").value.trim();
    const webhook = $("chWebhook").value.trim();
    if (!name) {
      showToast("チャンネル名を入力してください", "error");
      $("chName").focus();
      return;
    }
    if (!channelId && !webhook) {
      showToast("チャンネルID または Webhook URL を入力してください", "error");
      $("chChannelId").focus();
      return;
    }
    const pass = splitCsv($("chPass").value);
    const entry = {
      id: editingChannelId || genId("ch"),
      name,
      server_name: $("chServerName").value.trim(),
      passphrases: pass.length ? pass : [name],
      kind: "channel",
      guild_id: $("chGuildId").value.trim(),
      channel_id: channelId,
      user_id: "",
      prepend_mention: false,
      webhook_url: webhook,
    };
    const list = getTargets();
    const idx = list.findIndex((t) => t.id === entry.id);
    if (idx >= 0) list[idx] = entry;
    else list.push(entry);
    resetChannelForm();
    saveTargetsList(list);
  });

  // Friend Form bindings
  $("btnFrParseUrl").addEventListener("click", () => {
    const raw = $("frUrlPaste").value.trim();
    const m = raw.match(/channels\/@me\/(\d+)/i) || raw.match(/channels\/\d+\/(\d+)/i);
    if (!m) {
      showToast("Discord DM のURL（.../channels/@me/DMチャンネルID）を貼ってください", "error");
      return;
    }
    $("frChannelId").value = m[1];
    showToast("URLからDMチャンネルIDを入力しました", "ok");
  });

  $("btnFrCancelEdit").addEventListener("click", resetFriendForm);

  $("btnFrSave").addEventListener("click", () => {
    const name = $("frName").value.trim();
    if (!name) {
      showToast("フレンド名を入力してください", "error");
      $("frName").focus();
      return;
    }
    const pass = splitCsv($("frPass").value);
    const kind = $("frKind").value === "mention" ? "mention" : "dm";
    const entry = {
      id: editingFriendId || genId("fr"),
      name,
      server_name: "",
      passphrases: pass.length ? pass : [name],
      kind,
      guild_id: "",
      channel_id: $("frChannelId").value.trim(),
      user_id: $("frUserId").value.trim(),
      prepend_mention: $("frPrependMention").checked || kind === "mention",
      webhook_url: "",
    };
    const list = getTargets();
    const idx = list.findIndex((t) => t.id === entry.id);
    if (idx >= 0) list[idx] = entry;
    else list.push(entry);
    resetFriendForm();
    saveTargetsList(list);
  });

  // Discord Integration & Remote Control bindings
  $("btnScanDiscord").addEventListener("click", () => {
    showToast("Discord 画面をスキャンしています...", "info");
    send({ type: "ScanDiscordUi" });
  });

  $("btnImportScan").addEventListener("click", () => {
    if (!lastScanData) return;
    const list = getTargets();
    let added = 0;
    (lastScanData.channels || []).forEach((ch) => {
      const exists = list.some(
        (t) =>
          t.kind === "channel" &&
          t.name === ch.name &&
          (t.server_name || "") === (ch.server_name || "")
      );
      if (!exists && ch.name) {
        list.push({
          id: genId("scan_ch"),
          name: ch.name,
          server_name: ch.server_name || lastScanData.active_server || "",
          passphrases: [ch.name],
          kind: "channel",
          guild_id: "",
          channel_id: "",
          user_id: "",
          prepend_mention: false,
          webhook_url: "",
        });
        added++;
      }
    });
    (lastScanData.friends || []).forEach((fr) => {
      const exists = list.some((t) => (t.kind === "dm" || t.kind === "mention") && t.name === fr);
      if (!exists && fr) {
        list.push({
          id: genId("scan_fr"),
          name: fr,
          server_name: "",
          passphrases: [fr],
          kind: "dm",
          guild_id: "",
          channel_id: "",
          user_id: "",
          prepend_mention: false,
          webhook_url: "",
        });
        added++;
      }
    });
    if (added > 0) {
      saveTargetsList(list);
      showToast(`${added}件のチャンネル／フレンドを取り込みました！`, "ok");
    } else {
      showToast("新しく追加できる項目はありませんでした（既に登録済み）", "info");
    }
  });

  $("chkClipboardWatch").addEventListener("change", saveNow);
  $("chkBotSend").addEventListener("change", saveNow);
  $("chkGuardAutoFocus").addEventListener("change", saveNow);
  $("chkGuardRestoreWindow").addEventListener("change", saveNow);

  $("btnSyncBot").addEventListener("click", () => {
    const token = $("botTokenInput").value.trim();
    if (!token) {
      showToast("公式 Discord Bot Token を入力してください", "error");
      return;
    }
    showToast("公式 Discord Bot から同期しています...", "info");
    send({ type: "SyncDiscordBot", bot_token: token });
  });

  document.querySelectorAll("[data-remote]").forEach((btn) => {
    btn.addEventListener("click", () => {
      const delaySec = parseInt($("remoteDelaySec").value, 10) || 0;
      send({
        type: "DiscordRemoteAction",
        action: btn.dataset.remote,
        delay_ms: delaySec * 1000,
      });
    });
  });

  $("btnUrlDismiss").addEventListener("click", () => {
    pendingDetectedUrl = null;
    $("urlDetectBanner").style.display = "none";
  });

  $("btnUrlApply").addEventListener("click", () => {
    if (!pendingDetectedUrl) return;
    const d = pendingDetectedUrl;
    $("urlDetectBanner").style.display = "none";
    pendingDetectedUrl = null;
    if (d.is_dm) {
      switchTab("friends");
      resetFriendForm();
      $("frKind").value = "dm";
      $("frChannelId").value = d.channel_id;
      $("frName").focus();
      showToast("DMチャンネルIDを自動入力しました！フレンド名を入れて登録してください。", "ok");
    } else {
      switchTab("channels");
      resetChannelForm();
      $("chGuildId").value = d.guild_id;
      $("chChannelId").value = d.channel_id;
      $("chName").focus();
      showToast("サーバーID・チャンネルIDを自動入力しました！チャンネル名を入れて登録してください。", "ok");
    }
  });

  // General bindings
  $("btnToggleListen").addEventListener("click", () => send({ type: "ToggleListening" }));
  $("btnToggleOverlay").addEventListener("click", () => send({ type: "ToggleOverlay" }));
  $("btnSaveAll").addEventListener("click", saveNow);
  $("chkAutoStart").addEventListener("change", saveNow);
  $("chkStartMinimized").addEventListener("change", saveNow);
  $("btnConfirmYes").addEventListener("click", () =>
    send({ type: "ConfirmTargetReply", reply: "yes" })
  );
  $("btnConfirmNext").addEventListener("click", () =>
    send({ type: "ConfirmTargetReply", reply: "next" })
  );
  $("btnConfirmCancel").addEventListener("click", () =>
    send({ type: "ConfirmTargetReply", reply: "cancel" })
  );

  document.querySelectorAll("#modeSegmented .seg-btn").forEach((btn) => {
    btn.addEventListener("click", () => {
      updateModeButtons(btn.dataset.mode);
      send({ type: "SetInputMode", mode: btn.dataset.mode });
    });
  });

  $("quickTargetSelect").addEventListener("change", (e) => {
    const id = e.target.value || null;
    activeTargetId = id;
    send({ type: "SelectDiscordTarget", target_id: id, navigate: !!id });
  });

  $("btnQuickNavTarget").addEventListener("click", () => {
    const id = $("quickTargetSelect").value;
    if (!id) {
      showToast("宛先が選択されていません", "info");
      return;
    }
    send({ type: "NavigateDiscordTarget", target_id: id });
  });

  $("vadThreshold").addEventListener("input", (e) => {
    const v = parseFloat(e.target.value);
    $("vadThresholdVal").textContent = v.toFixed(3);
    updateThresholdMarker(v);
  });
  $("vadThreshold").addEventListener("change", saveNow);

  $("chkNcEnabled").addEventListener("change", saveNow);
  $("chkNcRadioClarity").addEventListener("change", saveNow);
  $("chkNcHighpass").addEventListener("change", saveNow);
  $("chkNcGate").addEventListener("change", saveNow);
  $("ncStrength").addEventListener("input", (e) => {
    $("ncStrengthVal").textContent = Math.round(parseFloat(e.target.value) * 100) + "%";
  });
  $("ncStrength").addEventListener("change", saveNow);

  $("audioDeviceSelect").addEventListener("change", saveNow);
  $("btnRefreshDevices").addEventListener("click", () => send({ type: "ListAudioDevices" }));
  $("btnClearDraft").addEventListener("click", () => send({ type: "ClearDraft" }));
  $("btnForceSend").addEventListener("click", () => send({ type: "ForceSendDraft" }));

  $("draftBox").addEventListener("input", (e) => {
    send({ type: "UpdateDraft", draft: e.target.value });
  });

  $("btnSimulate").addEventListener("click", () => {
    const text = $("simInput").value.trim();
    if (!text) return;
    send({ type: "SimulateTranscript", text });
    $("simInput").value = "";
  });

  $("simInput").addEventListener("keydown", (e) => {
    if (e.key === "Enter") $("btnSimulate").click();
  });

  $("btnClearHistory").addEventListener("click", () => {
    history.length = 0;
    renderHistory();
  });

  $("btnAddDict").addEventListener("click", () => {
    const rows = document.querySelectorAll("#dictRows .dict-row");
    const list = [];
    rows.forEach((row) => {
      list.push({
        from: row.querySelector(".dict-from").value,
        to: row.querySelector(".dict-to").value,
      });
    });
    list.push({ from: "", to: "" });
    renderDictionary(list);
  });

  $("chkOverlayClickThrough").addEventListener("change", (e) => {
    send({ type: "SetOverlayClickThrough", click_through: e.target.checked });
  });

  $("soundVolume").addEventListener("input", (e) => {
    $("soundVolVal").textContent = Math.round(parseFloat(e.target.value) * 100) + "%";
  });

  document.querySelectorAll("[data-sound]").forEach((btn) => {
    btn.addEventListener("click", () => {
      send({ type: "PlayTestSound", kind: btn.dataset.sound });
    });
  });

  $("btnCalibrateVad").addEventListener("click", () => {
    send({ type: "CalibrateVad" });
  });
  $("chkVadAutoAdjust").addEventListener("change", () => {
    const cfg = collectConfigFromUi();
    if (cfg) send({ type: "SaveConfig", config: cfg });
  });

  $("btnCheckActive").addEventListener("click", () => send({ type: "GetActiveWindow" }));
  $("btnTestPaste").addEventListener("click", () => {
    const delaySec = parseInt($("testDelaySec").value, 10) || 0;
    send({
      type: "TestPaste",
      text: $("testPasteText").value,
      press_enter: false,
      delay_ms: delaySec * 1000,
    });
  });
  $("btnTestPasteSend").addEventListener("click", () => {
    const delaySec = parseInt($("testDelaySec").value, 10) || 0;
    send({
      type: "TestPaste",
      text: $("testPasteText").value,
      press_enter: true,
      delay_ms: delaySec * 1000,
    });
  });
  $("btnTestNewline").addEventListener("click", () => {
    const delaySec = parseInt($("testDelaySec").value, 10) || 0;
    send({ type: "TestKeyAction", action: "newline", delay_ms: delaySec * 1000 });
  });
  $("btnTestUndo").addEventListener("click", () => {
    const delaySec = parseInt($("testDelaySec").value, 10) || 0;
    send({ type: "TestKeyAction", action: "undo", delay_ms: delaySec * 1000 });
  });

  // Backend event handler
  window.onBackendEvent = function (ev) {
    if (!ev || !ev.type) return;
    switch (ev.type) {
      case "ConfigLoaded":
        applyConfigToUi(ev.config, ev.config_path);
        break;
      case "ConfigSaved":
        showToast(
          ev.message || (ev.ok ? "設定を保存しました" : "保存に失敗しました"),
          ev.ok ? "ok" : "error"
        );
        break;
      case "AudioLevel": {
        const pct = Math.min(100, Math.round((ev.rms / 0.2) * 100));
        $("meterBar").style.width = pct + "%";
        const badge = $("speakingBadge");
        badge.textContent = ev.is_speaking ? "発話検知" : "無音";
        badge.classList.toggle("active", !!ev.is_speaking);

        if ($("chkVadAutoAdjust").checked && ev.effective_threshold > 0) {
          updateThresholdMarker(ev.effective_threshold);
          $("vadThresholdVal").textContent = Number(ev.effective_threshold).toFixed(3) + " (自動)";
          $("vadNoiseFloorHint").textContent = `(ノイズ床: ${Number(ev.noise_floor || 0).toFixed(3)})`;
        } else {
          $("vadNoiseFloorHint").textContent = "";
        }

        const probPct = Math.min(100, Math.max(0, Math.round((ev.vad_prob || 0) * 100)));
        $("vadProbBar").style.width = probPct + "%";
        $("vadProbVal").textContent = probPct + "%";
        const ncLive = $("ncLiveBadge");
        ncLive.textContent = ev.rnnoise_active ? "RNNoise 稼働中" : "RNNoise OFF";
        ncLive.className = "pill " + (ev.rnnoise_active ? "ok" : "");
        break;
      }
      case "TranscriptUpdate":
        if (typeof ev.draft === "string") {
          $("draftBox").value = ev.draft;
        }
        if (ev.is_final && ev.text) {
          addHistoryItem(ev.text);
        }
        break;
      case "CommandExecuted":
        showToast("コマンド実行: " + ev.detail, "ok");
        break;
      case "ConfirmationPrompt": {
        const banner = $("confirmBanner");
        if (ev.active) {
          $("confirmTitle").textContent = ev.title || "";
          $("confirmSub").textContent = ev.subtitle || "";
          banner.style.display = "flex";
        } else {
          banner.style.display = "none";
        }
        break;
      }
      case "DiscordUrlDetected": {
        pendingDetectedUrl = ev;
        $("urlDetectTitle").textContent = ev.is_dm
          ? "Discord の DM リンクを検出しました"
          : "Discord のチャンネルリンクを検出しました";
        $("urlDetectSub").textContent = ev.is_dm
          ? `DMチャンネルID: ${ev.channel_id}`
          : `サーバーID: ${ev.guild_id} / チャンネルID: ${ev.channel_id}`;
        $("urlDetectBanner").style.display = "flex";
        break;
      }
      case "DiscordScanResult": {
        if (!ev.ok) {
          showToast(ev.message || "スキャンに失敗しました", "error");
          break;
        }
        lastScanData = ev;
        $("scanResultBox").style.display = "block";
        $("scanSummary").textContent = ev.message;
        const details = $("scanDetails");
        details.innerHTML = "";
        const sLine = document.createElement("div");
        sLine.textContent = "サーバー: " + ((ev.servers || []).join(", ") || "未検出");
        const cLine = document.createElement("div");
        cLine.textContent =
          "チャンネル: " + ((ev.channels || []).map((c) => "#" + c.name).join(", ") || "未検出");
        const fLine = document.createElement("div");
        fLine.textContent = "DMフレンド: " + ((ev.friends || []).join(", ") || "未検出");
        details.appendChild(sLine);
        details.appendChild(cLine);
        details.appendChild(fLine);
        showToast(ev.message, "ok");
        break;
      }
      case "DiscordBotSyncResult": {
        showToast(ev.message, ev.ok ? "ok" : "error");
        break;
      }
      case "StatusChanged": {
        const btn = $("btnToggleListen");
        btn.classList.remove("on", "off", "sleeping");
        if (!ev.listening) {
          btn.classList.add("off");
          $("listenLabel").textContent = "録音 OFF";
          $("pillListen").textContent = "停止中";
          $("pillListen").className = "pill";
        } else if (ev.sleeping) {
          btn.classList.add("sleeping");
          $("listenLabel").textContent = "スリープ中";
          $("pillListen").textContent = "音声スリープ";
          $("pillListen").className = "pill warn";
        } else if (ev.recognizing) {
          btn.classList.add("on");
          $("listenLabel").textContent = "認識中...";
          $("pillListen").textContent = "認識中";
          $("pillListen").className = "pill active";
        } else {
          btn.classList.add("on");
          $("listenLabel").textContent = "録音 ON";
          $("pillListen").textContent = "録音待機中";
          $("pillListen").className = "pill ok";
        }

        const pillRn = $("pillRnnoise");
        pillRn.textContent = ev.rnnoise_enabled ? "RNNoise: ON" : "RNNoise: OFF";
        pillRn.className = "pill " + (ev.rnnoise_enabled ? "ok" : "");

        activeTargetId = ev.active_discord_target_id || null;
        const tgtLabel = ev.active_discord_target_label || "現在の画面（未指定）";
        $("sidebarDiscordTarget").textContent = tgtLabel;
        const pillDt = $("pillDiscordTarget");
        pillDt.textContent = "宛先: " + tgtLabel;
        pillDt.className = "pill " + (activeTargetId ? "active" : "");
        renderAllTargets();

        const procName = ev.active_process || "未検出";
        $("sidebarTarget").textContent = procName;
        const pillTarget = $("pillTarget");
        pillTarget.textContent = ev.target_active
          ? "許可対象: " + procName
          : "非対象: " + procName;
        pillTarget.className = "pill " + (ev.target_active ? "ok" : "warn");

        const pillStt = $("pillStt");
        pillStt.textContent = "STT: " + ev.stt_engine + (ev.stt_ready ? " (準備完了)" : " (待機)");
        pillStt.className = "pill " + (ev.stt_ready ? "ok" : "");

        updateModeButtons(ev.input_mode);
        $("chkOverlayEnabled").checked = !!ev.overlay_visible;
        break;
      }
      case "AudioDevices": {
        const sel = $("audioDeviceSelect");
        const prev = ev.selected || "";
        sel.innerHTML = '<option value="">デフォルトマイク</option>';
        (ev.devices || []).forEach((name) => {
          const opt = document.createElement("option");
          opt.value = name;
          opt.textContent = name;
          sel.appendChild(opt);
        });
        sel.value = prev;
        break;
      }
      case "Toast":
        showToast(ev.message, ev.level);
        break;
    }
  };

  send({ type: "GetConfig" });
  send({ type: "ListAudioDevices" });
})();
