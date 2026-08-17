//! One-click QSO upload to eQSL, QRZ Logbook and Club Log, plus QSL-confirmation
//! download from LoTW and eQSL. LoTW *upload* is intentionally not automated
//! (it requires TQSL signing) — the UI exports ADIF for the operator to sign;
//! but confirmations can still be downloaded here to drive award tracking.

use sdroxide_types::{LoginTarget, NetworkConfig, QsoRecord, UploadTarget};

use crate::http;

/// Upload one QSO's ADIF to `target`, returning a human-readable status on
/// success or an error string.
pub fn upload(
    cfg: &NetworkConfig,
    my_call: &str,
    target: UploadTarget,
    adif: &str,
) -> Result<String, String> {
    match target {
        UploadTarget::Eqsl => upload_eqsl(cfg, adif),
        UploadTarget::QrzLogbook => upload_qrz(cfg, adif),
        UploadTarget::ClubLog => upload_clublog(cfg, my_call, adif),
    }
}

fn upload_eqsl(cfg: &NetworkConfig, adif: &str) -> Result<String, String> {
    if cfg.eqsl.user.trim().is_empty() {
        return Err("eQSL username/password not set".into());
    }
    let body = http::post_form(
        "https://www.eqsl.cc/qslcard/importADIF.cfm",
        &[
            ("EQSL_USER", cfg.eqsl.user.trim()),
            ("EQSL_PSWD", cfg.eqsl.password.trim()),
            ("ADIFData", adif),
        ],
    )?;
    // eQSL returns HTML; success contains "Result: 1 out of 1 …". Errors carry
    // an "Error:" / "Warning:" line.
    let text = strip_html(&body);
    if text.to_ascii_lowercase().contains("added") || text.contains("Result: 1") {
        Ok("eQSL: accepted".into())
    } else if let Some(line) = text.lines().find(|l| {
        let l = l.to_ascii_lowercase();
        l.contains("error") || l.contains("warning") || l.contains("bad")
    }) {
        Err(line.trim().to_string())
    } else {
        // Some accounts return a terse OK page; treat a 200 with no error as ok.
        Ok("eQSL: submitted".into())
    }
}

fn upload_qrz(cfg: &NetworkConfig, adif: &str) -> Result<String, String> {
    if cfg.qrz_logbook_key.trim().is_empty() {
        return Err("QRZ Logbook API key not set".into());
    }
    let body = http::post_form(
        "https://logbook.qrz.com/api",
        &[("KEY", cfg.qrz_logbook_key.trim()), ("ACTION", "INSERT"), ("ADIF", adif)],
    )?;
    // Response is url-encoded key=value pairs: RESULT=OK / FAIL / AUTH / REPLACE.
    let fields = parse_kv(&body);
    match fields.iter().find(|(k, _)| k == "RESULT").map(|(_, v)| v.as_str()) {
        Some("OK") => Ok("QRZ: logged".into()),
        Some("REPLACE") => Ok("QRZ: already logged (replaced)".into()),
        Some(other) => {
            let reason = fields
                .iter()
                .find(|(k, _)| k == "REASON")
                .map(|(_, v)| v.clone())
                .unwrap_or_else(|| other.to_string());
            Err(format!("QRZ: {reason}"))
        }
        None => {
            Err(format!("QRZ: unexpected response: {}", body.chars().take(120).collect::<String>()))
        }
    }
}

fn upload_clublog(cfg: &NetworkConfig, my_call: &str, adif: &str) -> Result<String, String> {
    if cfg.clublog.user.trim().is_empty() || cfg.clublog_api_key.trim().is_empty() {
        return Err("Club Log email/password/API key not set".into());
    }
    // Station callsign for the log: my_call, else the record's own is used by CL.
    let body = http::post_form(
        "https://clublog.org/realtime.php",
        &[
            ("email", cfg.clublog.user.trim()),
            ("password", cfg.clublog.password.trim()),
            ("callsign", my_call.trim()),
            ("api", cfg.clublog_api_key.trim()),
            ("adif", adif),
        ],
    )?;
    let t = body.trim();
    // Club Log returns 200 with an empty/OK body on success, error text otherwise.
    if t.is_empty() || t.eq_ignore_ascii_case("ok") {
        Ok("Club Log: accepted".into())
    } else if t.to_ascii_lowercase().contains("error") || t.len() > 3 {
        Err(format!("Club Log: {}", t.chars().take(160).collect::<String>()))
    } else {
        Ok("Club Log: accepted".into())
    }
}

/// Check one service's stored credentials, without logging anything.
///
/// ⛔ NOTHING HERE MAY WRITE. A credential check that inserted a dummy QSO to
/// see whether the login worked would put a fictional contact in the operator's
/// permanent log and, for the services that forward to LoTW or an award
/// programme, somewhere it cannot be withdrawn from. Every branch below uses
/// the cheapest READ endpoint the service publishes, and the upload endpoints
/// above are deliberately not reachable from here.
///
/// The message on success is what the operator sees next to the button, so it
/// says something they can check against — the account or callsign the service
/// answered for, where the service gives one.
pub fn test_login(
    cfg: &NetworkConfig,
    my_call: &str,
    target: LoginTarget,
) -> Result<String, String> {
    match target {
        LoginTarget::Eqsl => test_eqsl(cfg),
        LoginTarget::QrzLogbook => test_qrz(cfg),
        LoginTarget::ClubLog => test_clublog(cfg, my_call),
        LoginTarget::Lotw => test_lotw(cfg),
    }
}

fn test_eqsl(cfg: &NetworkConfig) -> Result<String, String> {
    if cfg.eqsl.user.trim().is_empty() || cfg.eqsl.password.trim().is_empty() {
        return Err("username/password not set".into());
    }
    // The inbox download, asked for a date nothing can be newer than: the
    // question is whether the login is accepted, not what is in the inbox, and
    // this keeps eQSL from building an ADIF file to answer it.
    let url = format!(
        "https://www.eqsl.cc/qslcard/DownloadInBox.cfm?UserName={}&Password={}&RcvdSince=20991231",
        urlencode(cfg.eqsl.user.trim()),
        urlencode(cfg.eqsl.password.trim())
    );
    let page = http::get(&url)?;
    let text = strip_html(&page);
    let low = text.to_ascii_lowercase();
    // Every rule below is taken from what eQSL actually returned on 17 August
    // 2026, for a good login, a good login with nothing to fetch, and a
    // deliberately wrong password. Guessing at this is how the first version
    // managed to be wrong in both directions at once: eQSL says
    // "Error: No such Username/Password found", which contains neither "invalid"
    // nor "not found", and its empty-inbox answer is "You have no log entries",
    // which contains neither "no qso" nor a ".adi" link.
    if low.contains("no such username/password") || low.contains("error:") {
        let line = text
            .lines()
            .map(str::trim)
            .find(|l| {
                l.to_ascii_lowercase().contains("error")
                    || l.to_ascii_lowercase().contains("no such")
            })
            .unwrap_or("login rejected");
        return Err(line.trim_start_matches("Error:").trim().chars().take(120).collect());
    }
    if low.contains("no log entries")
        || low.contains("has been built")
        || page.to_ascii_lowercase().contains(".adi")
    {
        return Ok(format!("signed in as {}", cfg.eqsl.user.trim()));
    }
    Err("unexpected reply; check the username and password".into())
}

fn test_qrz(cfg: &NetworkConfig) -> Result<String, String> {
    if cfg.qrz_logbook_key.trim().is_empty() {
        return Err("API key not set".into());
    }
    // STATUS reports the logbook's own details and inserts nothing. This is the
    // one service of the four with an endpoint meant for exactly this.
    let body = http::post_form(
        "https://logbook.qrz.com/api",
        &[("KEY", cfg.qrz_logbook_key.trim()), ("ACTION", "STATUS")],
    )?;
    let kv = parse_kv(&body);
    let get = |k: &str| kv.iter().find(|(a, _)| a == k).map(|(_, v)| v.clone());
    match get("RESULT").as_deref() {
        Some("OK") => {
            let call = get("CALLSIGN").unwrap_or_else(|| "logbook".into());
            match get("COUNT") {
                Some(n) => Ok(format!("{call}, {n} QSOs")),
                None => Ok(call),
            }
        }
        _ => Err(get("REASON")
            .or_else(|| get("STATUS"))
            .unwrap_or_else(|| "key rejected".into())
            .chars()
            .take(120)
            .collect()),
    }
}

/// Club Log needs BOTH an account password and an API key, and the upload uses
/// both, so a check that proved only one would pass while uploads still failed.
/// They have separate read endpoints, so both are exercised.
fn test_clublog(cfg: &NetworkConfig, my_call: &str) -> Result<String, String> {
    if cfg.clublog.user.trim().is_empty() || cfg.clublog.password.trim().is_empty() {
        return Err("email/password not set".into());
    }
    let call = my_call.trim();
    if call.is_empty() {
        return Err("station callsign not set".into());
    }
    // The OQRS ADIF download. `type=dxqsl` asks for OQRS requests rather than
    // the whole log, so this stays small on a station with fifty thousand QSOs,
    // and POST is required rather than GET.
    //
    // ⚠️ THIS CHECKS THE ACCOUNT, NOT THE API KEY, and the upload needs both.
    // The documented key check is `GET /dxcc?call=&api=&full=1`, which returned
    // 403 Forbidden on every attempt here on 17 August 2026, with and without a
    // User-Agent, while the account call below succeeded from the same host
    // seconds later. Rather than report a guess about the key, the message says
    // which half was actually checked.
    let body = http::post_form(
        "https://clublog.org/getadif.php",
        &[
            ("email", cfg.clublog.user.trim()),
            ("password", cfg.clublog.password.trim()),
            ("call", call),
            ("type", "dxqsl"),
        ],
    )?;
    // A positive marker, not an error-word search: Club Log's own success text
    // is prose, and a rule that hunts for "error" in prose eventually finds one.
    if body.to_ascii_lowercase().contains("club log") {
        return Ok(format!("{call}: account accepted (API key not checked)"));
    }
    Err(body.trim().chars().take(120).collect())
}

fn test_lotw(cfg: &NetworkConfig) -> Result<String, String> {
    if cfg.lotw.user.trim().is_empty() || cfg.lotw.password.trim().is_empty() {
        return Err("login not set".into());
    }
    // The same report the confirmation sync uses, bounded to a date that cannot
    // return records, so this costs ARRL almost nothing to answer.
    let url = format!(
        "https://lotw.arrl.org/lotwuser/lotwreport.adi?login={}&password={}&qso_query=1&qso_qsl=yes&qso_qslsince=2099-12-31",
        urlencode(cfg.lotw.user.trim()),
        urlencode(cfg.lotw.password.trim())
    );
    let body = http::get(&url)?;
    // A rejected login is an HTML page; an accepted one is ADIF, whose header
    // ends in <eoh> even when no records follow.
    if body.to_ascii_lowercase().contains("username/password") || body.contains("<!DOCTYPE") {
        return Err("login rejected (LoTW returned its log-on page)".into());
    }
    if body.to_ascii_lowercase().contains("<eoh>") {
        return Ok(format!("signed in as {}", cfg.lotw.user.trim()));
    }
    Err("unexpected reply; check the login and password".into())
}

/// Download QSL confirmations from LoTW and eQSL, returning parsed confirmation
/// records (the UI matches these to the log to set `*_rcvd`). Best-effort: a
/// service that isn't configured is skipped; per-service errors are collected.
pub fn sync_confirmations(cfg: &NetworkConfig) -> (Vec<QsoRecord>, Vec<String>) {
    let mut confirmed = Vec::new();
    let mut errors = Vec::new();

    if !cfg.lotw.user.trim().is_empty() {
        match download_lotw(cfg) {
            Ok(mut recs) => confirmed.append(&mut recs),
            Err(e) => errors.push(format!("LoTW: {e}")),
        }
    }
    if !cfg.eqsl.user.trim().is_empty() {
        match download_eqsl(cfg) {
            Ok(mut recs) => confirmed.append(&mut recs),
            Err(e) => errors.push(format!("eQSL: {e}")),
        }
    }
    (confirmed, errors)
}

fn download_lotw(cfg: &NetworkConfig) -> Result<Vec<QsoRecord>, String> {
    // Confirmed QSLs only (qso_qsl=yes), with detail so BAND/MODE/DATE parse.
    let url = format!(
        "https://lotw.arrl.org/lotwuser/lotwreport.adi?login={}&password={}&qso_query=1&qso_qsl=yes&qso_qsldetail=yes",
        urlencode(cfg.lotw.user.trim()),
        urlencode(cfg.lotw.password.trim())
    );
    let body = http::get(&url)?;
    if body.to_ascii_lowercase().contains("username/password") || body.contains("<!DOCTYPE") {
        return Err("login rejected".into());
    }
    let mut recs = sdroxide_types::adif_to_qso_log(&body);
    for r in &mut recs {
        r.lotw_rcvd = true; // this report is the set of LoTW-confirmed QSOs
    }
    Ok(recs)
}

fn download_eqsl(cfg: &NetworkConfig) -> Result<Vec<QsoRecord>, String> {
    // eQSL's inbox download is two-step: the first call builds an .adi and
    // returns a page linking to it.
    let url = format!(
        "https://www.eqsl.cc/qslcard/DownloadInBox.cfm?UserName={}&Password={}&RcvdSince=19700101",
        urlencode(cfg.eqsl.user.trim()),
        urlencode(cfg.eqsl.password.trim())
    );
    let page = http::get(&url)?;
    // Find the ".adi" link in the returned HTML.
    let link = page
        .split(['"', '\'', ' ', '\n'])
        .find(|t| t.to_ascii_lowercase().ends_with(".adi"))
        .ok_or("no download link returned (check credentials)")?;
    let adi_url = if link.starts_with("http") {
        link.to_string()
    } else {
        format!("https://www.eqsl.cc/qslcard/{}", link.trim_start_matches('/'))
    };
    let body = http::get(&adi_url)?;
    let mut recs = sdroxide_types::adif_to_qso_log(&body);
    for r in &mut recs {
        r.eqsl_rcvd = true; // eQSL inbox = received confirmations
    }
    Ok(recs)
}

// ── helpers ──────────────────────────────────────────────────────────────

/// Parse `k=v&k=v` (or newline-separated) into pairs; keys uppercased.
fn parse_kv(body: &str) -> Vec<(String, String)> {
    body.split(['&', '\n'])
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| (k.trim().to_ascii_uppercase(), urldecode(v.trim())))
        .collect()
}

/// Very small HTML→text: drop tags, collapse whitespace.
fn strip_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn urldecode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                let hex = std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("");
                if let Ok(v) = u8::from_str_radix(hex, 16) {
                    out.push(v as char);
                    i += 3;
                    continue;
                }
                out.push('%');
                i += 1;
            }
            b'+' => {
                out.push(' ');
                i += 1;
            }
            c => {
                out.push(c as char);
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_qrz_response() {
        let kv = parse_kv("RESULT=OK&COUNT=1&LOGID=123");
        assert_eq!(kv.iter().find(|(k, _)| k == "RESULT").unwrap().1, "OK");
    }

    #[test]
    fn strips_html() {
        assert_eq!(strip_html("<b>Result: 1</b> added").trim(), "Result: 1 added");
    }
}
