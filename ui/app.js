(function () {
  let currentConfig = null;
  let activeTargetId = null;
  let editingChannelId = null;
  let editingFriendId = null;
  let lastScanData = null;
  let pendingDetectedUrl = null;
  const history = [];
  const toastDebugLogs = [];
  let voicevoxSpeakers = [];
  let gameProfiles = [];
  let diagnosticsItems = [];
  let isPanicMode = false;
  let lastCatalogInventory = null;
  let benchmarkCases = [];
  const phraseEditors = {};

  function ensurePhraseEditor(id) {
    if (phraseEditors[id]) return phraseEditors[id];
    const root = $(id);
    if (!root) return null;
    const placeholder = root.dataset.placeholder || "新しいワードを入力";
    root.innerHTML = "";

    const chipsEl = document.createElement("div");
    chipsEl.className = "phrase-chips";

    const addRow = document.createElement("div");
    addRow.className = "phrase-add-row";
    const inp = document.createElement("input");
    inp.type = "text";
    inp.className = "input grow";
    inp.placeholder = placeholder;
    const addBtn = document.createElement("button");
    addBtn.type = "button";
    addBtn.className = "btn-secondary btn-sm";
    addBtn.textContent = "＋ 追加";

    const state = { items: [], chipsEl, inp };
    phraseEditors[id] = state;

    const commit = () => {
      const val = inp.value.trim();
      if (!val) return;
      if (!state.items.includes(val)) {
        state.items.push(val);
        renderPhraseEditor(id);
        saveNow();
      } else {
        showToast(`「${val}」はすでに登録されています`, "info");
      }
      inp.value = "";
      inp.focus();
    };

    addBtn.addEventListener("click", commit);
    inp.addEventListener("keydown", (e) => {
      if (e.key === "Enter") {
        e.preventDefault();
        commit();
      }
    });

    addRow.appendChild(inp);
    addRow.appendChild(addBtn);
    root.appendChild(chipsEl);
    root.appendChild(addRow);
    renderPhraseEditor(id);
    return state;
  }

  function renderPhraseEditor(id) {
    const st = phraseEditors[id];
    if (!st) return;
    st.chipsEl.innerHTML = "";
    if (!st.items.length) {
      const empty = document.createElement("span");
      empty.className = "phrase-empty";
      empty.textContent = "未登録（下の欄から1つずつ追加できます）";
      st.chipsEl.appendChild(empty);
      return;
    }
    st.items.forEach((word, idx) => {
      const chip = document.createElement("span");
      chip.className = "phrase-chip";
      const lbl = document.createElement("span");
      lbl.textContent = word;
      const del = document.createElement("button");
      del.type = "button";
      del.className = "phrase-chip-del";
      del.title = "削除";
      del.textContent = "×";
      del.addEventListener("click", () => {
        st.items.splice(idx, 1);
        renderPhraseEditor(id);
        saveNow();
      });
      chip.appendChild(lbl);
      chip.appendChild(del);
      st.chipsEl.appendChild(chip);
    });
  }

  function setPhraseList(id, arr) {
    const st = ensurePhraseEditor(id);
    if (!st) return;
    st.items = Array.isArray(arr)
      ? arr.map((x) => String(x).trim()).filter(Boolean)
      : [];
    renderPhraseEditor(id);
  }

  function getPhraseList(id) {
    const st = ensurePhraseEditor(id);
    if (!st) return [];
    const pending = st.inp.value.trim();
    if (pending && !st.items.includes(pending)) {
      st.items.push(pending);
      st.inp.value = "";
      renderPhraseEditor(id);
    }
    return [...st.items];
  }

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

  function renderPairList(containerId, items, fromKey, toKey, fromPh, toPh) {
    const wrap = $(containerId);
    if (!wrap) return;
    wrap.innerHTML = "";
    (items || []).forEach((item) => {
      const row = document.createElement("div");
      row.className = "dict-row";
      row.innerHTML = `
        <input type="text" class="input pair-from" placeholder="${fromPh}" />
        <span class="dict-arrow">→</span>
        <input type="text" class="input pair-to" placeholder="${toPh}" />
        <button type="button" class="btn-danger btn-sm pair-del">削除</button>
      `;
      row.querySelector(".pair-from").value = item[fromKey] || "";
      row.querySelector(".pair-to").value = item[toKey] || "";
      row.querySelector(".pair-del").addEventListener("click", () => row.remove());
      wrap.appendChild(row);
    });
  }

  function collectPairList(containerId, fromKey, toKey) {
    const out = [];
    document.querySelectorAll(`#${containerId} .dict-row`).forEach((row) => {
      const f = row.querySelector(".pair-from").value.trim();
      const t = row.querySelector(".pair-to").value;
      if (f) {
        const obj = {};
        obj[fromKey] = f;
        obj[toKey] = t;
        out.push(obj);
      }
    });
    return out;
  }

  function renderCarefulTargetsCheckboxes(selectedIds) {
    const box = $("carefulTargetsBox");
    if (!box) return;
    const set = new Set(selectedIds || []);
    const targets = getTargets();
    box.innerHTML = "";
    if (!targets.length) {
      box.innerHTML = '<span class="small-label">登録済みフレンド/チャンネルがありません</span>';
      return;
    }
    targets.forEach((t) => {
      const lbl = document.createElement("label");
      lbl.className = "checkbox-row small-check";
      const chk = document.createElement("input");
      chk.type = "checkbox";
      chk.className = "careful-target-chk";
      chk.value = t.id;
      chk.checked = set.has(t.id);
      const sp = document.createElement("span");
      sp.textContent = targetDisplayName(t);
      lbl.appendChild(chk);
      lbl.appendChild(sp);
      box.appendChild(lbl);
    });
  }

  function renderSpeechHabits(habits) {
    const box = $("speechHabitsList");
    if (!box) return;
    box.innerHTML = "";
    if (!habits || !habits.length) {
      box.innerHTML = '<div class="empty-state">まだ学習された言い直し癖はありません</div>';
      return;
    }
    habits.forEach((h, idx) => {
      const row = document.createElement("div");
      row.className = "dict-row";
      row.innerHTML = `
        <input type="text" class="input habit-from" />
        <span class="dict-arrow">→</span>
        <input type="text" class="input habit-to" />
        <button type="button" class="btn-danger btn-sm habit-del">削除</button>
      `;
      row.querySelector(".habit-from").value = h.from || "";
      row.querySelector(".habit-to").value = h.to || "";
      row.querySelector(".habit-del").addEventListener("click", () => {
        if (currentConfig && Array.isArray(currentConfig.speech_habits)) {
          currentConfig.speech_habits.splice(idx, 1);
          renderSpeechHabits(currentConfig.speech_habits);
          saveNow();
        }
      });
      box.appendChild(row);
    });
  }

  function applyConfigToUi(cfg, configPath) {
    currentConfig = cfg;
    activeTargetId = (cfg.discord_targets && cfg.discord_targets.active_target_id) || null;
    if (configPath) {
      $("configPathHint").textContent = "設定ファイル: " + configPath;
    }

    updateModeButtons(cfg.input_mode);
    updateActivationButtons(cfg.activation_mode || "vad");
    updatePresetButtons((cfg.stt && cfg.stt.preset) || "normal");
    $("vadThreshold").value = cfg.vad.threshold;
    $("vadThresholdVal").textContent = Number(cfg.vad.threshold).toFixed(3);
    $("chkVadAutoAdjust").checked = cfg.vad.auto_adjust !== false;
    updateThresholdMarker(cfg.vad.threshold);

    const nc = cfg.noise_cancel || {
      enabled: true,
      strength: 0.2,
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
    $("ncVadEngine").value = nc.vad_engine || "gtcrn-16k";
    $("ncSttEngine").value = nc.stt_engine || "dpdfnet-16k";
    const dpdfLim = Math.abs(nc.dpdfnet_attenuation_limit_db != null ? nc.dpdfnet_attenuation_limit_db : 12);
    $("ncDpdfLimit").value = dpdfLim;
    $("ncDpdfLimitVal").textContent = dpdfLim;
    const eqVal = nc.presence_eq_strength != null ? nc.presence_eq_strength : 0.55;
    $("ncPresenceEq").value = eqVal;
    $("ncPresenceEqVal").textContent = Math.round(eqVal * 100) + "%";

    const dt = cfg.discord_targets || {};
    setPhraseList("peTargetPrefixes", dt.switch_prefixes);
    setPhraseList("peTargetSuffixes", dt.switch_suffixes);
    setPhraseList("peTargetResets", dt.reset_phrases);
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

    setPhraseList("peCmdSend", cfg.commands.send);
    setPhraseList("peCmdClearAll", cfg.commands.clear_all);
    setPhraseList("peCmdUndo", cfg.commands.undo);
    setPhraseList("peCmdNewline", cfg.commands.newline);
    setPhraseList("peCmdMute", cfg.commands.mute);
    setPhraseList("peCmdResume", cfg.commands.resume);

    $("chkRemoveFillers").checked = !!cfg.filter.remove_fillers;
    $("chkAutoPunct").checked = cfg.filter.auto_punctuation !== false;
    $("chkAutoNewline").checked = cfg.filter.auto_newline !== false;
    $("chkQuickAutoNewline").checked = cfg.filter.auto_newline !== false;
    $("chkAutoSpace").checked = !!cfg.filter.auto_space;
    $("chkStripPunct").checked = !!cfg.filter.strip_punctuation;
    $("chkAppendPeriod").checked = !!cfg.filter.append_period;
    $("fillersInput").value = joinCsv(cfg.filter.fillers);
    renderDictionary(cfg.filter.dictionary);

    const acc = (cfg.stt && cfg.stt.accuracy) || {};
    $("chkStutterCleanup").checked = acc.stutter_cleanup !== false;
    $("chkPhoneticFuzzy").checked = acc.phonetic_fuzzy_matching !== false;
    $("chkAutoLearnRespeak").checked = acc.auto_learn_respeak !== false;
    $("chkWsolaEnabled").checked = acc.wsola_enabled !== false;
    $("chkWsolaAutoFast").checked = acc.wsola_auto_fast !== false;
    const wRate = acc.wsola_rate != null ? acc.wsola_rate : 0.84;
    $("wsolaRate").value = wRate;
    $("wsolaRateVal").textContent = Number(wRate).toFixed(2) + "x";
    $("headPadMs").value = acc.head_silence_pad_ms != null ? acc.head_silence_pad_ms : 220;
    $("tailPadMs").value = acc.tail_silence_pad_ms != null ? acc.tail_silence_pad_ms : 300;
    $("chkOverlapEnabled").checked = acc.overlap_chunking_enabled !== false;
    $("overlapThresholdSec").value = acc.overlap_threshold_sec != null ? acc.overlap_threshold_sec : 12;
    $("overlapSec").value = acc.overlap_sec != null ? acc.overlap_sec : 1.5;
    $("beamSize").value = acc.beam_size != null ? acc.beam_size : 3;
    $("chkTempFallback").checked = acc.temperature_fallback !== false;
    $("chkTwoPassRescue").checked = acc.two_pass_rescue !== false;
    $("chkUltimateEnsemble").checked = !!acc.ultimate_ensemble;
    $("secondaryEngine").value = acc.secondary_engine || "sherpa-qwen3";
    $("chkContextPrompt").checked = acc.context_prompt_injection !== false;
    $("chkStreamingPreview").checked = acc.streaming_preview_enabled !== false;
    $("streamingIntervalMs").value = acc.streaming_interval_ms != null ? acc.streaming_interval_ms : 1700;

    const llm = acc.llm || {};
    $("chkLlmEnabled").checked = !!llm.enabled;
    $("llmBaseUrl").value = llm.base_url || "http://127.0.0.1:8081";
    $("llmEndpointPath").value = llm.endpoint_path || "/v1/chat/completions";
    $("llmModel").value = llm.model || "qwen3-1.7b";
    $("llmStyle").value = llm.style || "normal";
    $("llmExePath").value = llm.server_exe_path || "";
    $("llmArgs").value = llm.server_args || "";
    $("llmTimeoutMs").value = llm.timeout_ms != null ? llm.timeout_ms : 1800;
    $("llmApiKey").value = llm.api_key || "";
    $("chkLlmAutoLaunch").checked = !!llm.auto_launch;
    $("chkLlmStopExit").checked = llm.stop_on_exit !== false;

    renderPairList("snippetRows", cfg.snippets || [], "trigger", "expansion", "呼び出しトリガー", "展開する定型文");
    renderPairList("emojiAliasRows", cfg.emoji_aliases || [], "spoken", "emoji", "発話ワード (例: くさ)", "絵文字 (例: 😂)");
    renderSpeechHabits(cfg.speech_habits || []);

    $("audioFallbacksInput").value = joinCsv(cfg.audio_device_fallbacks || []);
    const nm = cfg.night_mode || {};
    $("chkNightEnabled").checked = !!nm.enabled;
    $("nightStartHour").value = nm.start_hour != null ? nm.start_hour : 23;
    $("nightEndHour").value = nm.end_hour != null ? nm.end_hour : 6;
    $("nightTtsScale").value = nm.tts_volume_scale != null ? nm.tts_volume_scale : 0.45;
    const pe = cfg.power_eco || {};
    $("chkVramWatchdog").checked = pe.vram_watchdog_enabled !== false;
    $("vramThresholdPct").value = pe.vram_threshold_pct != null ? pe.vram_threshold_pct : 88;
    $("chkBatteryEco").checked = pe.auto_battery_eco !== false;
    $("ecoThreads").value = pe.eco_threads != null ? pe.eco_threads : 2;

    $("chkGuardEnabled").checked = !!cfg.guard.enabled;
    $("chkGuardAutoFocus").checked = cfg.guard.auto_focus_discord !== false;
    $("chkGuardRestoreWindow").checked = cfg.guard.restore_previous_window !== false;
    $("guardSendMethod").value = cfg.guard.send_method || "auto_paste";
    $("allowedProcesses").value = joinCsv(cfg.guard.allowed_processes);
    $("clipboardDelay").value = cfg.guard.clipboard_restore_delay_ms;
    $("chkTargetLocked").checked = !!cfg.guard.target_locked;
    renderCarefulTargetsCheckboxes(cfg.guard.careful_target_ids || []);

    $("hkToggle").value = cfg.hotkeys.toggle_listening || "";
    $("hkPtt").value = cfg.hotkeys.push_to_talk || "";
    $("hkClear").value = cfg.hotkeys.clear_draft || "";
    $("hkOverlay").value = cfg.hotkeys.toggle_overlay || "";
    $("hkPanicMute").value = cfg.hotkeys.panic_mute || "";

    $("chkOverlayEnabled").checked = !!cfg.overlay.enabled;
    $("chkOverlayClickThrough").checked = !!cfg.overlay.click_through;
    $("chkSoundEnabled").checked = !!cfg.sound.enabled;
    $("soundVolume").value = cfg.sound.volume;
    $("soundVolVal").textContent = Math.round(cfg.sound.volume * 100) + "%";

    $("sttEngine").value = cfg.stt.engine || "auto";
    $("sttModelFile").value = cfg.stt.model_file || "";
    $("sttModelDir").value = cfg.stt.model_dir || "models";
    $("sttCliPath").value = cfg.stt.cli_path || "";
    $("sttLanguage").value = cfg.stt.language || "ja";
    $("vadSilenceMs").value = cfg.vad.silence_ms;
    $("vadMinSpeechMs").value = cfg.vad.min_speech_ms;

    const ha = (cfg.stt && cfg.stt.http_asr) || {};
    $("httpAsrBaseUrl").value = ha.base_url || "http://127.0.0.1:8080";
    $("httpAsrEndpoint").value = ha.endpoint_path || "/v1/audio/transcriptions";
    $("httpAsrModel").value = ha.model || "qwen3-asr-1.7b";
    $("httpAsrApiKey").value = ha.api_key || "";
    $("chkHttpAsrAutoLaunch").checked = !!ha.auto_launch;
    $("httpAsrExePath").value = ha.server_exe_path || "";
    $("httpAsrArgs").value = ha.server_args || "";
    $("chkHttpAsrStopExit").checked = ha.stop_on_exit !== false;

    const sc = cfg.send_countdown || { enabled: false, duration_ms: 1500 };
    $("chkCountdownEnabled").checked = !!sc.enabled;
    $("countdownMs").value = sc.duration_ms != null ? sc.duration_ms : 1500;

    gameProfiles = Array.isArray(cfg.game_profiles)
      ? JSON.parse(JSON.stringify(cfg.game_profiles))
      : [];
    renderGameProfiles();

    const tts = cfg.discord_tts || {};
    const vv = tts.voicevox || {};
    $("chkTtsEnabled").checked = !!tts.enabled;
    $("chkTtsDmOnly").checked = tts.dm_only !== false;
    $("chkTtsReadWithoutWinNotif").checked = tts.read_without_windows_notifications !== false;
    $("chkTtsReadSender").checked = tts.read_sender_name !== false;
    $("chkTtsPauseStt").checked = tts.pause_stt_while_speaking !== false;
    $("chkTtsDebugLog").checked = !!tts.debug_log_toasts;
    $("ttsDebugCard").style.display = tts.debug_log_toasts ? "block" : "none";
    $("ttsTemplate").value = tts.template || "{sender}さんから。{message}";
    $("ttsMaxChars").value = tts.max_chars != null ? tts.max_chars : 80;
    $("ttsUrlReplace").value = tts.url_replacement || "URL省略";
    $("ttsDedupMs").value = tts.dedup_window_ms != null ? tts.dedup_window_ms : 3000;
    $("ttsMaxQueue").value = tts.max_queue_size != null ? tts.max_queue_size : 5;
    $("ttsEchoTailMs").value = tts.echo_guard_tail_ms != null ? tts.echo_guard_tail_ms : 400;
    $("ttsAllowSenders").value = joinCsv(tts.allow_senders);
    $("ttsBlockSenders").value = joinCsv(tts.block_senders);

    $("ttsBaseUrl").value = vv.base_url || "http://127.0.0.1:50021";
    $("ttsExePath").value = vv.exe_path || "";
    $("chkTtsAutoLaunch").checked = vv.auto_launch !== false;
    $("chkTtsStopOnExit").checked = vv.stop_on_exit !== false;
    const spkId = vv.speaker_id != null ? vv.speaker_id : 3;
    renderVoicevoxSpeakerSelect(spkId);
    const spd = vv.speed_scale != null ? vv.speed_scale : 1.15;
    const vol = vv.volume_scale != null ? vv.volume_scale : 1.0;
    const pit = vv.pitch_scale != null ? vv.pitch_scale : 0.0;
    const into = vv.intonation_scale != null ? vv.intonation_scale : 1.0;
    $("ttsSpeed").value = spd;
    $("ttsSpeedVal").textContent = Number(spd).toFixed(2);
    $("ttsVolume").value = vol;
    $("ttsVolumeVal").textContent = Number(vol).toFixed(2);
    $("ttsPitch").value = pit;
    $("ttsPitchVal").textContent = Number(pit).toFixed(2);
    $("ttsIntonation").value = into;
    $("ttsIntonationVal").textContent = Number(into).toFixed(2);
  }

  function renderVoicevoxSpeakerSelect(selectedId) {
    const sel = $("ttsSpeakerSelect");
    const targetId = String(selectedId != null ? selectedId : 3);
    sel.innerHTML = "";
    if (voicevoxSpeakers.length > 0) {
      let matched = false;
      voicevoxSpeakers.forEach((sp) => {
        const opt = document.createElement("option");
        opt.value = String(sp.id);
        opt.textContent = `${sp.name}（${sp.style_name}） [ID: ${sp.id}]`;
        if (opt.value === targetId) matched = true;
        sel.appendChild(opt);
      });
      if (!matched) {
        const opt = document.createElement("option");
        opt.value = targetId;
        opt.textContent = `カスタム話者 [ID: ${targetId}]`;
        sel.appendChild(opt);
      }
    } else {
      const opt = document.createElement("option");
      opt.value = targetId;
      opt.textContent = `話者 ID: ${targetId}（「接続確認 / 話者更新」で一覧取得）`;
      sel.appendChild(opt);
    }
    sel.value = targetId;
    $("ttsSpeakerIdHint").textContent = `ID: ${targetId}`;
  }

  function addToastDebugLog(text) {
    if (!text) return;
    const time = new Date().toTimeString().slice(0, 8);
    toastDebugLogs.unshift({ text, time });
    if (toastDebugLogs.length > 40) toastDebugLogs.pop();
    const list = $("ttsDebugList");
    list.innerHTML = "";
    toastDebugLogs.forEach((item) => {
      const div = document.createElement("div");
      div.className = "history-item";
      const sp = document.createElement("span");
      sp.textContent = item.text;
      const tm = document.createElement("span");
      tm.className = "history-time";
      tm.textContent = item.time;
      div.appendChild(sp);
      div.appendChild(tm);
      list.appendChild(div);
    });
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
    const activeActBtn = document.querySelector("#activationSegmented .seg-btn.active");
    const activationMode = activeActBtn
      ? activeActBtn.dataset.activation
      : currentConfig.activation_mode || "vad";

    return {
      ...currentConfig,
      input_mode: mode,
      activation_mode: activationMode,
      audio_device: selectedDevice ? selectedDevice : null,
      auto_start: $("chkAutoStart").checked,
      start_minimized: $("chkStartMinimized").checked,
      send_countdown: {
        enabled: $("chkCountdownEnabled").checked,
        duration_ms: Math.max(300, Math.min(5000, parseInt($("countdownMs").value, 10) || 1500)),
      },
      game_profiles: gameProfiles,
      commands: {
        ...currentConfig.commands,
        send: getPhraseList("peCmdSend"),
        clear_all: getPhraseList("peCmdClearAll"),
        undo: getPhraseList("peCmdUndo"),
        newline: getPhraseList("peCmdNewline"),
        mute: getPhraseList("peCmdMute"),
        resume: getPhraseList("peCmdResume"),
      },
      filter: {
        dictionary: dict,
        fillers: splitCsv($("fillersInput").value),
        remove_fillers: $("chkRemoveFillers").checked,
        auto_space: $("chkAutoSpace").checked,
        strip_punctuation: $("chkStripPunct").checked,
        append_period: $("chkAppendPeriod").checked,
        auto_punctuation: $("chkAutoPunct").checked,
        auto_newline: $("chkAutoNewline").checked,
      },
      vad: {
        ...currentConfig.vad,
        threshold: parseFloat($("vadThreshold").value) || 0.012,
        silence_ms: parseInt($("vadSilenceMs").value, 10) || 620,
        min_speech_ms: parseInt($("vadMinSpeechMs").value, 10) || 100,
        auto_adjust: $("chkVadAutoAdjust").checked,
      },
      noise_cancel: {
        enabled: $("chkNcEnabled").checked,
        strength: parseFloat($("ncStrength").value),
        highpass_enabled: $("chkNcHighpass").checked,
        noise_gate_enabled: $("chkNcGate").checked,
        vad_prob_threshold: parseFloat($("ncVadProbThreshold").value) || 0.3,
        radio_clarity_mode: $("chkNcRadioClarity").checked,
        vad_engine: $("ncVadEngine").value || "gtcrn-16k",
        stt_engine: $("ncSttEngine").value || "dpdfnet-16k",
        dpdfnet_attenuation_limit_db: Math.abs(parseFloat($("ncDpdfLimit").value) || 12),
        presence_eq_strength: parseFloat($("ncPresenceEq").value) || 0.55,
      },
      audio_device_fallbacks: splitCsv($("audioFallbacksInput").value),
      snippets: collectPairList("snippetRows", "trigger", "expansion").map((s, i) => ({
        id: "snip_" + i,
        trigger: s.trigger,
        expansion: s.expansion,
      })),
      emoji_aliases: collectPairList("emojiAliasRows", "spoken", "emoji"),
      speech_habits: (() => {
        const list = [];
        document.querySelectorAll("#speechHabitsList .dict-row").forEach((r) => {
          const from = r.querySelector(".habit-from").value.trim();
          const to = r.querySelector(".habit-to").value.trim();
          if (from && to) list.push({ from, to });
        });
        return list;
      })(),
      night_mode: {
        enabled: $("chkNightEnabled").checked,
        start_hour: Math.max(0, Math.min(23, parseInt($("nightStartHour").value, 10) || 23)),
        end_hour: Math.max(0, Math.min(23, parseInt($("nightEndHour").value, 10) || 6)),
        tts_volume_scale: Math.max(0.1, Math.min(1.0, parseFloat($("nightTtsScale").value) || 0.45)),
      },
      power_eco: {
        vram_watchdog_enabled: $("chkVramWatchdog").checked,
        vram_threshold_pct: Math.max(50, Math.min(99, parseInt($("vramThresholdPct").value, 10) || 88)),
        auto_battery_eco: $("chkBatteryEco").checked,
        eco_threads: Math.max(1, Math.min(16, parseInt($("ecoThreads").value, 10) || 2)),
      },
      discord_targets: {
        active_target_id: validActive,
        switch_prefixes: getPhraseList("peTargetPrefixes"),
        switch_suffixes: getPhraseList("peTargetSuffixes"),
        reset_phrases: getPhraseList("peTargetResets"),
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
      discord_tts: {
        enabled: $("chkTtsEnabled").checked,
        dm_only: $("chkTtsDmOnly").checked,
        read_without_windows_notifications: $("chkTtsReadWithoutWinNotif").checked,
        read_sender_name: $("chkTtsReadSender").checked,
        template: $("ttsTemplate").value || "{sender}さんから。{message}",
        max_chars: parseInt($("ttsMaxChars").value, 10) || 80,
        url_replacement: $("ttsUrlReplace").value || "URL省略",
        dedup_window_ms: Math.max(0, parseInt($("ttsDedupMs").value, 10) || 0),
        max_queue_size: Math.max(1, parseInt($("ttsMaxQueue").value, 10) || 5),
        pause_stt_while_speaking: $("chkTtsPauseStt").checked,
        echo_guard_tail_ms: Math.max(0, parseInt($("ttsEchoTailMs").value, 10) || 0),
        allow_senders: splitCsv($("ttsAllowSenders").value),
        block_senders: splitCsv($("ttsBlockSenders").value),
        debug_log_toasts: $("chkTtsDebugLog").checked,
        voicevox: {
          base_url: $("ttsBaseUrl").value.trim() || "http://127.0.0.1:50021",
          speaker_id: parseInt($("ttsSpeakerSelect").value, 10) || 0,
          speed_scale: parseFloat($("ttsSpeed").value) || 1.15,
          pitch_scale: parseFloat($("ttsPitch").value) || 0.0,
          intonation_scale: parseFloat($("ttsIntonation").value) || 1.0,
          volume_scale: parseFloat($("ttsVolume").value) || 1.0,
          auto_launch: $("chkTtsAutoLaunch").checked,
          exe_path: $("ttsExePath").value.trim(),
          stop_on_exit: $("chkTtsStopOnExit").checked,
        },
      },
      guard: {
        enabled: $("chkGuardEnabled").checked,
        auto_focus_discord: $("chkGuardAutoFocus").checked,
        restore_previous_window: $("chkGuardRestoreWindow").checked,
        send_method: $("guardSendMethod").value || "auto_paste",
        allowed_processes: splitCsv($("allowedProcesses").value),
        clipboard_restore_delay_ms: parseInt($("clipboardDelay").value, 10) || 80,
        target_locked: $("chkTargetLocked").checked,
        careful_target_ids: Array.from(document.querySelectorAll(".careful-target-chk:checked")).map((c) => c.value),
      },
      hotkeys: {
        toggle_listening: $("hkToggle").value.trim(),
        push_to_talk: $("hkPtt").value.trim(),
        clear_draft: $("hkClear").value.trim(),
        toggle_overlay: $("hkOverlay").value.trim(),
        panic_mute: $("hkPanicMute").value.trim(),
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
        model_file: $("sttModelFile").value || "",
        model_dir: $("sttModelDir").value.trim(),
        cli_path: $("sttCliPath").value.trim(),
        language: $("sttLanguage").value.trim() || "ja",
        preset: (currentConfig.stt && currentConfig.stt.preset) || "normal",
        accuracy: {
          wsola_enabled: $("chkWsolaEnabled").checked,
          wsola_rate: parseFloat($("wsolaRate").value) || 0.84,
          wsola_auto_fast: $("chkWsolaAutoFast").checked,
          head_silence_pad_ms: parseInt($("headPadMs").value, 10) || 220,
          tail_silence_pad_ms: parseInt($("tailPadMs").value, 10) || 300,
          overlap_chunking_enabled: $("chkOverlapEnabled").checked,
          overlap_threshold_sec: parseFloat($("overlapThresholdSec").value) || 12,
          overlap_sec: parseFloat($("overlapSec").value) || 1.5,
          beam_size: Math.max(1, Math.min(5, parseInt($("beamSize").value, 10) || 3)),
          temperature_fallback: $("chkTempFallback").checked,
          two_pass_rescue: $("chkTwoPassRescue").checked,
          ultimate_ensemble: $("chkUltimateEnsemble").checked,
          secondary_engine: $("secondaryEngine").value || "sherpa-qwen3",
          context_prompt_injection: $("chkContextPrompt").checked,
          streaming_preview_enabled: $("chkStreamingPreview").checked,
          streaming_interval_ms: parseInt($("streamingIntervalMs").value, 10) || 1700,
          phonetic_fuzzy_matching: $("chkPhoneticFuzzy").checked,
          stutter_cleanup: $("chkStutterCleanup").checked,
          auto_learn_respeak: $("chkAutoLearnRespeak").checked,
          llm: {
            enabled: $("chkLlmEnabled").checked,
            base_url: $("llmBaseUrl").value.trim() || "http://127.0.0.1:8081",
            endpoint_path: $("llmEndpointPath").value.trim() || "/v1/chat/completions",
            model: $("llmModel").value.trim() || "qwen3-1.7b",
            model_file: ((currentConfig.stt && currentConfig.stt.accuracy && currentConfig.stt.accuracy.llm && currentConfig.stt.accuracy.llm.model_file) || ""),
            api_key: $("llmApiKey").value.trim(),
            server_exe_path: $("llmExePath").value.trim(),
            server_args: $("llmArgs").value.trim(),
            auto_launch: $("chkLlmAutoLaunch").checked,
            stop_on_exit: $("chkLlmStopExit").checked,
            timeout_ms: parseInt($("llmTimeoutMs").value, 10) || 1800,
            style: $("llmStyle").value || "normal",
          },
        },
        http_asr: {
          base_url: $("httpAsrBaseUrl").value.trim() || "http://127.0.0.1:8080",
          endpoint_path: $("httpAsrEndpoint").value.trim() || "/v1/audio/transcriptions",
          model: $("httpAsrModel").value.trim() || "qwen3-asr-1.7b",
          api_key: $("httpAsrApiKey").value.trim(),
          auto_launch: $("chkHttpAsrAutoLaunch").checked,
          server_exe_path: $("httpAsrExePath").value.trim(),
          server_args: $("httpAsrArgs").value.trim(),
          stop_on_exit: $("chkHttpAsrStopExit").checked,
        },
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

  function updateActivationButtons(mode) {
    const val = mode || "vad";
    document.querySelectorAll("#activationSegmented .seg-btn").forEach((b) => {
      b.classList.toggle("active", b.dataset.activation === val);
    });
  }

  function updatePresetButtons(preset) {
    const val = preset || "normal";
    document.querySelectorAll("[data-vad-preset]").forEach((b) => {
      b.classList.toggle("active", b.dataset.vadPreset === val);
    });
  }

  function updateThresholdMarker(th) {
    const pct = Math.min(100, Math.max(0, (th / 0.2) * 100));
    $("meterThreshold").style.left = pct + "%";
  }

  function buildQuickLearnRow(initialFrom, onSubmitted) {
    const form = document.createElement("div");
    form.className = "quick-learn-row";
    form.style.display = "none";
    form.addEventListener("click", (e) => e.stopPropagation());

    const fromInp = document.createElement("input");
    fromInp.type = "text";
    fromInp.className = "input";
    fromInp.style.width = "160px";
    fromInp.placeholder = "誤認識ワード";
    fromInp.value = initialFrom || "";

    const arrow = document.createElement("span");
    arrow.className = "small-label";
    arrow.textContent = "→";

    const toInp = document.createElement("input");
    toInp.type = "text";
    toInp.className = "input";
    toInp.style.width = "160px";
    toInp.placeholder = "正しい表記";

    const saveBtn = document.createElement("button");
    saveBtn.type = "button";
    saveBtn.className = "btn-success btn-sm";
    saveBtn.textContent = "辞書に登録";
    saveBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      const from = fromInp.value.trim();
      const to = toInp.value.trim();
      if (!from || !to) {
        showToast("誤認識ワードと正しい表記の両方を入力してください", "error");
        return;
      }
      send({ type: "QuickLearnCorrection", from, to });
      form.style.display = "none";
      if (onSubmitted) onSubmitted(from, to);
    });

    form.appendChild(fromInp);
    form.appendChild(arrow);
    form.appendChild(toInp);
    form.appendChild(saveBtn);
    return { form, fromInp, toInp };
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
    const historyCard = $("historyList");
    if (historyCard) {
      historyCard.classList.toggle("panic-masked", isPanicMode);
    }
    const list = $("historyList");
    if (!history.length) {
      list.innerHTML = '<div class="empty-state">まだ認識履歴はありません</div>';
      return;
    }
    list.innerHTML = "";
    history.forEach((item) => {
      const div = document.createElement("div");
      div.className = "history-item";
      div.style.flexDirection = "column";
      div.style.alignItems = "stretch";

      const topRow = document.createElement("div");
      topRow.className = "row space-between";
      const spanText = document.createElement("span");
      spanText.className = "grow";
      spanText.textContent = item.text;
      const actions = document.createElement("div");
      actions.className = "row gap-sm";
      const learnBtn = document.createElement("button");
      learnBtn.type = "button";
      learnBtn.className = "btn-secondary btn-sm";
      learnBtn.textContent = "修正＆辞書登録";
      const spanTime = document.createElement("span");
      spanTime.className = "history-time";
      spanTime.textContent = item.time;
      actions.appendChild(learnBtn);
      actions.appendChild(spanTime);
      topRow.appendChild(spanText);
      topRow.appendChild(actions);

      const { form, toInp } = buildQuickLearnRow(item.text);
      learnBtn.addEventListener("click", (e) => {
        e.stopPropagation();
        const showing = form.style.display !== "none";
        form.style.display = showing ? "none" : "flex";
        if (!showing) toInp.focus();
      });

      div.appendChild(topRow);
      div.appendChild(form);
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

  function renderDiagnostics(items) {
    const list = $("diagnosticsList");
    if (!items || !items.length) {
      list.innerHTML =
        '<div class="empty-state">まだ診断データがありません。発話するとここに録音時間・ピークdBFS・推論時間・RTFが表示されます。</div>';
      return;
    }
    list.innerHTML = "";
    items.forEach((item) => {
      const card = document.createElement("div");
      card.className = "diag-item";

      const stats = document.createElement("div");
      stats.className = "diag-stats";
      [
        item.timestamp,
        `長さ: ${Number(item.duration_sec || 0).toFixed(2)}s`,
        `ピーク: ${Number(item.peak_dbfs || 0).toFixed(1)} dBFS`,
        `推論: ${item.infer_ms}ms`,
        `RTF: ${Number(item.rtf || 0).toFixed(2)}`,
        `デバイス: ${item.device_name || "-"}`,
        `エンジン: ${item.engine_label || "-"}`,
      ].forEach((txt) => {
        const sp = document.createElement("span");
        sp.className = "pill";
        sp.textContent = txt;
        stats.appendChild(sp);
      });

      const rawRow = document.createElement("div");
      rawRow.className = "diag-text-row";
      rawRow.textContent = `生認識: ${item.raw_text || "(空)"}`;

      const finalRow = document.createElement("div");
      finalRow.className = "diag-text-row";
      finalRow.textContent = `整形後: ${item.final_text || "(なし)"}`;

      const btnRow = document.createElement("div");
      btnRow.className = "row gap-sm";
      btnRow.style.marginTop = "6px";
      btnRow.style.flexWrap = "wrap";

      const playBtn = document.createElement("button");
      playBtn.type = "button";
      playBtn.className = "btn-secondary btn-sm";
      playBtn.textContent = "▶ 再生";
      playBtn.addEventListener("click", () => {
        send({ type: "ReplayUtterance", id: item.id });
      });

      const abBtn = document.createElement("button");
      abBtn.type = "button";
      abBtn.className = "btn-primary btn-sm";
      abBtn.textContent = "⚖ A/B再推論";
      abBtn.addEventListener("click", () => {
        const resBox = $("ab-result-" + item.id);
        if (resBox) {
          resBox.className = "ab-result show";
          resBox.textContent = "A/B 再推論を実行中...";
        }
        send({
          type: "AbCompareUtterance",
          id: item.id,
          engine: $("abEngine").value,
          model_file: $("abModelFile").value || "",
        });
      });

      const learnBtn = document.createElement("button");
      learnBtn.type = "button";
      learnBtn.className = "btn-secondary btn-sm";
      learnBtn.textContent = "誤認識を学習";

      const { form, toInp } = buildQuickLearnRow(item.raw_text || item.final_text || "");
      learnBtn.addEventListener("click", () => {
        const showing = form.style.display !== "none";
        form.style.display = showing ? "none" : "flex";
        if (!showing) toInp.focus();
      });

      const benchBtn = document.createElement("button");
      benchBtn.type = "button";
      benchBtn.className = "btn-secondary btn-sm";
      benchBtn.textContent = "ベンチ保存";
      benchBtn.addEventListener("click", () => {
        const exp = window.prompt("正解テキスト（期待する文字起こし結果）を入力してください:", item.final_text || item.raw_text || "");
        if (exp && exp.trim()) {
          send({ type: "SaveBenchmarkCase", diag_id: item.id, expected_text: exp.trim() });
        }
      });

      const abResult = document.createElement("div");
      abResult.id = "ab-result-" + item.id;
      abResult.className = "ab-result";

      btnRow.appendChild(playBtn);
      btnRow.appendChild(abBtn);
      btnRow.appendChild(learnBtn);
      btnRow.appendChild(benchBtn);

      card.appendChild(stats);
      card.appendChild(rawRow);
      card.appendChild(finalRow);
      card.appendChild(btnRow);
      card.appendChild(form);
      card.appendChild(abResult);
      list.appendChild(card);
    });
  }

  function formatProfileDictText(dict) {
    return (dict || []).map((d) => `${d.from}=${d.to}`).join("\n");
  }

  function parseProfileDictText(txt) {
    return (txt || "")
      .split(/\r?\n/)
      .map((line) => line.trim())
      .filter(Boolean)
      .map((line) => {
        const idx = line.indexOf("=");
        if (idx <= 0) return null;
        return {
          from: line.slice(0, idx).trim(),
          to: line.slice(idx + 1).trim(),
        };
      })
      .filter((x) => x && x.from);
  }

  function resetGameProfileForm() {
    $("gpEditId").value = "";
    $("gpName").value = "";
    $("gpProcesses").value = "";
    $("gpSendMethod").value = "";
    $("gpSpeechStyle").value = "";
    $("gpOverlayX").value = "";
    $("gpOverlayY").value = "";
    $("gpOverlayOpacity").value = "";
    $("gpDictText").value = "";
    $("btnGpSave").textContent = "プロファイル保存";
  }

  function renderGameProfiles() {
    const list = $("gameProfileList");
    if (!gameProfiles.length) {
      list.innerHTML = '<div class="empty-state">ゲームプロファイルはまだ登録されていません</div>';
      return;
    }
    list.innerHTML = "";
    gameProfiles.forEach((gp) => {
      const card = document.createElement("div");
      card.className = "profile-item row space-between";

      const info = document.createElement("div");
      const title = document.createElement("strong");
      title.textContent = gp.name || "(無名)";
      const sub = document.createElement("div");
      sub.className = "small-label";
      sub.textContent = `プロセス: ${(gp.process_names || []).join(", ") || "-"} | 辞書: ${(gp.dictionary || []).length}件${gp.send_method ? " | 送信: " + gp.send_method : ""}`;
      info.appendChild(title);
      info.appendChild(sub);

      const btns = document.createElement("div");
      btns.className = "row gap-sm";

      const chk = document.createElement("input");
      chk.type = "checkbox";
      chk.checked = gp.enabled !== false;
      chk.title = "有効/無効";
      chk.addEventListener("change", () => {
        gp.enabled = chk.checked;
        saveNow();
      });

      const editBtn = document.createElement("button");
      editBtn.type = "button";
      editBtn.className = "btn-secondary btn-sm";
      editBtn.textContent = "編集";
      editBtn.addEventListener("click", () => {
        $("gpEditId").value = gp.id;
        $("gpName").value = gp.name || "";
        $("gpProcesses").value = (gp.process_names || []).join(", ");
        $("gpSendMethod").value = gp.send_method || "";
        $("gpSpeechStyle").value = gp.speech_style || "";
        $("gpOverlayX").value = gp.overlay_x != null ? gp.overlay_x : "";
        $("gpOverlayY").value = gp.overlay_y != null ? gp.overlay_y : "";
        $("gpOverlayOpacity").value = gp.overlay_opacity != null ? gp.overlay_opacity : "";
        $("gpDictText").value = formatProfileDictText(gp.dictionary);
        $("btnGpSave").textContent = "更新保存";
      });

      const delBtn = document.createElement("button");
      delBtn.type = "button";
      delBtn.className = "btn-danger btn-sm";
      delBtn.textContent = "削除";
      delBtn.addEventListener("click", () => {
        gameProfiles = gameProfiles.filter((x) => x.id !== gp.id);
        renderGameProfiles();
        saveNow();
      });

      btns.appendChild(chk);
      btns.appendChild(editBtn);
      btns.appendChild(delBtn);
      card.appendChild(info);
      card.appendChild(btns);
      list.appendChild(card);
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
  $("guardSendMethod").addEventListener("change", saveNow);

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
  $("chkAutoPunct").addEventListener("change", saveNow);
  $("chkAutoNewline").addEventListener("change", () => {
    $("chkQuickAutoNewline").checked = $("chkAutoNewline").checked;
    saveNow();
  });
  $("chkQuickAutoNewline").addEventListener("change", () => {
    $("chkAutoNewline").checked = $("chkQuickAutoNewline").checked;
    saveNow();
  });
  $("chkStripPunct").addEventListener("change", saveNow);
  $("chkAppendPeriod").addEventListener("change", saveNow);

  // Discord DM TTS (VOICEVOX) bindings
  $("chkTtsEnabled").addEventListener("change", saveNow);
  $("chkTtsAutoLaunch").addEventListener("change", saveNow);
  $("chkTtsStopOnExit").addEventListener("change", saveNow);
  $("btnVoicevoxDetect").addEventListener("click", () => {
    send({ type: "VoicevoxDetectPath" });
  });
  $("chkTtsDmOnly").addEventListener("change", saveNow);
  $("chkTtsReadWithoutWinNotif").addEventListener("change", saveNow);
  $("chkTtsReadSender").addEventListener("change", saveNow);
  $("chkTtsPauseStt").addEventListener("change", saveNow);
  $("chkTtsDebugLog").addEventListener("change", () => {
    $("ttsDebugCard").style.display = $("chkTtsDebugLog").checked ? "block" : "none";
    saveNow();
  });
  $("ttsSpeakerSelect").addEventListener("change", () => {
    $("ttsSpeakerIdHint").textContent = "ID: " + $("ttsSpeakerSelect").value;
    saveNow();
  });
  $("ttsSpeed").addEventListener("input", (e) => {
    $("ttsSpeedVal").textContent = Number(e.target.value).toFixed(2);
  });
  $("ttsSpeed").addEventListener("change", saveNow);
  $("ttsVolume").addEventListener("input", (e) => {
    $("ttsVolumeVal").textContent = Number(e.target.value).toFixed(2);
  });
  $("ttsVolume").addEventListener("change", saveNow);
  $("ttsPitch").addEventListener("input", (e) => {
    $("ttsPitchVal").textContent = Number(e.target.value).toFixed(2);
  });
  $("ttsPitch").addEventListener("change", saveNow);
  $("ttsIntonation").addEventListener("input", (e) => {
    $("ttsIntonationVal").textContent = Number(e.target.value).toFixed(2);
  });
  $("ttsIntonation").addEventListener("change", saveNow);

  $("btnVoicevoxRefresh").addEventListener("click", () => {
    const baseUrl = $("ttsBaseUrl").value.trim() || "http://127.0.0.1:50021";
    saveNow();
    send({ type: "VoicevoxCheckHealth", base_url: baseUrl });
    send({ type: "VoicevoxGetSpeakers", base_url: baseUrl });
  });
  $("btnTtsTestSpeak").addEventListener("click", () => {
    saveNow();
    send({ type: "VoicevoxTestSpeak", text: $("ttsTestText").value });
  });
  $("btnTtsStop").addEventListener("click", () => {
    send({ type: "TtsStop" });
  });
  $("btnClearToastDebug").addEventListener("click", () => {
    toastDebugLogs.length = 0;
    $("ttsDebugList").innerHTML = '<div class="empty-state">通知を待機しています...</div>';
  });
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
  $("ncVadEngine").addEventListener("change", saveNow);
  $("ncSttEngine").addEventListener("change", saveNow);
  $("ncDpdfLimit").addEventListener("input", (e) => {
    $("ncDpdfLimitVal").textContent = e.target.value;
  });
  $("ncDpdfLimit").addEventListener("change", saveNow);
  $("ncPresenceEq").addEventListener("input", (e) => {
    $("ncPresenceEqVal").textContent = Math.round(parseFloat(e.target.value) * 100) + "%";
  });
  $("ncPresenceEq").addEventListener("change", saveNow);
  $("wsolaRate").addEventListener("input", (e) => {
    $("wsolaRateVal").textContent = Number(e.target.value).toFixed(2) + "x";
  });
  $("wsolaRate").addEventListener("change", saveNow);
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

  $("btnAddSnippet").addEventListener("click", () => {
    const list = collectPairList("snippetRows", "trigger", "expansion");
    list.push({ trigger: "", expansion: "" });
    renderPairList("snippetRows", list, "trigger", "expansion", "呼び出しトリガー", "展開する定型文");
  });
  $("btnAddEmojiAlias").addEventListener("click", () => {
    const list = collectPairList("emojiAliasRows", "spoken", "emoji");
    list.push({ spoken: "", emoji: "" });
    renderPairList("emojiAliasRows", list, "spoken", "emoji", "発話ワード (例: くさ)", "絵文字 (例: 😂)");
  });

  $("btnTargetLock").addEventListener("click", () => send({ type: "ToggleTargetLock" }));
  $("chkTargetLocked").addEventListener("change", saveNow);
  $("btnPanicMute").addEventListener("click", () => send({ type: "TogglePanicSafeMode" }));
  $("btnDismissUpdate").addEventListener("click", () => {
    $("updateBanner").style.display = "none";
  });

  $("btnTestLlm").addEventListener("click", () => {
    send({ type: "TestLlmCorrector", base_url: $("llmBaseUrl").value.trim() || null });
  });
  $("btnLaunchLlm").addEventListener("click", () => {
    saveNow();
    send({ type: "LaunchLlmServer" });
  });

  $("btnRefreshCatalog").addEventListener("click", () => send({ type: "GetCatalogInventory" }));
  $("btnApplyWizard").addEventListener("click", () => send({ type: "ApplyWizardRecommendation" }));
  $("btnStartCalibrationWizard").addEventListener("click", () => {
    const box = $("calibrationResultBox");
    box.className = "ab-result show";
    box.textContent = "15秒滑舌診断を開始します。お好きな文章を普段のスピードで話してください...";
    send({ type: "StartCalibrationWizard" });
  });

  $("btnExportBackup").addEventListener("click", () => send({ type: "ExportBackup" }));
  $("btnImportBackup").addEventListener("click", () => $("backupFileInput").click());
  $("backupFileInput").addEventListener("change", (e) => {
    const file = e.target.files && e.target.files[0];
    if (!file) return;
    const reader = new FileReader();
    reader.onload = () => {
      if (typeof reader.result === "string") {
        send({ type: "ImportBackup", json: reader.result });
      }
    };
    reader.readAsText(file, "utf-8");
    e.target.value = "";
  });

  $("btnRunBenchmark").addEventListener("click", () => {
    $("benchResultBox").className = "ab-result show";
    $("benchResultBox").textContent = "ベンチマーク一括テストを実行中...";
    send({ type: "RunBenchmarkSuite" });
  });
  $("btnRunAutoTuner").addEventListener("click", () => {
    $("autoTunerProposalBox").className = "ab-result show";
    $("autoTunerProposalBox").textContent = "最適パラメータを探索中...";
    send({ type: "RunAutoTuner" });
  });
  $("btnRefreshStats").addEventListener("click", () => send({ type: "GetStats" }));
  $("btnResetStats").addEventListener("click", () => {
    if (window.confirm("認識統計データをリセットしますか？")) {
      send({ type: "ResetStats" });
    }
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
  $("sttModelFile").addEventListener("change", saveNow);
  $("sttEngine").addEventListener("change", saveNow);

  document.querySelectorAll("#activationSegmented .seg-btn").forEach((btn) => {
    btn.addEventListener("click", () => {
      updateActivationButtons(btn.dataset.activation);
      saveNow();
    });
  });

  $("btnCancelCountdown").addEventListener("click", () => {
    send({ type: "CancelSendCountdown" });
  });
  $("chkCountdownEnabled").addEventListener("change", saveNow);
  $("countdownMs").addEventListener("change", saveNow);

  $("btnClearDiagnostics").addEventListener("click", () => {
    send({ type: "ClearDiagnostics" });
  });

  $("chkHttpAsrAutoLaunch").addEventListener("change", saveNow);
  $("chkHttpAsrStopExit").addEventListener("change", saveNow);
  $("btnTestHttpAsr").addEventListener("click", () => {
    const baseUrl = $("httpAsrBaseUrl").value.trim() || null;
    send({ type: "TestHttpAsr", base_url: baseUrl });
  });
  $("btnLaunchHttpAsr").addEventListener("click", () => {
    saveNow();
    send({ type: "LaunchHttpAsr" });
  });

  document.querySelectorAll("[data-download-model]").forEach((btn) => {
    btn.addEventListener("click", () => {
      send({
        type: "DownloadModel",
        model_id: btn.dataset.downloadModel,
        custom_url: null,
        custom_filename: null,
      });
    });
  });

  $("btnDownloadCustomModel").addEventListener("click", () => {
    const customUrl = $("customModelUrl").value.trim();
    const customFilename = $("customModelFilename").value.trim();
    if (!customUrl) {
      showToast("カスタムURLを入力してください", "error");
      return;
    }
    send({
      type: "DownloadModel",
      model_id: "custom",
      custom_url: customUrl,
      custom_filename: customFilename || null,
    });
  });

  $("btnGpSave").addEventListener("click", () => {
    const name = $("gpName").value.trim();
    const processes = splitCsv($("gpProcesses").value);
    if (!name || !processes.length) {
      showToast("プロファイル名と対象プロセス名を入力してください", "error");
      return;
    }
    const editId = $("gpEditId").value;
    const xRaw = $("gpOverlayX").value.trim();
    const yRaw = $("gpOverlayY").value.trim();
    const opRaw = $("gpOverlayOpacity").value.trim();
    const gpObj = {
      id: editId || genId("gp"),
      name,
      enabled: true,
      process_names: processes,
      dictionary: parseProfileDictText($("gpDictText").value),
      send_method: $("gpSendMethod").value || null,
      speech_style: $("gpSpeechStyle").value || null,
      overlay_x: xRaw !== "" ? parseInt(xRaw, 10) : null,
      overlay_y: yRaw !== "" ? parseInt(yRaw, 10) : null,
      overlay_opacity: opRaw !== "" ? Math.max(0.2, Math.min(1.0, parseFloat(opRaw))) : null,
    };
    const idx = gameProfiles.findIndex((x) => x.id === gpObj.id);
    if (idx >= 0) {
      gpObj.enabled = gameProfiles[idx].enabled !== false;
      gameProfiles[idx] = gpObj;
    } else {
      gameProfiles.push(gpObj);
    }
    resetGameProfileForm();
    renderGameProfiles();
    saveNow();
  });
  $("btnGpCancelEdit").addEventListener("click", resetGameProfileForm);

  const VAD_PRESETS = {
    normal: {
      label: "通常会話（推奨）",
      threshold: 0.012,
      silence_ms: 620,
      min_speech_ms: 100,
      pre_roll_ms: 500,
      auto_adjust: true,
    },
    fast: {
      label: "早口・連続",
      threshold: 0.010,
      silence_ms: 750,
      min_speech_ms: 80,
      pre_roll_ms: 520,
      auto_adjust: true,
    },
    slow: {
      label: "ゆっくり・小声",
      threshold: 0.007,
      silence_ms: 850,
      min_speech_ms: 100,
      pre_roll_ms: 600,
      auto_adjust: true,
    },
  };

  document.querySelectorAll("[data-vad-preset]").forEach((btn) => {
    btn.addEventListener("click", () => {
      const p = VAD_PRESETS[btn.dataset.vadPreset];
      if (!p || !currentConfig) return;
      currentConfig.vad.pre_roll_ms = p.pre_roll_ms;
      currentConfig.stt.preset = btn.dataset.vadPreset;
      updatePresetButtons(btn.dataset.vadPreset);
      $("vadThreshold").value = p.threshold;
      $("vadThresholdVal").textContent = p.threshold.toFixed(3);
      updateThresholdMarker(p.threshold);
      $("vadSilenceMs").value = p.silence_ms;
      $("vadMinSpeechMs").value = p.min_speech_ms;
      $("chkVadAutoAdjust").checked = p.auto_adjust;
      saveNow();
      showToast(`会話プリセット「${p.label}」を適用しました`, "ok");
    });
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

  function renderCatalogInventory(payload) {
    if (!payload) return;
    lastCatalogInventory = payload;
    const hw = payload.hardware || {};
    $("hwGpuName").textContent = `${hw.gpu_name || "CPU"} (${hw.gpu_vendor || "CPU"} | VRAM ${Number(hw.vram_gb || 0).toFixed(1)} GB${hw.vram_usage_pct != null ? " / 使用率 " + hw.vram_usage_pct + "%" : ""} | CPU ${hw.cpu_cores || "-"}コア)`;
    $("hwSummaryNote").textContent = `${hw.summary_note || ""} | 推奨: ${hw.recommended_stt_engine} / ${hw.recommended_stt_model || "標準"} | VAD=${hw.recommended_vad_noise} / STT=${hw.recommended_stt_noise}`;

    const container = $("modelCatalogContainer");
    if (!container) return;
    container.innerHTML = "";
    const categories = [
      ["stt", "音声認識 (STT) モデル"],
      ["noise_suppression", "AIノイズ抑制モデル"],
      ["llm_correction", "文脈誤字補正 (ローカルLLM)"],
      ["vad", "発話区間検出 (VAD)"],
    ];
    const items = payload.items || [];
    categories.forEach(([catKey, catTitle]) => {
      const group = items.filter((m) => m.category === catKey);
      if (!group.length) return;
      const h = document.createElement("div");
      h.className = "catalog-category-title";
      h.textContent = catTitle;
      container.appendChild(h);

      const grid = document.createElement("div");
      grid.className = "catalog-grid";
      group.forEach((m) => {
        const card = document.createElement("div");
        card.className = "model-card" + (m.in_use ? " in-use" : "");

        const top = document.createElement("div");
        top.className = "row space-between";
        const title = document.createElement("strong");
        title.textContent = m.display_name || m.id;
        const badgeWrap = document.createElement("div");
        badgeWrap.className = "model-meta-row";
        if (m.in_use) {
          const b = document.createElement("span");
          b.className = "pill ok";
          b.textContent = "使用中";
          badgeWrap.appendChild(b);
        }
        const fit = document.createElement("span");
        fit.className = "pill " + ((m.pc_fit_badge || "").includes("推奨") || (m.pc_fit_badge || "").includes("最適") ? "ok" : "");
        fit.textContent = m.pc_fit_badge || m.runtime;
        badgeWrap.appendChild(fit);
        top.appendChild(title);
        top.appendChild(badgeWrap);

        const meta = document.createElement("div");
        meta.className = "small-label";
        meta.textContent = `方式: ${m.runtime} | 状態: ${m.installed ? "導入済み (" + m.disk_size_label + ")" : "未取得 (~" + m.download_size_mb + " MB)"}`;

        const ul = document.createElement("ul");
        ul.className = "model-benefits";
        (m.benefits || []).forEach((b) => {
          const li = document.createElement("li");
          li.textContent = b;
          ul.appendChild(li);
        });

        const best = document.createElement("div");
        best.className = "small-label";
        best.textContent = `得意: ${m.best_for || "-"} / 注意: ${m.caution || "-"}`;

        const sc = m.scores || {};
        const bars = document.createElement("div");
        bars.className = "score-bars";
        bars.innerHTML = `<span>精度:${sc.accuracy || 0}/5</span><span>耐騒音:${sc.noise_resilience || 0}/5</span><span>速度:${sc.speed || 0}/5</span><span>軽量:${sc.lightness || 0}/5</span>`;

        const actions = document.createElement("div");
        actions.className = "row gap-sm";
        actions.style.marginTop = "4px";
        actions.style.flexWrap = "wrap";

        if (!m.installed && m.downloadable) {
          const dlBtn = document.createElement("button");
          dlBtn.type = "button";
          dlBtn.className = "btn-primary btn-sm";
          dlBtn.textContent = `取得 (~${m.download_size_mb}MB)`;
          dlBtn.addEventListener("click", () => {
            send({ type: "DownloadModel", model_id: m.id, custom_url: null, custom_filename: null });
          });
          actions.appendChild(dlBtn);
        }

        if (m.installed && !m.in_use) {
          const useBtn = document.createElement("button");
          useBtn.type = "button";
          useBtn.className = "btn-secondary btn-sm";
          useBtn.textContent = "使用する";
          useBtn.addEventListener("click", () => {
            if (m.category === "noise_suppression") {
              $("ncSttEngine").value = m.id;
              saveNow();
            } else if (m.id.startsWith("ggml-") || m.id.startsWith("whisper-")) {
              $("sttEngine").value = "whisper-cpp";
              if (m.id.endsWith(".bin")) $("sttModelFile").value = m.id;
              saveNow();
            } else if (m.id === "qwen3-asr-0.6b-int8") {
              $("sttEngine").value = "sherpa-qwen3";
              saveNow();
            } else if (m.id === "qwen3-asr-1.7b-gguf") {
              $("sttEngine").value = "local-http";
              saveNow();
            } else if (m.id === "parakeet-ja-int8") {
              $("sttEngine").value = "sherpa-onnx";
              saveNow();
            } else if (m.category === "llm_correction") {
              $("chkLlmEnabled").checked = true;
              saveNow();
            }
          });
          actions.appendChild(useBtn);
        }

        if (m.can_delete) {
          const delBtn = document.createElement("button");
          delBtn.type = "button";
          delBtn.className = "btn-danger btn-sm";
          delBtn.textContent = "削除";
          delBtn.addEventListener("click", () => {
            if (m.in_use) {
              if (!window.confirm(`「${m.display_name}」は現在使用中です。安全にアンロードしてディスクから削除しますか？`)) {
                return;
              }
            } else if (!window.confirm(`「${m.display_name}」(${m.disk_size_label}) をディスクから削除しますか？`)) {
              return;
            }
            send({ type: "DeleteModel", model_id: m.id });
          });
          actions.appendChild(delBtn);
        } else if (m.bundled_default) {
          const lockBadge = document.createElement("span");
          lockBadge.className = "pill";
          lockBadge.textContent = "同梱（削除不可）";
          actions.appendChild(lockBadge);
        }

        card.appendChild(top);
        card.appendChild(meta);
        card.appendChild(ul);
        card.appendChild(best);
        card.appendChild(bars);
        card.appendChild(actions);
        grid.appendChild(card);
      });
      container.appendChild(grid);
    });
  }

  function renderBenchmarkCases(cases) {
    benchmarkCases = cases || [];
    const box = $("benchCasesList");
    if (!box) return;
    box.innerHTML = "";
    if (!benchmarkCases.length) {
      box.innerHTML = '<div class="empty-state">保存されたベンチマーク音声はありません</div>';
      return;
    }
    benchmarkCases.forEach((c) => {
      const row = document.createElement("div");
      row.className = "bench-item row space-between";
      const left = document.createElement("div");
      left.innerHTML = `<strong>${c.label}</strong> <span class="small-label">(${Number(c.duration_sec || 0).toFixed(2)}s)</span><div class="small-label">期待: ${c.expected_text}</div>`;
      const del = document.createElement("button");
      del.type = "button";
      del.className = "btn-danger btn-sm";
      del.textContent = "削除";
      del.addEventListener("click", () => send({ type: "DeleteBenchmarkCase", id: c.id }));
      row.appendChild(left);
      row.appendChild(del);
      box.appendChild(row);
    });
  }

  function renderStatsSummary(s) {
    const box = $("statsSummaryBox");
    if (!box || !s) return;
    const engines = (s.engines || []).map(([k, v]) => `${k} (${v.device}): ${v.utterances}回 / 平均${v.utterances ? Math.round(v.total_infer_ms / v.utterances) : 0}ms`).join("<br>");
    const tops = (s.top_misrecognitions || []).map(([lbl, cnt]) => `${lbl} (${cnt}回)`).join(" / ") || "なし";
    box.innerHTML = `
      <div class="stats-grid">
        <div class="stat-kpi"><div class="small-label">総発話回数</div><div class="kpi-val">${s.total_utterances || 0}</div></div>
        <div class="stat-kpi"><div class="small-label">取り消し率</div><div class="kpi-val">${Number(s.cancel_rate_pct || 0).toFixed(1)}%</div></div>
        <div class="stat-kpi"><div class="small-label">平均推論時間</div><div class="kpi-val">${s.avg_infer_ms || 0} ms</div></div>
      </div>
      <div class="small-label" style="margin-top:8px"><b>エンジン別統計:</b><br>${engines || "-"}</div>
      <div class="small-label" style="margin-top:6px"><b>よくある修正ワード:</b> ${tops}</div>
    `;
  }

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
        if ($("streamingPreviewLine")) {
          $("streamingPreviewLine").classList.remove("show");
          $("streamingPreviewLine").textContent = "";
        }
        if (typeof ev.draft === "string") {
          $("draftBox").value = ev.draft;
        }
        if (ev.is_final && ev.text) {
          addHistoryItem(ev.text);
        }
        break;
      case "StreamingPreview": {
        const line = $("streamingPreviewLine");
        if (line && ev.draft) {
          line.textContent = "⏳ 認識中プレビュー: " + ev.draft;
          line.classList.add("show");
        }
        break;
      }
      case "CommandExecuted":
        showToast("コマンド実行: " + ev.detail, "ok");
        break;
      case "ConfirmationPrompt": {
        const banner = $("confirmBanner");
        if (ev.active) {
          $("confirmTitle").textContent = ev.title || "";
          $("confirmSub").textContent = ev.subtitle || "";
          $("btnConfirmNext").style.display = ev.has_next === false ? "none" : "inline-block";
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
        pillRn.textContent = ev.rnnoise_enabled
          ? `ノイズ抑制: ${ev.noise_stt_engine || "ON"}`
          : "ノイズ抑制: OFF";
        pillRn.className = "pill " + (ev.rnnoise_enabled ? "ok" : "");

        isPanicMode = !!ev.panic_mode;
        const panicBtn = $("btnPanicMute");
        if (panicBtn) {
          panicBtn.textContent = isPanicMode ? "🛡 セーフ中 (解除)" : "🛡 配信セーフ";
          panicBtn.className = "btn-secondary btn-sm" + (isPanicMode ? " btn-panic-on" : "");
        }
        renderHistory();

        const lockBtn = $("btnTargetLock");
        if (lockBtn) {
          lockBtn.textContent = ev.target_locked ? "🔒 宛先ロック中" : "🔓 宛先ロック";
          lockBtn.className = "btn-secondary btn-sm" + (ev.target_locked ? " btn-warning" : "");
        }
        if ($("chkTargetLocked")) $("chkTargetLocked").checked = !!ev.target_locked;

        const pillPhase = $("pillPhase");
        if (pillPhase) {
          const ph = ev.phase || "idle";
          pillPhase.textContent =
            isPanicMode ? "セーフ停止" :
            ph === "rescuing" ? "2パス救済" :
            ph === "countdown" ? "送信カウント" :
            ph === "recognizing" ? "推論中" :
            ph === "listening" ? "収音待機" : "待機";
          pillPhase.className = "pill " + (isPanicMode ? "warn" : ph === "recognizing" || ph === "rescuing" ? "active" : ph === "listening" ? "ok" : "");
        }

        const envModes = [];
        if (ev.night_mode_active) envModes.push("🌙夜間");
        if (ev.eco_mode_active) envModes.push("🔋Eco");
        if (ev.portable_mode) envModes.push("💾Portable");
        const pillEnv = $("pillEnvMode");
        if (pillEnv) {
          if (envModes.length) {
            pillEnv.textContent = envModes.join(" ");
            pillEnv.style.display = "inline-block";
          } else {
            pillEnv.style.display = "none";
          }
        }

        activeTargetId = ev.active_discord_target_id || null;
        const tgtLabel = isPanicMode ? "*** (非表示)" : (ev.active_discord_target_label || "現在の画面（未指定）");
        $("sidebarDiscordTarget").textContent = tgtLabel;
        const pillDt = $("pillDiscordTarget");
        pillDt.textContent = (ev.target_locked ? "🔒 宛先: " : "宛先: ") + tgtLabel;
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

        const pillDev = $("pillSttDevice");
        const devLabel = ev.stt_device || "-";
        const latStr = ev.last_infer_ms
          ? ` (${ev.last_infer_ms}ms / RTF ${Number(ev.last_rtf || 0).toFixed(2)})`
          : "";
        pillDev.textContent = "推論: " + devLabel + latStr;
        pillDev.className = "pill " + (devLabel.includes("Vulkan") || devLabel.includes("DirectML") ? "ok" : "");

        $("diagEngine").textContent = "エンジン: " + (ev.stt_engine || "-");
        $("diagDevice").textContent = "デバイス: " + devLabel;
        $("diagLastMs").textContent = "遅延: " + (ev.last_infer_ms ? ev.last_infer_ms + "ms" : "-");
        $("diagLastRtf").textContent = "RTF: " + (ev.last_infer_ms ? Number(ev.last_rtf || 0).toFixed(2) : "-");

        const pillGp = $("pillGameProfile");
        if (ev.active_profile) {
          pillGp.textContent = "プロファイル: " + ev.active_profile;
          pillGp.style.display = "inline-block";
        } else {
          pillGp.style.display = "none";
        }

        updateModeButtons(ev.input_mode);
        updateActivationButtons(ev.activation_mode || "vad");
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
      case "SttModels": {
        const sel = $("sttModelFile");
        const abSel = $("abModelFile");
        const prev = ev.selected != null ? ev.selected : sel.value || "";
        const abPrev = abSel.value || "";
        sel.innerHTML = '<option value="">自動（最高精度モデルを優先）</option>';
        abSel.innerHTML = '<option value="">自動</option>';
        (ev.models || []).forEach((name) => {
          const opt = document.createElement("option");
          opt.value = name;
          opt.textContent = name;
          sel.appendChild(opt);
          abSel.appendChild(opt.cloneNode(true));
        });
        sel.value = prev;
        abSel.value = abPrev;

        const qBadge = $("qwen3StatusBadge");
        qBadge.textContent = ev.qwen3_installed ? "Qwen3-ASR: 導入済み" : "Qwen3-ASR: 未導入";
        qBadge.className = "pill " + (ev.qwen3_installed ? "ok" : "");
        break;
      }
      case "Toast":
        showToast(ev.message, ev.level);
        break;
      case "VoicevoxHealthResult": {
        const badge = $("ttsConnBadge");
        if (ev.ok) {
          badge.textContent = "VOICEVOX: 接続OK (v" + (ev.version || "?") + ")";
          badge.className = "pill ok";
          showToast(ev.message, "ok");
        } else {
          badge.textContent = "VOICEVOX: 未接続";
          badge.className = "pill warn";
          showToast(ev.message, "error");
        }
        break;
      }
      case "VoicevoxSpeakersResult": {
        if (ev.ok && Array.isArray(ev.speakers)) {
          voicevoxSpeakers = ev.speakers;
          const curId =
            (currentConfig &&
              currentConfig.discord_tts &&
              currentConfig.discord_tts.voicevox &&
              currentConfig.discord_tts.voicevox.speaker_id) != null
              ? currentConfig.discord_tts.voicevox.speaker_id
              : parseInt($("ttsSpeakerSelect").value, 10) || 3;
          renderVoicevoxSpeakerSelect(curId);
          showToast(ev.message, "ok");
        } else if (!ev.ok) {
          showToast(ev.message, "error");
        }
        break;
      }
      case "VoicevoxDetectResult": {
        if (ev.found && ev.path) {
          $("ttsExePath").value = ev.path;
          showToast(ev.message, "ok");
        } else {
          showToast(ev.message, "error");
        }
        break;
      }
      case "TtsStatusChanged": {
        const conn = $("ttsConnBadge");
        if (!ev.connected && ev.last_error) {
          conn.textContent = "VOICEVOX: 未接続";
          conn.className = "pill warn";
        } else if (ev.connected && conn.textContent.includes("未確認")) {
          conn.textContent = "VOICEVOX: 接続OK";
          conn.className = "pill ok";
        }
        const sp = $("ttsSpeakingBadge");
        if (ev.speaking) {
          sp.textContent = ev.queued > 0 ? `読み上げ中 (待機:${ev.queued})` : "読み上げ中";
          sp.className = "pill active";
        } else if (ev.queued > 0) {
          sp.textContent = `待機中 (${ev.queued}件)`;
          sp.className = "pill warn";
        } else {
          sp.textContent = "待機中";
          sp.className = "pill";
        }
        break;
      }
      case "ToastDebugLog":
        addToastDebugLog(ev.text);
        break;
      case "DiagnosticsUpdated":
        diagnosticsItems = ev.items || [];
        renderDiagnostics(diagnosticsItems);
        break;
      case "AbCompareResult": {
        const box = $("ab-result-" + ev.id);
        if (box) {
          box.className = "ab-result show" + (ev.ok ? "" : " err");
          box.textContent = ev.ok
            ? `[${ev.engine_label} | ${ev.device_name} | ${ev.infer_ms}ms | RTF ${Number(ev.rtf || 0).toFixed(2)}] ${ev.text || "(空)"}`
            : `エラー (${ev.engine_label}): ${ev.error || "推論失敗"}`;
        }
        break;
      }
      case "SendCountdown": {
        const banner = $("countdownBanner");
        if (ev.active) {
          const sec = (Math.max(0, ev.remaining_ms || 0) / 1000).toFixed(1);
          const pct = Math.min(
            100,
            Math.max(0, ((ev.remaining_ms || 0) / Math.max(1, ev.total_ms || 1)) * 100)
          );
          $("countdownText").textContent = `送信まで ${sec}秒（「待って」/「取り消し」/ Esc で中止）: ${ev.preview_text || ""}`;
          $("countdownProgress").style.width = pct + "%";
          banner.style.display = "flex";
        } else {
          banner.style.display = "none";
        }
        break;
      }
      case "ModelDownloadProgress": {
        $("dlProgressWrap").style.display = "block";
        $("dlProgressText").textContent = `[${ev.model_id}] ${ev.message || ""}`;
        $("dlProgressBar").style.width = Math.min(100, Math.max(0, ev.progress || 0)) + "%";
        if (ev.done) {
          showToast(ev.message || (ev.ok ? "モデル取得完了" : "取得失敗"), ev.ok ? "ok" : "error");
          if (ev.ok) {
            send({ type: "ListSttModels" });
            send({ type: "GetCatalogInventory" });
          }
        }
        break;
      }
      case "HttpAsrHealthResult": {
        const badge = $("httpAsrStatusBadge");
        badge.textContent = ev.ok ? "HTTP ASR: 接続OK" : "HTTP ASR: 未接続";
        badge.className = "pill " + (ev.ok ? "ok" : "warn");
        showToast(ev.message, ev.ok ? "ok" : "error");
        break;
      }
      case "CatalogInventory":
        renderCatalogInventory(ev);
        break;
      case "StatsUpdated":
        renderStatsSummary(ev.summary);
        break;
      case "BenchmarkCasesUpdated":
        renderBenchmarkCases(ev.cases);
        break;
      case "BenchmarkSuiteResult": {
        const box = $("benchResultBox");
        if (box) {
          box.className = "ab-result show";
          const rows = (ev.results || []).map((r) => `・${r.label}: 一致 ${(r.similarity * 100).toFixed(1)}% (${r.infer_ms}ms) → 「${r.actual_text}」`).join("\n");
          box.textContent = `平均一致率: ${(Number(ev.avg_similarity || 0) * 100).toFixed(1)}%\n` + rows;
        }
        break;
      }
      case "AutoTunerResult": {
        const p = ev.proposal || {};
        const box = $("autoTunerProposalBox");
        if (box) {
          box.className = "ab-result show";
          const dpdfDb = Number(p.best_dpdfnet_db != null ? p.best_dpdfnet_db : 12);
          box.innerHTML = `提案 (${p.tested_cases || 0}件評価 / 一致 ${(Number(p.avg_similarity || 0) * 100).toFixed(1)}%): EQ=${Number(p.best_eq_strength || 0).toFixed(2)}, WSOLA=${Number(p.best_wsola_rate || 1).toFixed(2)}x, Beam=${p.best_beam_size || 3}, DPDFNet=${dpdfDb}dB <button type="button" class="btn-primary btn-sm" id="btnApplyTuner">この設定を適用</button>`;
          const b = $("btnApplyTuner");
          if (b) {
            b.addEventListener("click", () => {
              send({
                type: "ApplyAutoTuner",
                eq_strength: p.best_eq_strength,
                wsola_rate: p.best_wsola_rate,
                beam_size: p.best_beam_size,
                dpdfnet_db: dpdfDb,
              });
            });
          }
        }
        break;
      }
      case "CalibrationWizardResult": {
        const box = $("calibrationResultBox");
        if (box) {
          box.className = "ab-result show";
          const ne = ev.recommended_noise_engine || "dpdfnet-16k";
          box.innerHTML = `${ev.score_note} | 推奨VAD=${Number(ev.recommended_vad || 0.012).toFixed(3)}, EQ=${Number(ev.recommended_eq || 0.55).toFixed(2)}, WSOLA=${ev.recommended_wsola ? "ON" : "OFF"}, ノイズ抑制=${ne} <button type="button" class="btn-primary btn-sm" id="btnApplyCalibration">診断結果を1クリック適用</button>`;
          const b = $("btnApplyCalibration");
          if (b) {
            b.addEventListener("click", () => {
              send({
                type: "ApplyCalibrationResult",
                vad_threshold: Number(ev.recommended_vad || 0.012),
                eq_strength: Number(ev.recommended_eq || 0.55),
                wsola_enabled: !!ev.recommended_wsola,
                noise_engine: ne,
              });
            });
          }
        }
        showToast("15秒滑舌診断が完了しました", "ok");
        break;
      }
      case "BackupExported": {
        const blob = new Blob([ev.json || "{}"], { type: "application/json" });
        const url = URL.createObjectURL(blob);
        const a = document.createElement("a");
        a.href = url;
        a.download = "voxdrop-backup-v1.6.json";
        document.body.appendChild(a);
        a.click();
        a.remove();
        setTimeout(() => URL.revokeObjectURL(url), 1000);
        showToast("設定と辞書のバックアップJSONをダウンロードしました", "ok");
        break;
      }
      case "AppUpdateAvailable": {
        const ban = $("updateBanner");
        if (ban) {
          $("updateTitle").textContent = `VoxDrop ${ev.latest_tag} が利用可能です（現在: ${ev.current_tag}）`;
          $("updateSub").textContent = ev.url || "";
          $("updateOpenLink").href = ev.url || "https://github.com/hhurt21/voxdrop/releases";
          ban.style.display = "flex";
        }
        break;
      }
      case "LlmHealthResult": {
        const badge = $("llmStatusBadge");
        if (badge) {
          badge.textContent = ev.ok ? "LLM: 接続OK" : "LLM: 未接続";
          badge.className = "pill " + (ev.ok ? "ok" : "warn");
        }
        showToast(ev.message, ev.ok ? "ok" : "error");
        break;
      }
      case "PostSendError":
        showToast(`[PostMessage送信失敗: ${ev.code}] ${ev.detail}`, "error");
        break;
    }
  };

  send({ type: "GetConfig" });
  send({ type: "ListAudioDevices" });
  send({ type: "ListSttModels" });
  send({ type: "GetCatalogInventory" });
  send({ type: "GetStats" });
  send({ type: "CheckAppUpdate" });
})();
