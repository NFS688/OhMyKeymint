use std::{
    path::Path,
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use ureq::http::Uri;

use crate::{config, keybox, security_patch, webui_http};

const YURIKEY_URL: &str = "https://raw.githubusercontent.com/Yurii0307/yurikey/main/key";
const INTEGRITY_BOX_URL: &str =
    "https://github.com/MeowDump/Integrity-Box/releases/download/archive/Megatron";
const MAX_ENCODED_BYTES: usize = 3 * 1024 * 1024;
const POLL_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    #[default]
    Yurikey,
    IntegrityBox,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub enabled: bool,
    pub source: Source,
    pub interval_minutes: u32,
    pub proxy_prefix: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            source: Source::default(),
            interval_minutes: 60,
            proxy_prefix: String::new(),
        }
    }
}

impl Settings {
    pub(crate) fn validate(&self) -> Result<()> {
        if !(1..=43_200).contains(&self.interval_minutes) {
            bail!("Keybox update interval must be between 1 and 43200 minutes");
        }
        if !self.proxy_prefix.is_empty() {
            if self.proxy_prefix.len() > 2048
                || self.proxy_prefix.trim() != self.proxy_prefix
                || !self.proxy_prefix.ends_with('/')
            {
                bail!("GitHub proxy prefix must be an HTTPS URL ending in /");
            }
            let uri: Uri = self.proxy_prefix.parse().context("invalid GitHub proxy prefix")?;
            if !valid_https_uri(&uri) || uri.query().is_some() {
                bail!("GitHub proxy prefix must be HTTPS without credentials, query or fragment");
            }
        }
        Ok(())
    }

    fn url(&self) -> String {
        let upstream = match self.source {
            Source::Yurikey => YURIKEY_URL,
            Source::IntegrityBox => INTEGRITY_BOX_URL,
        };
        format!("{}{upstream}", self.proxy_prefix)
    }
}

fn valid_https_uri(uri: &Uri) -> bool {
    uri.scheme_str() == Some("https")
        && uri.authority().is_some_and(|authority| !authority.as_str().contains('@'))
        && !uri.to_string().contains('#')
}

fn allowed_uri(uri: &Uri, proxy: Option<&Uri>) -> bool {
    if !valid_https_uri(uri) {
        return false;
    }
    if let Some(proxy) = proxy {
        return uri.authority() == proxy.authority() && uri.path().starts_with(proxy.path());
    }
    matches!(
        uri.authority().map(|value| value.as_str()),
        Some("raw.githubusercontent.com" | "github.com" | "release-assets.githubusercontent.com")
    )
}

fn compact(bytes: &[u8]) -> Vec<u8> {
    bytes.iter().copied().filter(|byte| !byte.is_ascii_whitespace()).collect()
}

fn decode(source: Source, contents: &str) -> Result<Vec<u8>> {
    if contents.is_empty() || contents.len() > MAX_ENCODED_BYTES {
        bail!("Keybox source response is empty or too large");
    }
    let mut bytes = contents.as_bytes().to_vec();
    let layers = match source {
        Source::Yurikey => 1,
        Source::IntegrityBox => 10,
    };
    for layer in 1..=layers {
        bytes = STANDARD
            .decode(compact(&bytes))
            .with_context(|| format!("Keybox Base64 decoding failed at layer {layer}"))?;
    }
    if source == Source::IntegrityBox {
        bytes = hex::decode(compact(&bytes)).context("Keybox hex decoding failed")?;
        for byte in &mut bytes {
            *byte = match *byte {
                b'A'..=b'Z' => b'A' + (*byte - b'A' + 13) % 26,
                b'a'..=b'z' => b'a' + (*byte - b'a' + 13) % 26,
                other => other,
            };
        }
    }
    if bytes.is_empty() || bytes.len() > keybox::MAX_KEYBOX_XML_BYTES {
        bail!("decoded Keybox is empty or exceeds the 64 KiB limit");
    }
    Ok(bytes)
}

fn fetch_and_install(settings: &Settings, scheduled: bool) -> Result<()> {
    settings.validate()?;
    let proxy = if settings.proxy_prefix.is_empty() {
        None
    } else {
        Some(settings.proxy_prefix.parse::<Uri>()?)
    };
    let contents = webui_http::download_https_utf8(
        settings.url().parse()?,
        &webui_http::DownloadPolicy {
            resource: "automatic Keybox",
            redirect_allowlist: "the selected GitHub source or configured proxy",
            max_bytes: MAX_ENCODED_BYTES,
            max_size_label: "3 MiB",
            max_redirects: 3,
            timeout: Duration::from_secs(60),
            connect_timeout: Duration::from_secs(10),
        },
        |uri| allowed_uri(uri, proxy.as_ref()),
    )?;
    let xml = decode(settings.source, &contents)?;
    let _operation = security_patch::acquire_data_operation_lock(Path::new(config::config_path()))?;
    // A settings change during a download cancels that scheduled installation.
    if scheduled && config::load_config_file()?.keybox_auto != *settings {
        bail!("automatic Keybox settings changed during download");
    }
    keybox::install_keybox_xml(&xml)
}

pub(crate) fn run_watcher() -> Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        bail!("automatic Keybox watcher requires root");
    }
    let parent = unsafe { libc::getppid() };
    if parent <= 1 {
        bail!("automatic Keybox watcher requires its privileged helper");
    }
    if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) } != 0 {
        return Err(std::io::Error::last_os_error()).context("cannot bind Keybox watcher lifetime");
    }
    if unsafe { libc::getppid() } != parent {
        return Ok(());
    }
    // The helper supervises this separate root process, so network access does
    // not depend on the keystore UID and cannot block Binder/property probes.
    let mut previous = Settings::default();
    let mut last_attempt: Option<Instant> = None;
    let mut last_error = None;
    loop {
        let settings = match config::load_config_file() {
            Ok(config) => {
                last_error = None;
                config.keybox_auto
            }
            Err(error) => {
                let message = format!("{error:#}");
                if last_error.as_ref() != Some(&message) {
                    log::warn!("automatic Keybox settings unavailable: {message}");
                    last_error = Some(message);
                }
                std::thread::sleep(POLL_INTERVAL);
                continue;
            }
        };
        if settings != previous {
            last_attempt = None;
            previous = settings.clone();
        }
        let interval = Duration::from_secs(u64::from(settings.interval_minutes) * 60);
        if settings.enabled && last_attempt.is_none_or(|last| last.elapsed() >= interval) {
            match fetch_and_install(&settings, true) {
                Ok(()) => log::info!("automatic Keybox update completed: {:?}", settings.source),
                Err(error) => log::warn!("automatic Keybox update failed: {error:#}"),
            }
            last_attempt = Some(Instant::now());
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

pub(crate) fn command(mut args: impl Iterator<Item = String>) -> Option<Result<String>> {
    let command = args.next()?;
    if !matches!(
        command.as_str(),
        "--webui-get-keybox-auto" | "--webui-save-keybox-auto" | "--webui-fetch-keybox-auto"
    ) {
        return None;
    }
    Some((|| {
        if unsafe { libc::geteuid() } != 0 {
            bail!("automatic Keybox management requires root");
        }
        if command == "--webui-get-keybox-auto" {
            if args.next().is_some() {
                bail!("automatic Keybox read does not accept arguments");
            }
            return Ok(serde_json::to_string(&config::load_config_file()?.keybox_auto)?);
        }
        let payload = args.next().ok_or_else(|| anyhow!("missing automatic Keybox settings"))?;
        if payload.len() > 4096
            || args.next().as_deref() != Some("--confirm-replace")
            || args.next().is_some()
        {
            bail!("invalid automatic Keybox arguments or missing replacement confirmation");
        }
        let settings: Settings = serde_json::from_slice(&STANDARD.decode(payload)?)?;
        settings.validate()?;
        if command == "--webui-save-keybox-auto" {
            config::save_keybox_auto(&settings)?;
        } else {
            fetch_and_install(&settings, false)?;
        }
        Ok(serde_json::to_string(&settings)?)
    })())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_both_sources_with_wrapped_base64() {
        let xml = include_bytes!("../template/keybox.xml");
        let encoded = STANDARD.encode(xml);
        assert_eq!(decode(Source::Yurikey, &format!("\n{encoded}\r\n")).unwrap(), xml);
        let rot13: Vec<u8> = xml
            .iter()
            .map(|byte| match byte {
                b'A'..=b'Z' => b'A' + (*byte - b'A' + 13) % 26,
                b'a'..=b'z' => b'a' + (*byte - b'a' + 13) % 26,
                _ => *byte,
            })
            .collect();
        let mut encoded = hex::encode(rot13);
        for _ in 0..10 {
            encoded = format!("\n{}\n", STANDARD.encode(encoded));
        }
        assert_eq!(decode(Source::IntegrityBox, &encoded).unwrap(), xml);
        assert!(decode(Source::IntegrityBox, "invalid").is_err());
        assert!(decode(Source::Yurikey, &STANDARD.encode(vec![b'x'; 65 * 1024])).is_err());
    }

    #[test]
    fn validates_frequency_and_user_proxy_without_built_in_mirrors() {
        let mut settings = Settings::default();
        assert_eq!(settings.interval_minutes, 60);
        assert_eq!(settings.url(), YURIKEY_URL);
        for interval in [0, 43_201, u32::MAX] {
            settings.interval_minutes = interval;
            assert!(settings.validate().is_err());
        }
        settings.interval_minutes = 60;
        for prefix in [
            "http://proxy.test/",
            "https://proxy.test",
            "https://u:p@proxy.test/",
            "https://proxy.test/?q=/",
            "https://proxy.test/#/",
            " https://proxy.test/",
        ] {
            settings.proxy_prefix = prefix.into();
            assert!(settings.validate().is_err(), "{prefix}");
        }
        settings.proxy_prefix = "https://proxy.test/path/".into();
        settings.validate().unwrap();
        let proxy = settings.proxy_prefix.parse().unwrap();
        assert!(allowed_uri(&settings.url().parse().unwrap(), Some(&proxy)));
        assert!(!allowed_uri(&"https://other.test/path/file".parse().unwrap(), Some(&proxy)));
        assert!(!allowed_uri(&"http://github.com/file".parse().unwrap(), None));
        assert!(!allowed_uri(&"https://github.com.evil.test/file".parse().unwrap(), None));
        assert!(allowed_uri(
            &"https://release-assets.githubusercontent.com/file".parse().unwrap(),
            None
        ));
    }

    #[test]
    fn removed_sources_and_unconfirmed_commands_do_not_start_downloads() {
        assert!(serde_json::from_str::<Settings>(r#"{"source":"tricky_addon"}"#).is_err());
        assert!(serde_json::from_str::<Settings>(r#"{"interval_minutes":1.5}"#).is_err());
        assert!(serde_json::from_str::<Settings>(r#"{"enabled":true,"extra":1}"#).is_err());
        for arguments in [
            vec!["--webui-fetch-keybox-auto"],
            vec!["--webui-fetch-keybox-auto", "e30="],
            vec!["--webui-save-keybox-auto", "e30=", "--confirm-replace", "extra"],
            vec!["--webui-get-keybox-auto", "extra"],
        ] {
            assert!(command(arguments.into_iter().map(str::to_string)).unwrap().is_err());
        }
        assert!(command(["unknown".into()].into_iter()).is_none());
    }
}
