use serde_json::Value;

const REDACTED: &str = "[REDACTED]";
#[cfg(test)]
const UPSTREAM_BODY_LIMIT: usize = 4 * 1024;

pub(super) fn redact_json_line(line: &[u8]) -> Vec<u8> {
    match serde_json::from_slice::<Value>(line) {
        Ok(mut value) => {
            redact_diagnostic_value(&mut value);
            let mut output = serde_json::to_vec(&value).unwrap_or_else(|_| b"{}".to_vec());
            output.push(b'\n');
            output
        }
        Err(_) => format!(
            "{{\"message\":{}}}\n",
            json_string(&redact_text(&String::from_utf8_lossy(line)))
        )
        .into_bytes(),
    }
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| format!("\"{REDACTED}\""))
}

pub fn redact_value(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if is_secret_key(key) {
                    *value = Value::String(REDACTED.to_owned());
                } else {
                    redact_value(value);
                }
            }
        }
        Value::Array(values) => values.iter_mut().for_each(redact_value),
        Value::String(text) => *text = redact_text(text),
        _ => {}
    }
}

pub fn redact_diagnostic_value(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if is_secret_key(key) || is_local_path_key(key) {
                    *value = Value::String(REDACTED.to_owned());
                } else if normalized_key(key) == "values" {
                    redact_listing_values(value);
                } else if normalized_key(key) == "attributes" {
                    redact_listing_value_container(value);
                } else if is_listing_value_key(key) {
                    *value = Value::String(REDACTED.to_owned());
                } else {
                    redact_diagnostic_value(value);
                }
            }
        }
        Value::Array(values) => values.iter_mut().for_each(redact_diagnostic_value),
        Value::String(text) => *text = redact_diagnostic_text(text),
        _ => {}
    }
}

fn redact_listing_values(value: &mut Value) {
    let Value::Object(values) = value else {
        *value = Value::String(REDACTED.to_owned());
        return;
    };
    for (key, value) in values {
        let normalized = normalized_key(key);
        if matches!(
            normalized.as_str(),
            "category" | "price" | "tradetype" | "delivery" | "revision" | "image" | "multiimage"
        ) {
            redact_value(value);
        } else {
            *value = Value::String(REDACTED.to_owned());
        }
    }
}

fn redact_listing_value_container(value: &mut Value) {
    match value {
        Value::Object(values) => {
            for value in values.values_mut() {
                *value = Value::String(REDACTED.to_owned());
            }
        }
        _ => *value = Value::String(REDACTED.to_owned()),
    }
}

fn normalized_key(key: &str) -> String {
    key.chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn is_local_path_key(key: &str) -> bool {
    matches!(
        normalized_key(key).as_str(),
        "logpath"
            | "imagepath"
            | "imagepaths"
            | "descriptionfile"
            | "inputpath"
            | "filepath"
            | "filename"
    )
}

fn is_listing_value_key(key: &str) -> bool {
    matches!(
        normalized_key(key).as_str(),
        "title" | "description" | "postalcode" | "safetext"
    )
}

fn is_secret_key(key: &str) -> bool {
    let normalized = normalized_key(key);
    normalized.ends_with("authorization")
        || normalized.ends_with("token")
        || normalized.ends_with("cookie")
        || normalized.ends_with("signature")
        || matches!(
            normalized.as_str(),
            "bearer"
                | "hmac"
                | "oauthcode"
                | "authorizationcode"
                | "spidcode"
                | "callbackurl"
                | "loginurl"
                | "authorizationurl"
                | "redirecturi"
                | "pkce"
                | "pkceverifier"
                | "pkcechallenge"
                | "codeverifier"
                | "codechallenge"
                | "rawimage"
                | "imagedata"
                | "imagebytes"
        )
}

pub fn redact_diagnostic_text(text: &str) -> String {
    redact_text(text)
        .split_inclusive(char::is_whitespace)
        .map(|part| {
            let token = part.trim_matches(|character: char| {
                character.is_whitespace()
                    || matches!(character, '`' | '\'' | '"' | '(' | ')' | ',' | ';' | ':')
            });
            if looks_like_local_path(token) {
                let whitespace = part
                    .chars()
                    .rev()
                    .take_while(|character| character.is_whitespace())
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect::<String>();
                format!("{REDACTED}{whitespace}")
            } else {
                part.to_owned()
            }
        })
        .collect()
}

fn looks_like_local_path(token: &str) -> bool {
    if token.is_empty() || token.contains("://") {
        return false;
    }
    let lower = token.to_ascii_lowercase();
    let has_file_extension = [".json", ".txt", ".jpg", ".jpeg", ".png", ".heic", ".webp"]
        .iter()
        .any(|extension| lower.ends_with(extension));
    token.starts_with("./")
        || token.starts_with("../")
        || token.starts_with("~/")
        || ["/users/", "/home/", "/tmp/", "/private/", "/var/folders/"]
            .iter()
            .any(|prefix| lower.starts_with(prefix))
        || has_file_extension
}

pub fn redact_text(text: &str) -> String {
    let mut redacted = redact_callback_urls(text);
    for marker in ["Bearer ", "Basic "] {
        redacted = redact_after_marker(&redacted, marker);
    }
    for key in [
        "access_token",
        "refresh_token",
        "id_token",
        "authorization",
        "cookie",
        "set-cookie",
        "signature",
        "hmac",
        "oauth_code",
        "authorization_code",
        "code_verifier",
        "code_challenge",
        "pkce_verifier",
        "pkce_challenge",
        "callback_url",
        "login_url",
        "authorization_url",
        "redirect_uri",
    ] {
        redacted = redact_assignment(&redacted, key);
    }
    redacted = redact_data_images(&redacted);
    redacted
}

fn redact_callback_urls(text: &str) -> String {
    text.split_inclusive(char::is_whitespace)
        .map(|part| {
            let lower = part.to_ascii_lowercase();
            if lower.contains("://")
                && (lower.contains("code=")
                    || lower.contains("/callback")
                    || lower.contains("oauth"))
            {
                let whitespace = part
                    .chars()
                    .rev()
                    .take_while(|character| character.is_whitespace())
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect::<String>();
                format!("{REDACTED}{whitespace}")
            } else {
                part.to_owned()
            }
        })
        .collect()
}

fn redact_after_marker(text: &str, marker: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut remainder = text;
    while let Some(index) = remainder
        .to_ascii_lowercase()
        .find(&marker.to_ascii_lowercase())
    {
        let value_start = index + marker.len();
        result.push_str(&remainder[..value_start]);
        result.push_str(REDACTED);
        let end = remainder[value_start..]
            .find(|character: char| {
                character.is_whitespace() || matches!(character, ',' | ';' | '"')
            })
            .map_or(remainder.len(), |offset| value_start + offset);
        remainder = &remainder[end..];
    }
    result.push_str(remainder);
    result
}

fn redact_assignment(text: &str, key: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut remainder = text;
    let lower_key = key.to_ascii_lowercase();
    loop {
        let lower = remainder.to_ascii_lowercase();
        let Some(index) = lower.find(&lower_key) else {
            result.push_str(remainder);
            break;
        };
        result.push_str(&remainder[..index + key.len()]);
        let after_key = &remainder[index + key.len()..];
        let separator_len = after_key
            .char_indices()
            .take_while(|(_, character)| {
                character.is_whitespace() || matches!(character, '=' | ':' | '"')
            })
            .last()
            .map_or(0, |(index, character)| index + character.len_utf8());
        if separator_len == 0 {
            remainder = after_key;
            continue;
        }
        result.push_str(&after_key[..separator_len]);
        result.push_str(REDACTED);
        let value = &after_key[separator_len..];
        let end = value
            .find(|character: char| {
                character.is_whitespace() || matches!(character, '&' | ',' | ';' | '"')
            })
            .unwrap_or(value.len());
        remainder = &value[end..];
    }
    result
}

fn redact_data_images(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut remainder = text;
    while let Some(index) = remainder.to_ascii_lowercase().find("data:image/") {
        result.push_str(&remainder[..index]);
        result.push_str(REDACTED);
        let tail = &remainder[index..];
        let end = tail
            .find(|character: char| character.is_whitespace() || matches!(character, '"' | '\''))
            .unwrap_or(tail.len());
        remainder = &tail[end..];
    }
    result.push_str(remainder);
    result
}

#[cfg(test)]
pub fn sanitized_upstream_body(body: &[u8]) -> String {
    let mut sanitized = match serde_json::from_slice::<Value>(body) {
        Ok(mut value) => {
            redact_diagnostic_value(&mut value);
            serde_json::to_string(&value).unwrap_or_else(|_| REDACTED.to_owned())
        }
        Err(_) => match std::str::from_utf8(body) {
            Ok(text) => redact_diagnostic_text(text),
            Err(_) => "[REDACTED_BINARY_BODY]".to_owned(),
        },
    };
    if sanitized.len() > UPSTREAM_BODY_LIMIT {
        let mut boundary = UPSTREAM_BODY_LIMIT;
        while !sanitized.is_char_boundary(boundary) {
            boundary -= 1;
        }
        sanitized.truncate(boundary);
        sanitized.push_str("...[truncated]");
    }
    sanitized
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn redacts_every_secret_class() {
        let mut value = json!({
            "authorization": "Bearer secret-auth-value",
            "session_token": "secret-token-value",
            "cookie": "secret-cookie-value",
            "hmac_signature": "secret-signature-value",
            "oauth_code": "secret-code-value",
            "callback_url": "flea://secret-callback-value",
            "login_url": "https://login.vend.fi/oauth/authorize?state=secret-login-state",
            "code_verifier": "secret-verifier-value",
            "raw_image": "secret-image-value",
            "message": "Bearer secret-text-auth access_token=secret-text-token flea://oauth/callback?code=secret-callback-code data:image/png;base64,secret-image-data"
        });
        redact_value(&mut value);
        let encoded = value.to_string();
        for secret in [
            "secret-auth-value",
            "secret-token-value",
            "secret-cookie-value",
            "secret-signature-value",
            "secret-code-value",
            "secret-callback-value",
            "secret-login-state",
            "secret-verifier-value",
            "secret-image-value",
            "secret-text-auth",
            "secret-text-token",
            "secret-callback-code",
            "secret-image-data",
        ] {
            assert!(!encoded.contains(secret), "secret leaked: {secret}");
        }
    }

    #[test]
    fn diagnostic_redaction_hides_listing_values_and_local_paths_only_in_logs() {
        let public = json!({
            "values": {
                "title": "Safe listing title",
                "description": "private description",
                "category": "furniture/chairs",
                "dynamic_field": "private dynamic value"
            },
            "http": {
                "path": "/adinput/ad/recommerce/draft-1/update",
                "content_type": "application/json"
            },
            "image_paths": ["/private/photos/chair.jpg"],
            "message": "failed to read ./private/chair.json"
        });
        let mut diagnostic = public.clone();

        redact_diagnostic_value(&mut diagnostic);

        let diagnostic = diagnostic.to_string();
        assert!(!diagnostic.contains("Safe listing title"));
        assert!(!diagnostic.contains("private description"));
        assert!(!diagnostic.contains("private dynamic value"));
        assert!(!diagnostic.contains("/private/photos/chair.jpg"));
        assert!(!diagnostic.contains("./private/chair.json"));
        assert!(diagnostic.contains("furniture/chairs"));
        assert!(diagnostic.contains("/adinput/ad/recommerce/draft-1/update"));
        assert!(diagnostic.contains("application/json"));
        assert_eq!(public["values"]["title"], "Safe listing title");
    }

    #[test]
    fn upstream_body_is_bounded_and_redacted() {
        let body = format!(
            "access_token=secret {}",
            "x".repeat(UPSTREAM_BODY_LIMIT * 2)
        );
        let sanitized = sanitized_upstream_body(body.as_bytes());
        assert!(!sanitized.contains("secret"));
        assert!(sanitized.len() <= UPSTREAM_BODY_LIMIT + "...[truncated]".len());
        assert!(sanitized.ends_with("...[truncated]"));
        assert_eq!(
            sanitized_upstream_body(&[0xff, 0xd8, 0xff, 0x00]),
            "[REDACTED_BINARY_BODY]"
        );
    }
}
