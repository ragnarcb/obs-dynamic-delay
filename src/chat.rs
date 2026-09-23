//! Twitch chat commands for the streamer and moderators, e.g. `!delay on`.
//! Reads chat anonymously (no login needed) over IRC; only listens.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, bail};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::mpsc::UnboundedSender;

use crate::EngineMsg;
use crate::config::TwitchChat;
use crate::status::{Cmd, Shared};

/// Keeps a chat connection matching the current settings, reconnecting on changes.
pub async fn run(shared: Arc<Shared>, tx: UnboundedSender<EngineMsg>) {
    loop {
        let cfg = shared.config.lock().unwrap().twitch_chat.clone();
        if !cfg.enabled || channel(&cfg).is_empty() {
            tokio::time::sleep(Duration::from_secs(3)).await;
            continue;
        }
        match listen(&shared, &tx, &cfg).await {
            Ok(()) => {} // settings changed
            Err(e) => {
                log::warn!("[chat] {e:#}");
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
        }
    }
}

fn channel(cfg: &TwitchChat) -> String {
    cfg.channel.trim().trim_start_matches('#').trim_start_matches("https://www.twitch.tv/").to_lowercase()
}

async fn listen(shared: &Shared, tx: &UnboundedSender<EngineMsg>, cfg: &TwitchChat) -> Result<()> {
    let chan = channel(cfg);
    let stream = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect("irc.chat.twitch.tv:6667")).await??;
    let (r, mut w) = stream.into_split();
    let nick = format!("justinfan{}", 10_000 + std::process::id() % 80_000);
    w.write_all(format!("CAP REQ :twitch.tv/tags\r\nPASS SCHMOOPIIE\r\nNICK {nick}\r\nJOIN #{chan}\r\n").as_bytes()).await?;
    log::info!("[chat] listening to #{chan}");
    let mut lines = BufReader::new(r).lines();
    let mut check = tokio::time::interval(Duration::from_secs(3));
    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line? else { bail!("chat connection closed") };
                if line.starts_with("PING") {
                    w.write_all(line.replacen("PING", "PONG", 1).as_bytes()).await?;
                    w.write_all(b"\r\n").await?;
                    continue;
                }
                if let Some((user, cmd)) = parse_command(&line, cfg) {
                    log::info!("[chat] {user}: {cmd:?}");
                    let _ = tx.send(EngineMsg::Cmd(cmd));
                }
            }
            _ = check.tick() => {
                if shared.config.lock().unwrap().twitch_chat != *cfg {
                    return Ok(());
                }
            }
        }
    }
}

/// Parses a tagged PRIVMSG; returns the sender and the command if they may use it.
fn parse_command(line: &str, cfg: &TwitchChat) -> Option<(String, Cmd)> {
    let (tags, rest) = line.strip_prefix('@')?.split_once(' ')?;
    let (prefix, rest) = rest.strip_prefix(':')?.split_once(' ')?;
    let rest = rest.strip_prefix("PRIVMSG ")?;
    let (_, text) = rest.split_once(" :")?;
    let user = prefix.split('!').next().unwrap_or("").to_string();

    let badges = tags.split(';').find_map(|t| t.strip_prefix("badges=")).unwrap_or("");
    let has = |b: &str| badges.split(',').any(|x| x.split('/').next() == Some(b));
    let allowed = match cfg.allow.as_str() {
        "broadcaster" => has("broadcaster"),
        "vips" => has("broadcaster") || has("moderator") || has("vip"),
        _ => has("broadcaster") || has("moderator"),
    };
    if !allowed {
        return None;
    }
    let prefix = cfg.prefix.trim().to_lowercase();
    let text = text.trim().to_lowercase();
    let args = text.strip_prefix(&prefix)?;
    if !(args.is_empty() || args.starts_with(char::is_whitespace)) {
        return None; // "!delayed" is not "!delay"
    }
    let mut it = args.split_whitespace();
    let word = it.next().unwrap_or("toggle").to_lowercase();
    let arg = it.next().and_then(|n| n.trim_end_matches('s').parse::<u32>().ok());
    let cmd = match word.as_str() {
        "on" | "ligar" => Cmd::On,
        "off" | "desligar" => Cmd::Off,
        "toggle" => Cmd::Toggle,
        "censor" | "apagar" => Cmd::Censor(arg),
        "replay" => Cmd::Replay(arg),
        "clip" | "clipe" => Cmd::Clip(arg),
        "panic" | "panico" | "pânico" => Cmd::Panic,
        n => match n.trim_end_matches('s').parse::<u32>() {
            Ok(secs) => Cmd::Set(secs),
            Err(_) => return None,
        },
    };
    Some((user, cmd))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(allow: &str) -> TwitchChat {
        TwitchChat { enabled: true, channel: "streamer".into(), allow: allow.into(), prefix: "!delay".into() }
    }

    fn msg(badges: &str, text: &str) -> String {
        format!("@badge-info=;badges={badges};color=;display-name=Joe :joe!joe@joe.tmi.twitch.tv PRIVMSG #streamer :{text}")
    }

    #[test]
    fn mods_can_use_commands() {
        let c = cfg("mods");
        assert_eq!(parse_command(&msg("moderator/1", "!delay on"), &c).unwrap().1, Cmd::On);
        assert_eq!(parse_command(&msg("broadcaster/1", "!delay 45"), &c).unwrap().1, Cmd::Set(45));
        assert_eq!(parse_command(&msg("moderator/1", "!delay apagar 8"), &c).unwrap().1, Cmd::Censor(Some(8)));
        assert_eq!(parse_command(&msg("moderator/1", "!DELAY off"), &c).unwrap().1, Cmd::Off);
        assert_eq!(parse_command(&msg("moderator/1", "!delay"), &c).unwrap().1, Cmd::Toggle);
    }

    #[test]
    fn viewers_are_ignored() {
        let c = cfg("mods");
        assert!(parse_command(&msg("subscriber/12", "!delay off"), &c).is_none());
        assert!(parse_command(&msg("vip/1", "!delay off"), &c).is_none());
        assert!(parse_command(&msg("vip/1", "!delay off"), &cfg("vips")).is_some());
        assert!(parse_command(&msg("moderator/1", "!delay off"), &cfg("broadcaster")).is_none());
        assert!(parse_command(&msg("moderator/1", "hello"), &c).is_none());
        assert!(parse_command(&msg("moderator/1", "!delay banana"), &c).is_none());
    }
}
