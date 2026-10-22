#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Protocol {
    Standard,
    VsCode,
}

fn current_protocol() -> Protocol {
    protocol_for(std::env::var("TERM_PROGRAM").ok().as_deref())
}

fn protocol_for(term_program: Option<&str>) -> Protocol {
    if term_program == Some("vscode") {
        Protocol::VsCode
    } else {
        Protocol::Standard
    }
}

pub(crate) fn fresh_prompt(cwd: Option<&str>, hostname: Option<&str>) -> Vec<u8> {
    fresh_prompt_for(current_protocol(), cwd, hostname)
}

fn fresh_prompt_for(protocol: Protocol, cwd: Option<&str>, hostname: Option<&str>) -> Vec<u8> {
    match protocol {
        Protocol::VsCode => {
            let mut value = b"\x1b]633;A\x1b\\".to_vec();
            if let Some(cwd) = cwd {
                extend_osc633_property(&mut value, "Cwd", cwd);
            }
            extend_osc633_property(&mut value, "HasRichCommandDetection", "True");
            value
        }
        Protocol::Standard => {
            let mut value = b"\x1b]133;A\x1b\\".to_vec();
            if let (Some(cwd), Some(hostname)) = (cwd, hostname) {
                let hostname = encode_file_uri_component(hostname, false);
                let cwd = encode_file_uri_component(cwd, true);
                value.extend_from_slice(format!("\x1b]7;file://{hostname}{cwd}\x1b\\").as_bytes());
            }
            value
        }
    }
}

/// Encode literal UTF-8 data, preserving only unreserved URI bytes and path
/// separators. Authority delimiters must never be supplied by the hostname.
fn encode_file_uri_component(value: &str, path: bool) -> String {
    use std::fmt::Write;

    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'.' | b'_' | b'~')
            || (path && byte == b'/')
        {
            encoded.push(char::from(byte));
        } else {
            write!(encoded, "%{byte:02X}").expect("write to String");
        }
    }
    encoded
}

pub(crate) fn prompt_end() -> &'static [u8] {
    prompt_end_for(current_protocol())
}

fn prompt_end_for(protocol: Protocol) -> &'static [u8] {
    match protocol {
        Protocol::VsCode => b"\x1b]633;B\x1b\\",
        Protocol::Standard => b"\x1b]133;B\x1b\\",
    }
}

pub(crate) fn command_output_start(command: &str) -> Vec<u8> {
    command_output_start_for(
        current_protocol(),
        command,
        std::env::var("VSCODE_SHELL_INTEGRATION_NONCE")
            .ok()
            .as_deref(),
    )
}

fn command_output_start_for(protocol: Protocol, command: &str, nonce: Option<&str>) -> Vec<u8> {
    match protocol {
        Protocol::VsCode => {
            let command = escape_osc633(command);
            let nonce = nonce.map(escape_osc633);
            let e = match nonce {
                Some(nonce) => format!("\x1b]633;E;{command};{nonce}\x1b\\"),
                None => format!("\x1b]633;E;{command}\x1b\\"),
            };
            format!("{e}\x1b]633;C\x1b\\").into_bytes()
        }
        Protocol::Standard => b"\x1b]133;C\x1b\\".to_vec(),
    }
}

pub(crate) fn command_finished(exit_code: i32) -> Vec<u8> {
    command_finished_for(current_protocol(), exit_code)
}

fn command_finished_for(protocol: Protocol, exit_code: i32) -> Vec<u8> {
    match protocol {
        Protocol::VsCode => format!("\x1b]633;D;{exit_code}\x1b\\").into_bytes(),
        Protocol::Standard => format!("\x1b]133;D;{exit_code}\x1b\\").into_bytes(),
    }
}

fn extend_osc633_property(output: &mut Vec<u8>, name: &str, value: &str) {
    output
        .extend_from_slice(format!("\x1b]633;P;{name}={}\x1b\\", escape_osc633(value)).as_bytes());
}

fn escape_osc633(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_control() || ch == ' ' || matches!(ch, '\\' | ';') {
            for byte in ch.to_string().bytes() {
                escaped.push_str(&format!("\\x{byte:02X}"));
            }
        } else {
            escaped.push(ch);
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vscode_markers_form_a_complete_command_lifecycle() {
        let mut bytes = fresh_prompt_for(Protocol::VsCode, Some("/tmp/a b"), None);
        bytes.extend_from_slice(prompt_end_for(Protocol::VsCode));
        bytes.extend(command_output_start_for(
            Protocol::VsCode,
            "printf 'a;b'",
            Some("nonce"),
        ));
        bytes.extend(command_finished_for(Protocol::VsCode, 7));
        let output = String::from_utf8(bytes).unwrap();
        assert!(output.contains("\x1b]633;A\x1b\\"));
        assert!(output.contains("Cwd=/tmp/a\\x20b"));
        assert!(output.contains("HasRichCommandDetection=True"));
        assert!(output.contains("\x1b]633;E;printf\\x20'a\\x3Bb';nonce\x1b\\"));
        assert!(output.contains("\x1b]633;C\x1b\\"));
        assert!(output.ends_with("\x1b]633;D;7\x1b\\"));
    }

    #[test]
    fn other_terminals_keep_osc133_and_osc7() {
        let output = String::from_utf8(fresh_prompt_for(
            Protocol::Standard,
            Some("/tmp"),
            Some("host"),
        ))
        .unwrap();
        assert_eq!(output, "\x1b]133;A\x1b\\\x1b]7;file://host/tmp\x1b\\");
        assert_eq!(
            command_finished_for(Protocol::Standard, 130),
            b"\x1b]133;D;130\x1b\\"
        );
    }

    #[test]
    fn osc633_escaping_preserves_unicode_and_escapes_delimiters() {
        assert_eq!(escape_osc633("日本 \\ ;"), "日本\\x20\\x5C\\x20\\x3B");
    }
}

#[cfg(test)]
mod cwd_safety_tests {
    use super::*;

    fn decode(value: &str) -> Vec<u8> {
        let mut decoded = Vec::new();
        let mut bytes = value.bytes();
        while let Some(byte) = bytes.next() {
            decoded.push(if byte == b'%' {
                let hex = [bytes.next().unwrap(), bytes.next().unwrap()];
                u8::from_str_radix(std::str::from_utf8(&hex).unwrap(), 16).unwrap()
            } else {
                byte
            });
        }
        decoded
    }

    #[test]
    fn cwd_safety_osc7_encodes_controls_and_uri_delimiters() {
        let cwd = "/tmp/a b%#?日本/\x07\x1b\\\r\n\u{85}\u{9c}\x1b]52;c;payload\x07";
        let host = "host\x07\x1b\\\r\n\u{9c}/?#%@日本";
        let output = fresh_prompt_for(Protocol::Standard, Some(cwd), Some(host));
        let prefix = b"\x1b]133;A\x1b\\\x1b]7;file://";
        assert!(output.starts_with(prefix));
        assert!(output.ends_with(b"\x1b\\"));
        let uri = &output[prefix.len()..output.len() - 2];
        assert!(
            uri.iter().all(|b| b.is_ascii_graphic()),
            "OSC7 payload must be printable ASCII"
        );
        let uri = std::str::from_utf8(uri).unwrap();
        let (authority, path) = uri.split_once('/').unwrap();
        assert!(!authority.contains(['@', ':', '?', '#', '\\']));
        assert_eq!(decode(authority), host.as_bytes());
        assert_eq!(decode(&format!("/{path}")), cwd.as_bytes());
        assert!(path.contains("a%20b%25%23%3F%E6%97%A5%E6%9C%AC/"));
        assert_eq!(output.iter().filter(|&&b| b == 0x1b).count(), 4);
    }

    #[test]
    fn cwd_safety_osc7_encodes_every_control() {
        let controls: String = (0..=0x9f)
            .filter_map(char::from_u32)
            .filter(|c| c.is_control())
            .collect();
        let cwd = format!("/{controls}");
        let bytes = fresh_prompt_for(Protocol::Standard, Some(&cwd), Some("host"));
        let payload = &bytes[b"\x1b]133;A\x1b\\\x1b]7;file://host".len()..bytes.len() - 2];
        assert!(payload.iter().all(|b| b.is_ascii_graphic()));
        assert_eq!(
            decode(std::str::from_utf8(payload).unwrap()),
            cwd.as_bytes()
        );
    }
}
