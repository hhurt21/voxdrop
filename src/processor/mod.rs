pub mod commands;
pub mod filter;
pub mod japanese_fix;

use crate::config::{DiscordTargetsConfig, TextFilterConfig, VoiceCommandsConfig};
use commands::{TargetSwitchAction, VoiceCommand};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessedUtterance {
    pub text: String,
    pub command: Option<VoiceCommand>,
    pub target_switch: Option<TargetSwitchAction>,
}

/// Runs the full text-processing pipeline on raw STT output:
/// 0. Japanese naturalness & ASR artifact post-correction
/// 1. Filler removal & custom dictionary substitution
/// 2. Discord target switching via passphrase (合言葉)
/// 3. Voice command extraction (suffix or standalone)
/// 4. Punctuation cleanup / auto-period rules on the remaining text
pub fn process_transcript(
    raw: &str,
    filter_cfg: &TextFilterConfig,
    cmd_cfg: &VoiceCommandsConfig,
    targets_cfg: &DiscordTargetsConfig,
) -> ProcessedUtterance {
    let repaired = japanese_fix::normalize_japanese_transcript(raw);
    let normalized = filter::apply_fillers_and_dictionary(&repaired, filter_cfg);

    let (target_switch, after_target) =
        match commands::parse_target_switch(&normalized, targets_cfg) {
            Some(ts) => (Some(ts.action), ts.remaining_text),
            None => (None, normalized),
        };

    let parsed = commands::parse_voice_command(&after_target, cmd_cfg);
    let final_text = filter::apply_punctuation_rules(&parsed.remaining_text, filter_cfg);
    ProcessedUtterance {
        text: final_text,
        command: parsed.command,
        target_switch,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{DiscordTarget, DiscordTargetKind};

    #[test]
    fn end_to_end_pipeline_cleans_filler_replaces_dict_and_extracts_send() {
        let filter_cfg = TextFilterConfig::default();
        let cmd_cfg = VoiceCommandsConfig::default();
        let targets_cfg = DiscordTargetsConfig::default();

        let res = process_transcript("えーと、くさ送信して。", &filter_cfg, &cmd_cfg, &targets_cfg);
        assert_eq!(res.text, "草");
        assert_eq!(res.command, Some(VoiceCommand::Send));
        assert_eq!(res.target_switch, None);
    }

    #[test]
    fn append_period_does_not_break_command_detection() {
        let mut filter_cfg = TextFilterConfig::default();
        filter_cfg.append_period = true;
        let cmd_cfg = VoiceCommandsConfig::default();
        let targets_cfg = DiscordTargetsConfig::default();

        let res = process_transcript("了解しました送って", &filter_cfg, &cmd_cfg, &targets_cfg);
        assert_eq!(res.text, "了解しました。");
        assert_eq!(res.command, Some(VoiceCommand::Send));
    }

    #[test]
    fn passphrase_switch_and_send_work_in_same_utterance() {
        let filter_cfg = TextFilterConfig::default();
        let cmd_cfg = VoiceCommandsConfig::default();
        let mut targets_cfg = DiscordTargetsConfig::default();
        targets_cfg.targets.push(DiscordTarget {
            id: "guild_raid".into(),
            name: "レイド連絡".into(),
            passphrases: vec!["れいど".into(), "レイド".into()],
            kind: DiscordTargetKind::Channel,
            guild_id: "100".into(),
            channel_id: "200".into(),
            ..Default::default()
        });

        let res = process_transcript(
            "えーと合言葉レイド、くさ送信して",
            &filter_cfg,
            &cmd_cfg,
            &targets_cfg,
        );
        assert_eq!(
            res.target_switch,
            Some(TargetSwitchAction::SwitchExact("guild_raid".into()))
        );
        assert_eq!(res.text, "草");
        assert_eq!(res.command, Some(VoiceCommand::Send));
    }

    #[test]
    fn auto_newline_toggle_controls_sentence_line_breaks() {
        let mut filter_cfg = TextFilterConfig::default();
        let cmd_cfg = VoiceCommandsConfig::default();
        let targets_cfg = DiscordTargetsConfig::default();

        filter_cfg.auto_newline = true;
        let with_nl = process_transcript(
            "これでいいのか？よさそうこれ一部だけ送信できるの？",
            &filter_cfg,
            &cmd_cfg,
            &targets_cfg,
        );
        assert_eq!(
            with_nl.text,
            "これでいいのか？\nよさそう。\nこれ一部だけ送信できるの？"
        );

        filter_cfg.auto_newline = false;
        let without_nl = process_transcript(
            "これでいいのか？よさそうこれ一部だけ送信できるの？",
            &filter_cfg,
            &cmd_cfg,
            &targets_cfg,
        );
        assert_eq!(
            without_nl.text,
            "これでいいのか？よさそう。これ一部だけ送信できるの？"
        );
    }
}
